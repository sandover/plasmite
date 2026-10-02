#!/usr/bin/env python3
"""Measure CLI JSONL writes and finite history reads with one process per batch."""
import argparse
import json
import subprocess
import time
from pathlib import Path

from bench_transport_comparison import serialized_payload


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--pool-dir", required=True)
    parser.add_argument("--pool", required=True)
    parser.add_argument("--server", required=True)
    parser.add_argument("--local-server", required=True)
    parser.add_argument("--messages", type=int, default=100)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.messages < 1 or args.repeats < 2:
        parser.error("need positive messages and at least two repeats")
    refs = {"local_cli_stream": args.pool, "local_http_cli_stream": args.local_server + "/" + args.pool,
            "https_cli_stream": args.server + "/" + args.pool}
    result = {"versions": {"plasmite": subprocess.check_output([args.binary, "--version"], text=True).strip()}, "configuration": vars(args) | {"output": str(args.output), "sizes": [512, 4096],
        "timing": "One CLI process per batch, including process startup, JSON input/output, and pipe transfer. Read uses follow --tail N --no-follow; this reads retained history rather than N exact-fetch requests."}, "runs": []}
    for size in [512, 4096]:
        for repeat in range(args.repeats):
            lanes = list(refs)
            offset = repeat % len(lanes)
            for lane in lanes[offset:] + lanes[:offset]:
                data = [serialized_payload(size, repeat + 1, i, lane) for i in range(args.messages)]
                payload = "\n".join(json.dumps(item, separators=(",", ":")) for item in data) + "\n"
                command = [args.binary, "--dir", args.pool_dir]
                started = time.perf_counter_ns()
                append = subprocess.run([*command, "feed", refs[lane], "--json"], input=payload,
                    capture_output=True, text=True, encoding="utf-8", check=True)
                append_ns = time.perf_counter_ns() - started
                receipts = [json.loads(line) for line in append.stdout.splitlines()]
                sequences = [receipt["seq"] for receipt in receipts]
                if len(sequences) != len(data) or len(set(sequences)) != len(data):
                    raise RuntimeError("stream append count mismatch")
                started = time.perf_counter_ns()
                read = subprocess.run([*command, "follow", refs[lane], "--tail", str(args.messages), "--no-follow", "--json"],
                    capture_output=True, text=True, encoding="utf-8", check=True, timeout=60)
                read_ns = time.perf_counter_ns() - started
                messages = [json.loads(line) for line in read.stdout.splitlines()]
                if len(messages) != len(data):
                    raise RuntimeError(f"stream read count mismatch: {len(messages)}")
                for message, sequence, expected in zip(messages, sequences, data, strict=True):
                    if message["seq"] != sequence or message["data"] != expected:
                        raise RuntimeError("stream readback mismatch")
                result["runs"].append({"payload_bytes": size, "repeat": repeat + 1, "lane": lane,
                    "sequences": sequences, "readback_verified": len(data),
                    "append": {"elapsed_ns": append_ns, "throughput_ops_s": len(data)*1e9/append_ns},
                    "read": {"elapsed_ns": read_ns, "throughput_ops_s": len(data)*1e9/read_ns}})
        print(f"Completed CLI streaming at {size} bytes", flush=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
