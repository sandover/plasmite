"""Measure Python feed and finite reads at the public message boundary.

Set PLASMITE_LIB_DIR to the SDK directory and PYTHONPATH to the Python binding
under test. The script also runs against the prior 2.0 development binding.
"""

import argparse
import gc
import json
import statistics
import time
from tempfile import TemporaryDirectory

from plasmite import Client, Message
from plasmite._views import VIEW_BACKEND, to_native


def measure(width, operation, repeats):
    data = {"target": 7}
    data.update({f"field{i}": {"number": i, "text": "abc"} for i in range(width)})
    count = 5000 if width == 0 else 800
    current = hasattr(Message, "field")
    timings = []
    wall_timings = []
    for _ in range(repeats):
        with TemporaryDirectory() as directory, Client(directory) as client:
            with client.create_pool("probe", 32 * 1024 * 1024) as pool:
                feed = pool.feed if current else pool.feed_view
                fetch = pool.fetch if current else pool.fetch_view
                first = feed(data)
                assert first.seq == 1 and first.data["target"] == 7
                if operation.startswith("read_") or operation == "relay":
                    for _ in range(count - 1):
                        feed(data)
                if operation == "relay":
                    with client.create_pool("destination", 32 * 1024 * 1024) as destination:
                        append = destination.feed if current else destination.feed_view
                        gc.collect()
                        expected = 0
                        wall_started = time.perf_counter_ns()
                        cpu_started = time.process_time_ns()
                        with fetch() as reader:
                            for expected, message in enumerate(reader, 1):
                                assert message.seq == expected
                                assert append(message).seq == expected
                        timings.append((time.process_time_ns() - cpu_started) / count / 1000)
                        wall_timings.append((time.perf_counter_ns() - wall_started) / count / 1000)
                        read = destination.fetch if current else destination.fetch_view
                        for seq in (1, count):
                            with read(since=seq) as reader:
                                saved = next(reader)
                                assert saved.seq == seq
                                assert (saved.data if current else to_native(saved.data)) == data
                        assert expected == count
                    continue
                gc.collect()
                wall_started = time.perf_counter_ns()
                started = time.process_time_ns()
                if operation == "feed_metadata":
                    assert sum(feed(data).seq for _ in range(count)) == (count + 1) * (count + 2) // 2 - 1
                elif operation == "forward":
                    assert all(feed(first).seq > first.seq for _ in range(count))
                else:
                    with fetch() as reader:
                        if operation == "read_metadata":
                            assert sum(m.seq for m in reader) == count * (count + 1) // 2
                        elif operation == "read_full":
                            assert sum(len(m.data if current else to_native(m.data)) for m in reader) == count * len(data)
                        elif current:
                            assert sum(m.field("target") for m in reader) == count * 7
                        else:
                            assert sum(m.data["target"] for m in reader) == count * 7
                timings.append((time.process_time_ns() - started) / count / 1000)
                wall_timings.append((time.perf_counter_ns() - wall_started) / count / 1000)
    return {
        "other_fields": width,
        "operation": operation,
        "messages": count,
        "cpu_us_median": round(statistics.median(timings), 2),
        "cpu_us_min": round(min(timings), 2),
        "cpu_us_max": round(max(timings), 2),
        "wall_us_median": round(statistics.median(wall_timings), 2),
        "wall_us_min": round(min(wall_timings), 2),
        "wall_us_max": round(max(wall_timings), 2),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repeats", type=int, default=5)
    operations = ("feed_metadata", "forward", "read_metadata", "read_full", "read_one_field")
    parser.add_argument("--operation", choices=(*operations, "relay"))
    options = parser.parse_args()
    if options.operation is not None:
        operations = (options.operation,)
    rows = [measure(width, operation, options.repeats)
            for width in (0, 160) for operation in operations]
    print(json.dumps({"backend": VIEW_BACKEND, "rows": rows}, indent=2))


if __name__ == "__main__":
    main()
