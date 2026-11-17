from plasmite import FollowStart
#!/usr/bin/env python3
"""Check reader fairness, independent retention and direct view forwarding."""

import argparse
import gc
import hashlib
import json
import multiprocessing as mp
import sys
import time
import tracemalloc
from pathlib import Path
from tempfile import TemporaryDirectory

import plasmite


def receive(directory, count, batch_count, ready, start, output):
    with plasmite.Client(directory) as client, client.open_pool("events") as pool:
        with pool.follow_views(
            start=FollowStart.sequence(1), max_messages=count, error_on_gap=True
        ) as stream:
            ready.put(True)
            start.wait()
            wall, cpu = time.perf_counter_ns(), time.process_time_ns()
            seen = 0
            while seen < count:
                batch = (
                    [next(stream)]
                    if batch_count == 0
                    else stream.next_batch(batch_count, 262144)
                )
                for message in batch:
                    data = plasmite.parse_message(
                        plasmite.to_json(message).encode()
                    ).data
                    assert message.seq == seen + 1 and data == {"index": seen}
                    seen += 1
            output.put(
                {
                    "wall_ns": time.perf_counter_ns() - wall,
                    "cpu_ns": time.process_time_ns() - cpu,
                    "records": seen,
                }
            )


def fairness(count, batch_count):
    ctx = mp.get_context("spawn")
    with TemporaryDirectory() as directory, plasmite.Client(directory) as client:
        with client.create_pool("events", 8 * 1024 * 1024) as pool:
            ready, output, start = ctx.Queue(), ctx.Queue(), ctx.Event()
            readers = [
                ctx.Process(
                    target=receive,
                    args=(directory, count, batch_count, ready, start, output),
                )
                for _ in range(2)
            ]
            for reader in readers:
                reader.start()
            try:
                for _ in readers:
                    ready.get(timeout=20)
                wall, cpu = time.perf_counter_ns(), time.process_time_ns()
                start.set()
                for i in range(count):
                    pool.append_view({"index": i})
                writer = {
                    "wall_ns": time.perf_counter_ns() - wall,
                    "cpu_ns": time.process_time_ns() - cpu,
                }
                results = [output.get(timeout=20) for _ in readers]
                for reader in readers:
                    reader.join(20)
                    assert reader.exitcode == 0
                return {
                    "batch_count": batch_count,
                    "writer": writer,
                    "readers": results,
                }
            finally:
                for reader in readers:
                    if reader.is_alive():
                        reader.terminate()
                    reader.join()
                ready.close()
                output.close()


def retention():
    with TemporaryDirectory() as directory, plasmite.Client(directory) as client:
        with client.create_pool("events", 1024 * 1024) as pool:
            for i in range(32):
                pool.append_view({"index": i, "nested": [i], "padding": "x" * 128})
            gc.collect()
            tracemalloc.start()
            with pool.follow_views(start=FollowStart.sequence(1)) as stream:
                messages = stream.next_batch(16, 4096, 0)
                lengths = [len(memoryview(message)) for message in messages]
                child = messages[0].data["nested"]
                assert sum(lengths) <= 4096 and len(messages) <= 16
                peak = tracemalloc.get_traced_memory()[1]
                del messages
            pool.close()
            client.close()
            gc.collect()
            assert list(child) == [0]
            live_python = tracemalloc.get_traced_memory()[0]
            tracemalloc.stop()
            return {
                "ns_per_op": None,
                "max_count": 16,
                "max_bytes": 4096,
                "returned_owner_lengths": lengths,
                "retained_child_owner_bytes": lengths[0],
                "python_peak_bytes": peak,
                "python_live_bytes_with_child": live_python,
                "notes": [
                    "Tracemalloc excludes C/Rust payload storage and mapped pages.",
                    "Encoded lengths describe independent returned owners; no RSS claim.",
                    "A child remains readable after discarding the list and closing stream/pool/client.",
                ],
            }


def forwarding():
    runs = []
    with TemporaryDirectory() as directory, plasmite.Client(directory) as client:
        with client.create_pool("events", 16 * 1024 * 1024) as pool:
            for name, data, count in [
                ("small", {"value": 7}, 1500),
                ("wide", {f"field_{i}": {"value": i} for i in range(200)}, 150),
                ("long_string", {"text": "x" * 65536}, 100),
            ]:
                source = pool.append_view(data, ["source"])
                for repeat in range(5):
                    lanes = ["forward", "native-copy-encode", "root-subtree-copy"]
                    if repeat % 2:
                        lanes.reverse()
                    for lane in lanes:
                        start, cpu = time.perf_counter_ns(), time.process_time_ns()
                        for _ in range(count):
                            if lane == "forward":
                                result = pool.append_view(source)
                            elif lane == "root-subtree-copy":
                                result = pool.append_view(source.data, ["source"])
                            else:
                                result = pool.append_view(
                                    plasmite.to_native(source.data), ["source"]
                                )
                        elapsed, used_cpu = (
                            time.perf_counter_ns() - start,
                            time.process_time_ns() - cpu,
                        )
                        assert plasmite.to_native(result.data) == data and list(
                            result.tags
                        ) == ["source"]
                        runs.append(
                            {
                                "fixture": name,
                                "lane": lane,
                                "repeat": repeat + 1,
                                "operations": count,
                                "ns_per_op": elapsed / count,
                                "cpu_ns_per_op": used_cpu / count,
                            }
                        )
    return runs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--mode", choices=["fairness", "retention", "forwarding"], required=True
    )
    args = parser.parse_args()
    library = Path(plasmite._LIB._name)
    import plasmite._views as views

    backend_path = Path(views.backend.__file__)
    result = {
        "backend": views.VIEW_BACKEND,
        "backend_sha256": hashlib.sha256(backend_path.read_bytes()).hexdigest(),
        "facade_sha256": hashlib.sha256(Path(views.__file__).read_bytes()).hexdigest(),
        "python": sys.version,
        "library_sha256": hashlib.sha256(library.read_bytes()).hexdigest(),
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    }
    if args.mode == "fairness":
        result["runs"] = []
        for repeat in range(5):
            for batch_count in [0, 16] if repeat % 2 == 0 else [16, 0]:
                row = fairness(3000, batch_count)
                row["repeat"] = repeat + 1
                result["runs"].append(row)
    elif args.mode == "retention":
        result["diagnostic"] = retention()
    else:
        result["runs"] = forwarding()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(args.output)


if __name__ == "__main__":
    main()
