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
from textwrap import dedent
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def record():
    value = {
        "id": 0,
        "sensor": "motor-7",
        "metrics": {"power": 0, **{f"channel_{i}": i * 0.25 for i in range(16)}},
        "context": {"site": "factory-a", "line": 3, "labels": ["motor", "temperature"]},
        "note": ("x" * int(os.environ["PLASMITE_BENCH_NOTE_BYTES"])
                 if "PLASMITE_BENCH_NOTE_BYTES" in os.environ else "sensor observation " * 25),
    }

    if "PLASMITE_BENCH_EXTRA_FIELDS" in os.environ:
        value["fields"] = {f"field_{i}": i for i in range(int(os.environ["PLASMITE_BENCH_EXTRA_FIELDS"]))}
    return value


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
        current = pool is not None and hasattr(pool, "feed")
        single_message = current and hasattr(plasmite.Message, "field")
        probe = None
        if args.native_counts:
            import ctypes
            probe = ctypes.CDLL(str(args.native_counts))
            probe.plsm_bench_probe_begin.argtypes = []
            probe.plsm_bench_probe_begin.restype = None
            probe.plsm_bench_probe_end.argtypes = []
            probe.plsm_bench_probe_end.restype = ctypes.c_char_p
        value = record()
        fixture = (Path(args.directory) / "fixture.lite3").read_bytes() if args.binary else None
        if args.role == "writer" and args.receipt == "native-view":
            import plasmite._views  # Exclude extension loading from delivery timing.
        batch = None
        if pool is not None and args.role == "reader" and args.reader_views:
            import plasmite._views
            import bench_view_stream as batch
        views = None
        source_view = None
        if pool is not None and args.role == "writer" and (args.receipt == "view" or args.forward_view):
            from bench_view_abi import ViewABI
            views = ViewABI(plasmite)
            if args.forward_view:
                source_view = views.from_bytes(fixture)
                stack.callback(views.lib.plsm_view_release, source_view)
        if pool is not None and args.role == "writer" and not args.binary:
            if args.receipt == "view":
                store = lambda item: views.append(pool, item)
            elif single_message:
                store = pool.feed
            else:
                names = {
                    "full": ("feed", "append"),
                    "identity": ("feed_receipt", "append_identity"),
                    "native-view": ("feed_view", "append_view"),
                }
                store = getattr(pool, names[args.receipt][0 if current else 1])
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
        if probe:
            probe.plsm_bench_probe_begin()
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
                if args.producer_work_ns:
                    work_until = time.perf_counter_ns() + args.producer_work_ns
                    while time.perf_counter_ns() < work_until:
                        pass
                if pool is None:
                    # Match the Python binding's standard json.dumps settings.
                    sys.stdout.buffer.write(json.dumps(value).encode("utf-8") + b"\n")
                    if args.interval_ms or args.pipe_flush:
                        sys.stdout.buffer.flush()
                elif args.binary:
                    if args.forward_view:
                        seq = views.forward(pool, source_view).seq
                    elif single_message:
                        seq = pool.feed(fixture, format="lite3").seq
                    elif current:
                        seq = pool.feed_lite3_receipt(fixture).seq
                    else:
                        seq = pool.append_lite3(fixture, plasmite.Durability.FAST)
                    if seq != index + 1:
                        raise RuntimeError("binary append identity mismatch")
                else:
                    receipt = store(value)
                    timestamp = receipt.time if current else getattr(receipt, "timestamp_ns", None)
                    if (receipt.seq != index + 1
                            or (args.receipt == "full" and receipt.data != value)
                            or (args.receipt != "full" and (timestamp is None or timestamp <= 0))):
                        raise RuntimeError("append receipt mismatch")
            if pool is None:
                sys.stdout.buffer.flush()
        else:
            if current and batch is None:
                follow = pool.follow_lite3 if args.binary and not single_message else pool.follow
                stream = stack.enter_context(follow(since=1))

                def records():
                    while True:
                        message = stream.next(wait=120)
                        if message is None:
                            raise RuntimeError("timed out waiting for benchmark message")
                        yield message if args.binary else message.data

                incoming = records()
            else:
                incoming = pool.open_lite3_follow(
                    start=plasmite.FollowStart.sequence(1), max_messages=args.messages, timeout_ms=120_000, error_on_gap=True,
                ) if args.binary else (
                    (json.loads(line) for line in sys.stdin.buffer)
                    if pool is None else
                    batch.data(plasmite, pool, args.messages, args.reader_batch_count,
                               args.reader_batch_bytes, args.reader_view_conversion)
                    if batch is not None else
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
                    timestamp = item.time if current else item.timestamp_ns
                    payload = item.raw if single_message else item.payload
                    if item.seq != count + 1 or payload != fixture or timestamp <= 0:
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
                  "latencies_ns": latencies,
                  "peak_rss_bytes": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
                      * (1 if sys.platform == "darwin" else 1024)}
        if probe:
            result["native_operations"] = json.loads(probe.plsm_bench_probe_end())
        if args.binary:
            result["fixture_sha256"] = hashlib.sha256(fixture).hexdigest()
        print(json.dumps(result), file=sys.stderr, flush=True)


def trial(args, lane, directory, profile=False):
    environment = dict(os.environ, PLASMITE_LIB_DIR=str(args.library),
                       PYTHONDONTWRITEBYTECODE="1")
    command = [sys.executable, str(Path(__file__).resolve()), "--worker",
               "--lane", lane, "--messages", str(args.messages),
               "--directory", directory, "--bindings", str(args.bindings),
               "--interval-ms", str(args.interval_ms), "--receipt", args.receipt,
               "--producer-work-ns", str(args.producer_work_ns),
               "--reader-batch-count", str(args.reader_batch_count),
               "--reader-batch-bytes", str(args.reader_batch_bytes),
               "--reader-view-conversion", args.reader_view_conversion]
    if args.reader_views:
        command.append("--reader-views")
    if args.binary:
        command.append("--binary")
    if args.forward_view:
        command.append("--forward-view")
    if args.pipe_flush:
        command.append("--pipe-flush")
    if args.native_counts:
        command.extend(["--native-counts", str(args.native_counts.resolve())])
        environment["DYLD_INSERT_LIBRARIES"] = str(args.native_counts.resolve())
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
                setup = dedent(f"""\
                    import plasmite
                    from pathlib import Path

                    with plasmite.Client({directory!r}) as client:
                        with client.create_pool('fixture', 1048576) as seed:
                            value = {record()!r}
                            if hasattr(seed, 'feed'):
                                message = seed.feed(value, ['sensor'])
                                if hasattr(message, 'field'):
                                    payload = message.raw
                                else:
                                    with seed.fetch_lite3(since=message.seq) as reader:
                                        payload = next(reader).payload
                            else:
                                message = seed.append(value, ['sensor'])
                                payload = seed.get_lite3(message.seq).payload
                        Path({str(Path(directory) / 'fixture.lite3')!r}).write_bytes(payload)
                        size = max(16777216, {args.messages} * (len(payload) + 4096))
                        client.create_pool('events', size).close()
                    """)
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
            result["p99_latency_ms"] = latencies[max(0, (len(latencies) * 99 + 99) // 100 - 1)] / 1e6
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
    parser.add_argument("--receipt", choices=["full", "identity", "view", "native-view"], default="full",
                        help="producer receipt work; the consumer still decodes and verifies complete data")
    parser.add_argument("--baseline-receipt", choices=["full", "identity", "view", "native-view"], default="full")
    parser.add_argument("--producer-work-ns", type=int, default=0,
                        help="diagnostic busy work before each candidate append; same payload/result")
    parser.add_argument("--baseline-producer-work-ns", type=int, default=0)
    parser.add_argument("--reader-views", action="store_true")
    parser.add_argument("--reader-view-conversion", choices=["message-json", "native"], default="message-json")
    parser.add_argument("--reader-batch-count", type=int, default=0, help="explicit view batch; zero keeps ordinary view iteration")
    parser.add_argument("--baseline-reader-batch-count", type=int, default=0)
    parser.add_argument("--reader-batch-bytes", type=int, default=262144)
    parser.add_argument("--no-pipe", action="store_true", help="compare only the two Plasmite configurations")
    parser.add_argument("--compare-pipe-flush", action="store_true",
                        help="include both buffered and per-record-flushed pipes")
    parser.add_argument("--pipe-flush", action="store_true",
                        help="flush each pipe record; do not add pacing")
    parser.add_argument("--native-counts", type=Path,
                        help="macOS probe dylib; diagnostic counts only, not performance timing")
    parser.add_argument("--binary", action="store_true",
                        help="compare verified repeated Lite3 forwarding; omit the JSON pipe")
    parser.add_argument("--forward-view", action="store_true",
                        help="binary candidate forwards one validated immutable owner; baseline still validates input bytes")
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
    if (args.reader_batch_count or args.baseline_reader_batch_count) and not args.reader_views:
        parser.error("read batch counts require --reader-views")
    if args.reader_batch_count < 0 or args.baseline_reader_batch_count < 0 or args.reader_batch_bytes <= 0:
        parser.error("batch counts must be nonnegative; byte bound must be positive")
    if args.reader_views and args.binary:
        parser.error("reader views request message conversion, not repeated raw Lite3 frames")
    if args.forward_view and not args.binary:
        parser.error("forward-view needs --binary")
    if args.compare_pipe_flush and (args.binary or args.pipe_flush):
        parser.error("compare-pipe-flush needs JSON mode without pipe-flush")
    if args.native_counts and args.profile:
        parser.error("collect native counts and profiles in separate runs")
    if args.native_counts and sys.platform != "darwin":
        parser.error("native operation probe currently supports macOS only")
    if args.binary and args.pipe_flush:
        parser.error("binary forwarding omits the pipe")
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
              "reader_batch_count": args.reader_batch_count, "baseline_reader_batch_count": args.baseline_reader_batch_count,
              "reader_batch_bytes": args.reader_batch_bytes,
              "reader_views": args.reader_views, "reader_view_conversion": args.reader_view_conversion,
              "batch_adapter_sha256": hashlib.sha256((Path(__file__).parent / "bench_view_stream.py").read_bytes()).hexdigest() if args.reader_views else None,
              "interval_ms": args.interval_ms,
              "producer_work_ns": args.producer_work_ns, "baseline_producer_work_ns": args.baseline_producer_work_ns,
              "format": "lite3-repeat" if args.binary else "json-object",
              "receipt": args.receipt, "baseline_receipt": args.baseline_receipt,
              "forward_owned_view": args.forward_view,
              "native_view_work": "CPython exact builtin input walk, retained encoded result, native identity access; consumer still decodes and verifies complete data",
              "view_work": "benchmark-only ctypes producer: standard json.dumps, append view, read identity, release; consumer still verifies complete data",
              "forward_view_work": "candidate forwards one prevalidated immutable owner; baseline validates arbitrary encoded input on each append",
              "view_adapter_sha256": hashlib.sha256((Path(__file__).parent / "bench_view_abi.py").read_bytes()).hexdigest() if args.forward_view or args.receipt == "view" or args.baseline_receipt == "view" else None,
              "pipe_flush": args.pipe_flush, "compare_pipe_flush": args.compare_pipe_flush,
              "native_operation_diagnostic": bool(args.native_counts),
              "probe_sha256": hashlib.sha256(args.native_counts.read_bytes()).hexdigest() if args.native_counts else None,
              "first_record_json_bytes": len(json.dumps(record()).encode()),
              "runs": []}
    if args.reader_views:
        os.environ["PLASMITE_LIB_DIR"] = str(args.library)
        sys.path.insert(0, str(args.bindings))
        import plasmite._views as view_facade
        result["view_backend"] = view_facade.VIEW_BACKEND
        result["view_backend_sha256"] = hashlib.sha256(Path(view_facade.backend.__file__).read_bytes()).hexdigest()
        result["view_facade_sha256"] = hashlib.sha256(Path(view_facade.__file__).read_bytes()).hexdigest()
    if args.native_counts:
        result["native_probe_sha256"] = hashlib.sha256(args.native_counts.read_bytes()).hexdigest()
    libraries = {"plasmite": args.library}
    if args.baseline_library:
        libraries["baseline"] = args.baseline_library.resolve()
        baseline = libraries["baseline"] / library.name
        result["baseline_library"] = str(baseline)
        result["baseline_library_sha256"] = hashlib.sha256(baseline.read_bytes()).hexdigest()
        if args.baseline_bindings:
            args.baseline_bindings = args.baseline_bindings.resolve()
            result["baseline_bindings_sha256"] = hashlib.sha256((args.baseline_bindings / "plasmite" / "__init__.py").read_bytes()).hexdigest()
    if args.receipt == "native-view" or args.baseline_receipt == "native-view":
        result["native_view_sources_sha256"] = {
            str(path.relative_to(args.bindings)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in [args.bindings / "plasmite" / "_views.py",
                         args.bindings / "plasmite" / "_views_fallback.py",
                         args.bindings / "native" / "views.c",
                         *sorted((args.bindings / "plasmite").glob("_views_native*.so"))]
        }
    pipe_lanes = [] if args.binary or args.no_pipe else (["pipe", "pipe-flushed"] if args.compare_pipe_flush else ["pipe"])
    # Warm each complete path once; alternate order in the measured repeats.
    for repeat in range(args.repeats + 1):
        lanes = [*pipe_lanes, *libraries]
        if len(lanes) > 2:
            offset = repeat % len(lanes)
            lanes = lanes[offset:] + lanes[:offset]
        if repeat % 2:
            lanes.reverse()
        for label in lanes:
            lane = "pipe" if label in pipe_lanes else "plasmite"
            trial_args = argparse.Namespace(**vars(args))
            if label == "baseline":
                trial_args.forward_view = False
            if label == "pipe-flushed":
                trial_args.pipe_flush = True
            trial_args.library = libraries.get(label, args.library)
            if label == "baseline":
                trial_args.receipt = args.baseline_receipt
                trial_args.producer_work_ns = args.baseline_producer_work_ns
                trial_args.reader_batch_count = args.baseline_reader_batch_count
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
                           (["p50_latency_ms", "p95_latency_ms", "p99_latency_ms"] if args.interval_ms else []))}
        for lane in [*pipe_lanes, *libraries]
    }
    if args.profile:
        with tempfile.TemporaryDirectory(prefix="plasmite-profile-") as directory:
            result["profile_run"] = trial(args, "plasmite", directory, profile=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result["medians"], indent=2))


if __name__ == "__main__":
    main()
