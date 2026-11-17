#!/usr/bin/env python3
"""Check the callback-order fix using literal inputs that cannot close handles."""

import argparse
import ast
import hashlib
import json
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import time

import plasmite
from plasmite import _views


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    previous = subprocess.check_output(
        ["git", "show", "8752e35:bindings/python/plasmite/_views.py"], text=True
    )
    definition = next(
        node
        for node in ast.parse(previous).body
        if isinstance(node, ast.FunctionDef) and node.name == "append_view"
    )
    namespace = dict(vars(_views))
    exec(
        compile(
            ast.Module(body=[definition], type_ignores=[]),
            "previous-append-view",
            "exec",
        ),
        namespace,
    )
    before, after = namespace["append_view"], _views.append_view
    result = {
        "python": sys.version,
        "backend": _views.VIEW_BACKEND,
        "library_sha256": hashlib.sha256(
            Path(plasmite._LIB._name).read_bytes()
        ).hexdigest(),
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "previous_source_sha256": hashlib.sha256(previous.encode()).hexdigest(),
        "current_facade_sha256": hashlib.sha256(
            Path(_views.__file__).read_bytes()
        ).hexdigest(),
        "notes": [
            "Old ordering runs only with literal builtins and no user callbacks.",
            "Close-hook regression cases mock the final native boundary, avoiding unsafe old calls.",
        ],
        "runs": [],
    }
    try:
        for shape, count in [("small", 4000), ("wide", 300), ("long_string", 200)]:
            data = {"value": 7, "text": "sensor", "array": [1, 2, 3]}
            if shape == "wide":
                data.update({f"field_{i}": i for i in range(200)})
            if shape == "long_string":
                data["text"] = "x" * 65536
            with (
                TemporaryDirectory() as directory,
                plasmite.Client(directory) as client,
            ):
                with client.create_pool("guard", 8 * 1024 * 1024) as pool:
                    for repeat in range(7):
                        order = [("before", before), ("after", after)]
                        if repeat % 2:
                            order.reverse()
                        for name, function in order:
                            _views.append_view = function
                            for _ in range(5):
                                pool.feed_view(data)
                            cpu = time.process_time_ns()
                            start = time.perf_counter_ns()
                            for _ in range(count):
                                message = pool.feed_view(data)
                            elapsed = time.perf_counter_ns() - start
                            used_cpu = time.process_time_ns() - cpu
                            assert message.data == data
                            result["runs"].append(
                                {
                                    "fixture": shape,
                                    "variant": name,
                                    "repeat": repeat + 1,
                                    "operations": count,
                                    "ns_per_op": elapsed / count,
                                    "cpu_ns_per_op": used_cpu / count,
                                }
                            )
    finally:
        _views.append_view = after
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(str(args.output))


if __name__ == "__main__":
    main()
