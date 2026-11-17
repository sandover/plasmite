#!/usr/bin/env python3
"""Compare a JSON pipe with the ordinary local Plasmite Python API (Unix only).

Build with `cargo build --release --lib`, then run this script with --output.
Both lanes use one producer, one consumer, identical structured records, and
full record verification. Timings include encoding and consumption, but exclude
imports, pool creation, and process startup. Plasmite uses fast durability and
retains history; the pipe does not. --profile records separate cProfile runs.
"""

import argparse
import contextlib
import cProfile
import hashlib
import json
import os
import platform
import resource
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def record():
    return {
        "id": 0,
        "sensor": "motor-7",
        "metrics": {"power": 0, **{f"channel_{i}": i * 0.25 for i in range(16)}},
        "context": {"site": "factory-a", "line": 3, "labels": ["motor", "temperature"]},
        "note": ("x" * int(os.environ["PLASMITE_BENCH_NOTE_BYTES"])
                 if "PLASMITE_BENCH_NOTE_BYTES" in os.environ else "sensor observation " * 25),
    }


def update(value, index):
    value["id"] = index
    value["metrics"]["power"] = index * 3


def cpu_seconds():
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return usage.ru_utime + usage.ru_stime


def worker(args):
    with contextlib.ExitStack() as stack:
        pool = None
        if args.lane == "plasmite":
            sys.path.insert(0, str(args.bindings))
            import plasmite
            client = stack.enter_context(plasmite.Client(args.directory))
            pool = stack.enter_context(client.open_pool("events"))
        value = record()
        fixture = (Path(args.directory) / "fixture.lite3").read_bytes() if args.binary else None
        profile = cProfile.Profile() if args.profile else None
        print("ready", file=sys.stderr, flush=True)
        # The pipe reader starts blocked on data. Other workers await a start token.
        if args.role == "writer" or args.lane == "plasmite":
            if sys.stdin.buffer.readline() != b"go\n":
                raise RuntimeError("missing start token")
        started_cpu = cpu_seconds()
        started = time.monotonic_ns()
        if profile:
            profile.enable()
        checksum = 0
        latencies = []
        if args.role == "writer":
            due = time.monotonic()
            for index in range(args.messages):
                if not args.binary:
                    update(value, index)
                if args.interval_ms:
                    delay = due - time.monotonic()
                    if delay > 0:
                        time.sleep(delay)
                    value["sent_ns"] = time.monotonic_ns()
                    due += args.interval_ms / 1000
                if pool is None:
                    # Match Pool.append's standard json.dumps settings.
                    sys.stdout.buffer.write(json.dumps(value).encode("utf-8") + b"\n")
                    if args.interval_ms:
                        sys.stdout.buffer.flush()
                elif args.binary:
                    if pool.append_lite3(fixture, plasmite.Durability.FAST) != index + 1:
                        raise RuntimeError("binary append identity mismatch")
                else:
                    receipt = (pool.append_identity(value) if args.receipt == "identity"
                               else pool.append(value))
                    if (receipt.seq != index + 1
                            or (args.receipt == "full" and receipt.data != value)
                            or (args.receipt == "identity" and receipt.timestamp_ns <= 0)):
                        raise RuntimeError("append receipt mismatch")
            if pool is None:
                sys.stdout.buffer.flush()
        else:
            incoming = pool.open_lite3_follow(
                start=plasmite.FollowStart.sequence(1), max_messages=args.messages, timeout_ms=120_000, error_on_gap=True,
            ) if args.binary else (
                (json.loads(line) for line in sys.stdin.buffer)
                if pool is None else
                (message.data for message in pool.follow(
                    start=plasmite.FollowStart.sequence(1), max_messages=args.messages,
                    timeout_ms=120_000, error_on_gap=True,
                ))
            )
            count = 0
            for item in incoming:
                if args.interval_ms:
                    received = time.monotonic_ns()
                    latencies.append(received - item.pop("sent_ns"))
                if args.binary:
                    if item.seq != count + 1 or item.payload != fixture or item.timestamp_ns <= 0:
                        raise RuntimeError(f"binary frame mismatch at {count}")
                    checksum += count * 3
                else:
                    update(value, count)
                    if item != value:
                        raise RuntimeError(f"record mismatch at {count}")
                    checksum += item["metrics"]["power"]
                count += 1
                if count == args.messages:
                    break
            if count != args.messages:
                raise RuntimeError(f"received {count}, expected {args.messages}")
        ended = time.monotonic_ns()
        used_cpu = cpu_seconds() - started_cpu
        if profile:
            profile.disable()
            profile.dump_stats(args.profile)
        result = {"role": args.role, "cpu_seconds": used_cpu,
                  "started_ns": started, "ended_ns": ended,
                  "messages": args.messages, "checksum": checksum,
                  "latencies_ns": latencies}
        if args.binary:
            result["fixture_sha256"] = hashlib.sha256(fixture).hexdigest()
        print(json.dumps(result), file=sys.stderr, flush=True)


def trial(args, lane, directory, profile=False):
    environment = dict(os.environ, PLASMITE_LIB_DIR=str(args.library),
                       PYTHONDONTWRITEBYTECODE="1")
    command = [sys.executable, str(Path(__file__).resolve()), "--worker",
               "--lane", lane, "--messages", str(args.messages),
               "--directory", directory, "--bindings", str(args.bindings),
               "--interval-ms", str(args.interval_ms), "--receipt", args.receipt]
    if args.binary:
        command.append("--binary")
    processes = []
    try:
        if lane == "plasmite":
            # Import only after choosing the library, just as in the workers.
            setup = (
                "import plasmite; "
                f"c=plasmite.Client({directory!r}); "
                f"p=c.create_pool('events', {max(16 * 1024 * 1024, args.messages * max(8192, 2 * len(json.dumps(record()).encode()) + 4096))}); "
                "p.close(); c.close()"
            )
            if args.binary:
                setup = (
                    "import plasmite; from pathlib import Path; "
                    f"c=plasmite.Client({directory!r}); "
                    "seed=c.create_pool('fixture',1048576); "
                    f"m=seed.append({record()!r}, ['sensor']); "
                    "payload=seed.get_lite3(m.seq).payload; "
                    f"Path({str(Path(directory) / 'fixture.lite3')!r}).write_bytes(payload); "
                    "seed.close(); "
                    f"p=c.create_pool('events', max(16777216, {args.messages} * (len(payload)+4096))); "
                    "p.close(); c.close()"
                )
            setup_env = dict(environment, PYTHONPATH=str(args.bindings))
            subprocess.run([sys.executable, "-c", setup], env=setup_env, check=True)
        writer_command = [*command, "--role", "writer"]
        reader_command = [*command, "--role", "reader"]
        if profile:
            for role, cmd in [("writer", writer_command), ("reader", reader_command)]:
                cmd.extend(["--profile", str(args.output.parent / f"{lane}-{role}.pstats")])
        writer = subprocess.Popen(writer_command, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  env=environment)
        processes.append(writer)
        reader = subprocess.Popen(reader_command,
                                  stdin=writer.stdout if lane == "pipe" else subprocess.PIPE,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                  env=environment)
        processes.append(reader)
        writer.stdout.close()
        writer.stdout = None
        for process in processes:
            if process.stderr.readline() != b"ready\n":
                raise RuntimeError(process.stderr.read().decode())
        started = time.monotonic_ns()
        if lane == "plasmite":
            reader.stdin.write(b"go\n")
            reader.stdin.flush()
        writer.stdin.write(b"go\n")
        writer.stdin.flush()
        reports = []
        for process in processes:
            _, stderr = process.communicate(timeout=120)
            output = stderr.decode()
            if process.returncode:
                raise RuntimeError(output)
            reports.append(json.loads(output))
        elapsed = (max(report["ended_ns"] for report in reports) - started) / 1e9
        expected = 3 * args.messages * (args.messages - 1) // 2
        if reports[1]["checksum"] != expected:
            raise RuntimeError("consumer checksum mismatch")
        result = {"lane": lane, "elapsed_seconds": elapsed,
                "messages_per_second": args.messages / elapsed,
                "cpu_seconds": sum(report["cpu_seconds"] for report in reports),
                "workers": reports, "verified_messages": args.messages,
                "profiled": profile}
        if args.interval_ms:
            latencies = sorted(reports[1]["latencies_ns"])
            result["p50_latency_ms"] = statistics.median(latencies) / 1e6
            result["p95_latency_ms"] = latencies[max(0, (len(latencies) * 95 + 99) // 100 - 1)] / 1e6
        return result
    finally:
        for process in processes:
            if process.poll() is None:
                process.kill()
            process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", type=Path, default=ROOT / "target" / "release")
    parser.add_argument("--baseline-library", type=Path,
                        help="also measure a saved library in alternating order")
    parser.add_argument("--bindings", type=Path, default=ROOT / "bindings" / "python")
    parser.add_argument("--baseline-bindings", type=Path,
                        help="Python bindings to use with the saved baseline library")
    parser.add_argument("--receipt", choices=["full", "identity"], default="full",
                        help="producer receipt work; the consumer still decodes and verifies complete data")
    parser.add_argument("--baseline-receipt", choices=["full", "identity"], default="full")
    parser.add_argument("--binary", action="store_true",
                        help="compare verified repeated Lite3 forwarding; omit the JSON pipe")
    parser.add_argument("--messages", type=int, default=20_000)
    parser.add_argument("--interval-ms", type=float, default=0,
                        help="pace records and measure delivery latency (pipe flushes each record)")
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--profile", nargs="?", const=True)
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--role", choices=["writer", "reader"], help=argparse.SUPPRESS)
    parser.add_argument("--lane", choices=["pipe", "plasmite"], help=argparse.SUPPRESS)
    parser.add_argument("--directory", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.messages < 1 or args.repeats < 1 or args.interval_ms < 0:
        parser.error("messages and repeats must be positive; interval must be nonnegative")
    if args.binary and (args.receipt != "full" or args.baseline_receipt != "full"):
        parser.error("receipt choices apply to JSON producers only")
    if args.binary and args.interval_ms:
        parser.error("binary forwarding currently measures saturated traffic only")
    if args.worker:
        worker(args)
        return
    if args.output is None:
        parser.error("--output is required")
    args.library = args.library.resolve()
    args.bindings = args.bindings.resolve()
    args.output = args.output.resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    library = args.library / ("libplasmite.dylib" if sys.platform == "darwin" else "libplasmite.so")
    result = {"platform": platform.platform(), "machine": platform.machine(),
              "python": sys.version, "library": str(library),
              "library_sha256": hashlib.sha256(library.read_bytes()).hexdigest(),
              "bindings_sha256": hashlib.sha256((args.bindings / "plasmite" / "__init__.py").read_bytes()).hexdigest(),
              "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "messages": args.messages, "repeats": args.repeats,
              "interval_ms": args.interval_ms,
              "format": "lite3-repeat" if args.binary else "json-object",
              "receipt": args.receipt, "baseline_receipt": args.baseline_receipt,
              "first_record_json_bytes": len(json.dumps(record()).encode()),
              "runs": []}
    libraries = {"plasmite": args.library}
    if args.baseline_library:
        libraries["baseline"] = args.baseline_library.resolve()
        baseline = libraries["baseline"] / library.name
        result["baseline_library"] = str(baseline)
        result["baseline_library_sha256"] = hashlib.sha256(baseline.read_bytes()).hexdigest()
        if args.baseline_bindings:
            args.baseline_bindings = args.baseline_bindings.resolve()
            result["baseline_bindings_sha256"] = hashlib.sha256((args.baseline_bindings / "plasmite" / "__init__.py").read_bytes()).hexdigest()
    # Warm both complete paths once; alternate order in the measured repeats.
    for repeat in range(args.repeats + 1):
        lanes = list(libraries) if args.binary else ["pipe", *libraries]
        if not args.binary:
            offset = repeat % len(lanes)
            lanes = lanes[offset:] + lanes[:offset]
        if repeat % 2:
            lanes.reverse()
        for label in lanes:
            lane = "pipe" if label == "pipe" else "plasmite"
            trial_args = argparse.Namespace(**vars(args))
            trial_args.library = libraries.get(label, args.library)
            if label == "baseline":
                trial_args.receipt = args.baseline_receipt
            if label == "baseline" and args.baseline_bindings:
                trial_args.bindings = args.baseline_bindings
            with tempfile.TemporaryDirectory(prefix="plasmite-pipe-") as directory:
                measured = trial(trial_args, lane, directory)
            if repeat:
                measured["lane"] = label
                measured["repeat"] = repeat
                result["runs"].append(measured)
                print(f"{label}: {measured['messages_per_second']:,.0f} messages/s", flush=True)
    result["medians"] = {
        lane: {key: statistics.median(run[key] for run in result["runs"] if run["lane"] == lane)
               for key in (["elapsed_seconds", "messages_per_second", "cpu_seconds"] +
                           (["p50_latency_ms", "p95_latency_ms"] if args.interval_ms else []))}
        for lane in (list(libraries) if args.binary else ["pipe", *libraries])
    }
    if args.profile:
        with tempfile.TemporaryDirectory(prefix="plasmite-profile-") as directory:
            result["profile_run"] = trial(args, "plasmite", directory, profile=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result["medians"], indent=2))


if __name__ == "__main__":
    main()
