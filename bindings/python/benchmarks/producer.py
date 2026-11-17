#!/usr/bin/env python3
"""Compare complete Python producer goals, including requested conversion."""

import argparse
import gc
import hashlib
import json
import platform
import sys
import tempfile
import time
import tracemalloc
from pathlib import Path

import plasmite
from plasmite import _views


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def run(operation, verify, count, allocations):
    for _ in range(5):
        verify(operation())
    if allocations:
        gc.collect()
        tracemalloc.start()
    cpu = time.process_time_ns()
    start = time.perf_counter_ns()
    for _ in range(count):
        result = operation()
    elapsed, used_cpu = time.perf_counter_ns() - start, time.process_time_ns() - cpu
    verify(result)
    record = {
        "operations": count,
        "ns_per_op": None if allocations else elapsed / count,
        "cpu_ns_per_op": None if allocations else used_cpu / count,
    }
    if allocations:
        current, peak = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        record.update(
            python_traced_current_bytes=current, python_traced_peak_bytes=peak
        )
    return record


def require(condition):
    if not condition:
        raise RuntimeError("producer benchmark result mismatch")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=7)
    parser.add_argument("--scale", type=float, default=1)
    parser.add_argument("--allocations", action="store_true")
    args = parser.parse_args()
    if args.repeats < 1 or args.scale <= 0:
        parser.error("positive scale and repeats required")
    result = {
        "python": sys.version,
        "platform": platform.platform(),
        "backend": _views.VIEW_BACKEND,
        "library_sha256": digest(plasmite._LIB._name),
        "runner_sha256": digest(__file__),
        "binding_sha256": digest(plasmite.__file__),
        "facade_sha256": digest(_views.__file__),
        "backend_sha256": digest(_views.backend.__file__),
        "native_source_sha256": digest(
            Path(__file__).resolve().parents[1] / "native/views.c"
        ),
        "allocation_diagnostic": args.allocations,
        "runs": [],
        "notes": [
            "Fresh append, Fast durability, warm mapped pool; fixture/import/setup excluded.",
            "Identity control returns only identity; append_view also retains encoded data.",
            "Selected/native/JSON lanes compare the same requested final value, while legacy API performs its required extra work.",
            "Compact controls use compact UTF-8 JSON; legacy append uses existing json.dumps defaults.",
            "Tracemalloc excludes native C/Rust allocations; diagnostic timings are null.",
        ],
    }
    for shape, base_count in [
        ("small", 2000),
        ("wide", 300),
        ("long_string", 200),
        ("array", 100),
        ("bytes", 200),
    ]:
        data = {"sensor": "motor-7", "metrics": {"power": 7.5}, "samples": [1, 2, 3]}
        if shape == "wide":
            data.update(
                {f"field_{i}": {"name": "sensor", "value": i} for i in range(200)}
            )
        elif shape == "long_string":
            data["note"] = "x" * 65536
        elif shape == "array":
            data["samples"] = list(range(1000))
        elif shape == "bytes":
            data["blob"] = b"x" * 65536
        with (
            tempfile.TemporaryDirectory() as directory,
            plasmite.Client(directory) as client,
        ):
            with client.create_pool("producer", 8 * 1024 * 1024) as pool:

                def retained():
                    return pool.feed_view(data)

                def native_result():
                    message = pool.feed_view(data)
                    return message.seq, plasmite.to_native(message.data)

                def selected():
                    message = pool.feed_view(data)
                    return message.seq, message.data["metrics"]["power"]

                lanes = {
                    "view_retained": (
                        retained,
                        lambda m: require(
                            m.seq > 0 and m.time > 0 and m.data == data
                        ),
                    ),
                    "view_native": (
                        native_result,
                        lambda m: require(m[0] > 0 and m[1] == data),
                    ),
                    "view_selected": (
                        selected,
                        lambda m: require(m[0] > 0 and m[1] == 7.5),
                    ),
                }
                if shape != "bytes":

                    def compact():
                        return json.dumps(
                            data, ensure_ascii=False, separators=(",", ":")
                        ).encode("utf-8")

                    def compact_identity():
                        return pool.feed_json_receipt(
                            compact(), [], plasmite.Durability.FAST
                        )

                    def legacy_selected():
                        message = pool.feed(data)
                        return message.seq, message.data["metrics"]["power"]

                    def legacy_native():
                        message = pool.feed(data)
                        return message.seq, message.data

                    def compact_native():
                        message = plasmite.parse_message(
                            pool.feed_json(compact(), [], plasmite.Durability.FAST)
                        )
                        return message.seq, message.data

                    def view_json():
                        return plasmite.to_json(pool.feed_view(data))

                    def compact_json():
                        return pool.feed_json(
                            compact(), [], plasmite.Durability.FAST
                        ).decode("utf-8")

                    check_identity = lambda m: require(m.seq > 0 and m.time > 0)
                    check_json = lambda m: require(json.loads(m)["data"] == data)
                    lanes.update(
                        {
                            "legacy_identity": (
                                lambda: pool.feed_receipt(data),
                                check_identity,
                            ),
                            "compact_identity": (compact_identity, check_identity),
                            "legacy_selected": (
                                legacy_selected,
                                lambda m: require(m[0] > 0 and m[1] == 7.5),
                            ),
                            "legacy_native": (
                                legacy_native,
                                lambda m: require(m[0] > 0 and m[1] == data),
                            ),
                            "compact_native": (
                                compact_native,
                                lambda m: require(m[0] > 0 and m[1] == data),
                            ),
                            "view_json": (view_json, check_json),
                            "compact_json": (compact_json, check_json),
                        }
                    )
                for repeat in range(args.repeats):
                    order = list(lanes)
                    if repeat % 2:
                        order.reverse()
                    for lane in order:
                        operation, verify = lanes[lane]
                        measured = run(
                            operation,
                            verify,
                            max(1, int(base_count * args.scale)),
                            args.allocations,
                        )
                        result["runs"].append(
                            dict(measured, fixture=shape, lane=lane, repeat=repeat + 1)
                        )
                require(pool._get_view(pool.feed_view(data).seq).data == data)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(
        json.dumps(
            {
                "backend": result["backend"],
                "verified_lanes": len(result["runs"]),
                "output": str(args.output),
            }
        )
    )


if __name__ == "__main__":
    main()
