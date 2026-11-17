#!/usr/bin/env python3
"""Isolate stored frame layout from concurrent producer/consumer scheduling."""

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path
from tempfile import TemporaryDirectory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--messages", type=int, default=5000)
    parser.add_argument("--repeats", type=int, default=5)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[3]
    bindings = root / "bindings/python"
    sys.path[:0] = [str(bindings), str(root / "scripts")]
    os.environ["PLASMITE_LIB_DIR"] = str(args.library.resolve())
    os.environ["PLASMITE_BENCH_NOTE_BYTES"] = "0"
    os.environ.pop("PLASMITE_BENCH_EXTRA_FIELDS", None)
    import plasmite
    from bench_pipe_comparison import record, update

    driver = root / "scripts/bench_pipe_comparison.py"
    result = {
        "python": sys.version,
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "driver_sha256": hashlib.sha256(driver.read_bytes()).hexdigest(),
        "library_sha256": hashlib.sha256(
            Path(plasmite._LIB._name).read_bytes()
        ).hexdigest(),
        "messages": args.messages,
        "runs": [],
        "notes": [
            "All append work finishes before reader process starts.",
            "Reader reports its own elapsed/CPU after imports and start token; source pool pages are warm.",
        ],
    }
    for repeat in range(args.repeats):
        order = (
            ["native-view", "identity"]
            if repeat % 2 == 0
            else ["identity", "native-view"]
        )
        for variant in order:
            with (
                TemporaryDirectory() as directory,
                plasmite.Client(directory) as client,
            ):
                with client.create_pool(
                    "events", max(16777216, args.messages * 8192)
                ) as pool:
                    value = record()
                    for index in range(args.messages):
                        update(value, index)
                        if variant == "native-view":
                            pool.feed_view(value)
                        else:
                            pool.append_identity(value)
                command = [
                    sys.executable,
                    str(driver),
                    "--worker",
                    "--lane",
                    "plasmite",
                    "--role",
                    "reader",
                    "--bindings",
                    str(bindings),
                    "--directory",
                    directory,
                    "--messages",
                    str(args.messages),
                ]
                child = subprocess.run(
                    command,
                    input=b"go\n",
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.PIPE,
                    check=True,
                )
                lines = child.stderr.splitlines()
                assert lines[0] == b"ready", lines
                data = json.loads(lines[-1])
                assert data["messages"] == args.messages
                result["runs"].append(dict(data, variant=variant, repeat=repeat + 1))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(str(args.output))


if __name__ == "__main__":
    main()
