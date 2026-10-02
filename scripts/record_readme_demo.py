#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pillow"]
# ///
"""Record the README's local IPC example through a real shell and PTY.

Use a released 1.0.0 CLI on PATH. Run with uv and a monospace font:
  uv run scripts/record_readme_demo.py --font /System/Library/Fonts/Menlo.ttc

The cast contains the actual terminal bytes and timings. Pillow renders those
bytes into the GIF and final still; no application output is synthesized.
"""

import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import time
import fcntl
import termios

from PIL import Image, ImageDraw, ImageFont


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--font", required=True, help="Path to a monospace TTF/TTC")
    args = parser.parse_args()
    binary = shutil.which("pls")
    if not binary:
        parser.error("install the released Plasmite 1.0.0 CLI first")
    version = subprocess.check_output([binary, "version"], text=True).strip()
    if version != "plasmite 1.0.0":
        parser.error(f"expected released plasmite 1.0.0, found {version}")

    output = Path(__file__).resolve().parents[1] / "docs/images/ipc"
    output.mkdir(parents=True, exist_ok=True)
    events = []
    started = time.monotonic()

    with tempfile.TemporaryDirectory(prefix="plasmite-readme-demo-") as scratch:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 16, 84, 0, 0))
        env = dict(os.environ, PS1="$ ", PS2="> ", TERM="dumb", LC_ALL="C")

        def child_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        shell = subprocess.Popen(
            ["/bin/sh", "-i"], cwd=scratch, env=env,
            stdin=slave, stdout=slave, stderr=slave, preexec_fn=child_terminal,
        )
        os.close(slave)

        def collect(seconds):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                ready, _, _ = select.select([master], [], [], max(0, until - time.monotonic()))
                if ready:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            return
                        raise
                    if not data:
                        return
                    events.append([round(time.monotonic() - started, 4), "o", data.decode("utf-8")])

        def enter(command, pause=1.0):
            for char in command:
                os.write(master, char.encode())
                collect(0.018)
            os.write(master, b"\n")
            collect(pause)

        try:
            collect(0.25)
            enter("pls version", 0.6)
            enter("# Alice creates a channel (aka a pool)", 0.4)
            enter("pls --dir ./pools pool create channel", 0.8)
            enter("echo '{\"from\":\"A\",\"msg\":\"hello world\"}' | pls --dir ./pools feed channel", 0.8)
            enter("# Alice's writer has exited. Bob starts watching.", 0.8)
            enter("pls --dir ./pools follow channel --tail 1 --json --data-only", 2.5)
            # Interrupt only this recording's foreground command, then restore the prompt.
            os.write(master, b"\x03")
            collect(0.3)
        finally:
            os.write(master, b"exit\n")
            try:
                shell.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(os.tcgetpgrp(master), signal.SIGKILL)
                shell.kill()
                shell.wait(timeout=5)
            os.close(master)

        fetched = json.loads(subprocess.check_output(
            [binary, "--dir", "./pools", "fetch", "channel", "1", "--json"],
            cwd=scratch, text=True,
        ))
        if fetched["seq"] != 1 or fetched["data"] != {"from": "A", "msg": "hello world"}:
            raise RuntimeError("the pool did not retain the expected message")

    transcript = "".join(event[2] for event in events)
    if '{"from":"A","msg":"hello world"}\r\n' not in transcript:
        raise RuntimeError("the late reader did not return the expected message")

    header = {
        "version": 2, "width": 84, "height": 16, "timestamp": int(time.time()),
        "title": "Local IPC with Plasmite 1.0.0",
        "plasmite_version": version,
        "plasmite_sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
    }
    (output / "local-ipc.cast").write_text(
        "\n".join(json.dumps(row) for row in [header, *events]) + "\n"
    )

    font = ImageFont.truetype(args.font, 16)
    cell = font.getlength("M")
    width, height = 840, 370
    lines, row, col = [""], 0, 0

    def terminal_write(text):
        nonlocal row, col
        for char in text:
            if char == "\r":
                col = 0
            elif char == "\n":
                row += 1
                col = 0
                if row >= len(lines):
                    lines.append("")
            elif char == "\b":
                col = max(0, col - 1)
            elif char >= " ":
                line = lines[row].ljust(col)
                lines[row] = line[:col] + char + line[col + 1:]
                col += 1

    def frame():
        im = Image.new("RGB", (width, height), "#0f181e")
        draw = ImageDraw.Draw(im)
        for index, line in enumerate(lines[-16:]):
            color = "#64a6a8" if line.startswith("$ #") else "#e7ead7"
            draw.text((18, 16 + index * 21), line, font=font, fill=color)
        visible_row = min(row, 15)
        draw.rectangle((18 + col * cell, 17 + visible_row * 21,
                        19 + col * cell, 33 + visible_row * 21), fill="#64a6a8")
        return im

    frames, index = [], 0
    duration = events[-1][0] + 2.0
    for tick in range(int(duration * 10) + 1):
        at = tick / 10
        while index < len(events) and events[index][0] <= at:
            terminal_write(events[index][2])
            index += 1
        frames.append(frame())
    frames[-1].save(output / "local-ipc.png")
    frames[0].save(
        output / "local-ipc.gif", save_all=True, append_images=frames[1:],
        duration=100, loop=0, optimize=True,
    )
    print(f"Recorded {version}: {duration:.1f}s; {len(events)} terminal events")
    print(transcript)


if __name__ == "__main__":
    main()
