#!/usr/bin/env python3
"""Check the installed CLI's secure server start, health, and restart path.

Requires only Python 3.10+ and its standard library.
"""

from __future__ import annotations

import argparse
import http.client
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys
import tempfile
import time


def stop_server(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def start_server(
    cli: Path, server_dir: Path, ready_file: Path, log_file: Path
) -> subprocess.Popen[bytes]:
    env = os.environ.copy()
    env["PLASMITE_SERVE_READY_FILE"] = str(ready_file)
    with log_file.open("wb") as log:
        process = subprocess.Popen(
            [
                str(cli),
                "--dir",
                str(server_dir),
                "serve",
                "--bind",
                "127.0.0.1:0",
                "--remote-bind",
                "127.0.0.1:0",
            ],
            cwd=server_dir.parent,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
        )

    try:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if process.poll() is not None:
                break
            if ready_file.is_file():
                return process
            time.sleep(0.05)
        details = log_file.read_text(encoding="utf-8", errors="replace").strip()
        raise RuntimeError(details or "server did not publish its HTTPS listener address")
    except BaseException:
        stop_server(process)
        raise


def check_health(server_dir: Path, ready_file: Path) -> None:
    address = ready_file.read_text(encoding="utf-8").strip()
    port = int(address.rsplit(":", 1)[1])
    state_dir = server_dir / ".plasmite-serve"
    identity = json.loads((state_dir / "identity.json").read_text(encoding="utf-8"))
    certificate = state_dir / identity["cert_file"]
    context = ssl.create_default_context(cafile=str(certificate))
    deadline = time.monotonic() + 5
    while True:
        connection = http.client.HTTPSConnection("localhost", port, context=context, timeout=2)
        try:
            connection.request("GET", "/healthz")
            response = connection.getresponse()
            body = json.loads(response.read())
            if response.status != 200 or body != {"ok": True}:
                raise RuntimeError("HTTPS health check returned an unexpected response")
            return
        except ConnectionError:
            if time.monotonic() >= deadline:
                raise RuntimeError("HTTPS health check did not respond")
            time.sleep(0.05)
        finally:
            connection.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    args = parser.parse_args()
    if sys.version_info < (3, 10):
        parser.error("Python 3.10 or newer is required")
    cli = args.cli.resolve()
    scratch = args.scratch.resolve()
    if not cli.is_file():
        parser.error(f"installed CLI binary not found: {cli}")
    scratch.mkdir(parents=True, exist_ok=True)

    try:
        with tempfile.TemporaryDirectory(prefix="packaged-serve-", dir=scratch) as temporary:
            root = Path(temporary)
            server_dir = root / "server"
            server_dir.mkdir()
            for attempt in range(2):
                ready_file = root / f"ready-{attempt}"
                process = start_server(
                    cli, server_dir, ready_file, root / f"server-{attempt}.log"
                )
                try:
                    check_health(server_dir, ready_file)
                finally:
                    stop_server(process)
        print("[smoke] packaged secure server start + restart ok")
    except (
        OSError,
        ValueError,
        KeyError,
        json.JSONDecodeError,
        http.client.HTTPException,
        RuntimeError,
    ) as error:
        print(f"[smoke] packaged secure server failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
