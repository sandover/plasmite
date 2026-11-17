#!/usr/bin/env python3
"""Compare UDP message-conversion Python allocation peaks; never report timings."""

import argparse
import gc
import hashlib
import json
import platform
import sys
import tracemalloc
from pathlib import Path
from tempfile import TemporaryDirectory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bindings", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    sys.path.insert(0, str(args.bindings.resolve()))
    import plasmite

    result = {
        "python": sys.version,
        "platform": platform.platform(),
        "library_sha256": hashlib.sha256(
            Path(plasmite._LIB._name).read_bytes()
        ).hexdigest(),
        "binding_sha256": hashlib.sha256(
            Path(plasmite.__file__).read_bytes()
        ).hexdigest(),
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "notes": [
            "Allocation diagnostics exclude C/Rust allocations; ns_per_op is null.",
            "Each measurement starts a fresh tracemalloc session; result survives until after the peak is read.",
        ],
        "runs": [],
    }
    with TemporaryDirectory() as directory, plasmite.Client(directory) as client:
        with client.open_pool("udp", create=True) as pool:
            for shape, data in [
                ("small", {"value": 7}),
                ("long_string", {"value": "x" * 65536}),
            ]:
                sent = pool.feed(data)
                payload = pool._get_lite3(sent.seq).payload
                frame = plasmite.UdpFrame(sent.seq, 1, payload)
                assert frame.message().data == data
                for repeat in range(7):
                    gc.collect()
                    tracemalloc.start()
                    message = frame.message()
                    current, peak = tracemalloc.get_traced_memory()
                    tracemalloc.stop()
                    assert message.data == data
                    result["runs"].append(
                        {
                            "fixture": shape,
                            "repeat": repeat + 1,
                            "ns_per_op": None,
                            "encoded_bytes": len(payload),
                            "python_current_bytes": current,
                            "python_peak_bytes": peak,
                        }
                    )
                    del message
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(str(args.output))


if __name__ == "__main__":
    main()
