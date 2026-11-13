#!/usr/bin/env python3
"""Matched Python view/native results; run timings and allocations separately."""

import argparse
import copy
import gc
import hashlib
import json
import platform
import resource
import sys
import tempfile
import time
import tracemalloc
from pathlib import Path

import plasmite
from plasmite import _views


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def measure(operation, verify, count, allocations):
    for _ in range(10):
        verify(operation())
    if allocations:
        gc.collect()
        tracemalloc.start()
    started = time.perf_counter_ns()
    for _ in range(count):
        value = operation()
    elapsed = time.perf_counter_ns() - started
    verify(value)
    result = {
        "operations": count,
        "ns_per_op": None if allocations else elapsed / count,
    }
    if allocations:
        current, peak = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        result.update(
            python_traced_current_bytes=current, python_traced_peak_bytes=peak
        )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=7)
    parser.add_argument("--allocations", action="store_true")
    parser.add_argument("--scale", type=float, default=1)
    args = parser.parse_args()
    if args.repeats < 1 or args.scale <= 0:
        parser.error("positive repeats and scale required")
    result = {
        "python": sys.version,
        "platform": platform.platform(),
        "backend": _views.VIEW_BACKEND,
        "allocation_diagnostic": args.allocations,
        "library_sha256": digest(plasmite._LIB._name),
        "runner_sha256": digest(__file__),
        "binding_sha256": digest(plasmite.__file__),
        "facade_sha256": digest(_views.__file__),
        "native_source_sha256": digest(
            Path(__file__).resolve().parents[1] / "native/views.c"
        ),
        "runs": [],
        "retention": [],
    }
    backend_file = getattr(_views.backend, "__file__", None)
    result["backend_sha256"] = digest(backend_file) if backend_file else None
    result["notes"] = [
        "Storage pages are warm; cold means fresh message acquisition.",
        "Tracemalloc excludes libplasmite/Rust/C allocations; diagnostic ns are null.",
        "Native copying creates independent editable containers. Warm object lookup uses an already decoded object.",
        "Fallback buffers keep extra immutable encoded copies. No native-tree fallback on arrival.",
    ]
    for shape, count in [
        ("small", 3000),
        ("wide", 500),
        ("long_string", 300),
        ("array", 200),
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
        with (
            tempfile.TemporaryDirectory() as directory,
            plasmite.Client(directory) as client,
        ):
            with client.create_pool("views", 8 * 1024 * 1024) as pool:
                seq = pool.append(data).seq
                message = pool.get_view(seq)
                old = pool.get(seq)
                view_data, native_data = message.data, old.data
                expected_sum = sum(data["samples"])
                checks = {
                    "field": lambda value: require(value == 7.5),
                    "native": lambda value: require(value == data),
                    "array": lambda value: require(value == expected_sum),
                    "json": lambda value: require(json.loads(value) == data),
                }
                lanes = {
                    "cold_view_field": (
                        lambda: pool.get_view(seq).data["metrics"]["power"],
                        checks["field"],
                    ),
                    "cold_legacy_field": (
                        lambda: pool.get(seq).data["metrics"]["power"],
                        checks["field"],
                    ),
                    "cold_view_native": (
                        lambda: plasmite.to_native(pool.get_view(seq).data),
                        checks["native"],
                    ),
                    "cold_legacy_native": (
                        lambda: pool.get(seq).data,
                        checks["native"],
                    ),
                    "warm_view_field": (
                        lambda: view_data["metrics"]["power"],
                        checks["field"],
                    ),
                    "warm_native_field": (
                        lambda: native_data["metrics"]["power"],
                        checks["field"],
                    ),
                    "view_array": (lambda: sum(view_data["samples"]), checks["array"]),
                    "native_array": (
                        lambda: sum(native_data["samples"]),
                        checks["array"],
                    ),
                    "view_copy": (
                        lambda: plasmite.to_native(view_data),
                        checks["native"],
                    ),
                    "native_copy": (
                        lambda: copy.deepcopy(native_data),
                        checks["native"],
                    ),
                    "view_json": (lambda: plasmite.to_json(view_data), checks["json"]),
                    "native_json": (
                        lambda: json.dumps(native_data, separators=(",", ":")),
                        checks["json"],
                    ),
                    "view_presence": (
                        lambda: "sensor" in view_data,
                        lambda value: require(value),
                    ),
                    "native_presence": (
                        lambda: "sensor" in native_data,
                        lambda value: require(value),
                    ),
                }
                for repeat in range(args.repeats):
                    order = list(lanes)
                    if repeat % 2:
                        order.reverse()
                    for lane in order:
                        operation, verify = lanes[lane]
                        measurement = measure(
                            operation,
                            verify,
                            max(1, int(count * args.scale)),
                            args.allocations,
                        )
                        result["runs"].append(
                            dict(
                                measurement, fixture=shape, lane=lane, repeat=repeat + 1
                            )
                        )
                encoded_length = len(memoryview(message))
                child, buffer = view_data["metrics"], memoryview(message)
                del message, view_data
            gc.collect()
            require(child["power"] == 7.5 and buffer.readonly)
            result["retention"].append(
                {
                    "fixture": shape,
                    "encoded_bytes": encoded_length,
                    "child_valid_after_pool_close": True,
                    "readonly_buffer": True,
                    "process_peak_rss_bytes": resource.getrusage(
                        resource.RUSAGE_SELF
                    ).ru_maxrss
                    * (1 if sys.platform == "darwin" else 1024),
                }
            )
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


def require(condition):
    if not condition:
        raise RuntimeError("benchmark result failed verification")


if __name__ == "__main__":
    main()
