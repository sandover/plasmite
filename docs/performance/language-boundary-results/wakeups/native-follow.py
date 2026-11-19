"""Matched native follow measurements on macOS, Linux and Windows.

Run with Python 3.10+ and paths to two Plasmite 2.0 shared libraries.
Each reader runs in its own process. Use a local filesystem for the pools.
"""
import argparse
import ctypes as c
import json
import os
from pathlib import Path
import random
import statistics
import subprocess
import sys
import tempfile
import threading
import time

FOREVER = (1 << 64) - 1
POINTER = c.c_void_p
OUT = c.POINTER(POINTER)


class Buffer(c.Structure):
    _fields_ = [("data", POINTER), ("size", c.c_size_t)]


class Receipt(c.Structure):
    _fields_ = [("seq", c.c_uint64), ("time", c.c_uint64)]


class Frame(c.Structure):
    _fields_ = [("seq", c.c_uint64), ("time", c.c_uint64), ("flags", c.c_uint32), ("payload", Buffer)]


class Native:
    def __init__(self, library):
        self.library = c.CDLL(library)

    def bind(self, name, args, result=c.c_int):
        fn = getattr(self.library, "plsm_" + name)
        fn.argtypes = args
        fn.restype = result
        return fn

    def pool(self, directory, name, create):
        client, pool = POINTER(), POINTER()
        error = POINTER()
        new = self.bind("client_new", [c.c_char_p, OUT, OUT])
        assert new(os.fsencode(directory), c.byref(client), c.byref(error)) == 0
        if create:
            call = self.bind("pool_create", [POINTER, c.c_char_p, c.c_uint64, OUT, OUT])
            assert call(client, name, 32 * 1024**2, c.byref(pool), c.byref(error)) == 0
        else:
            call = self.bind("pool_open", [POINTER, c.c_char_p, c.c_uint32, OUT, OUT])
            assert call(client, name, 0, c.byref(pool), c.byref(error)) == 0
        return client, pool

    def close_pool(self, client, pool):
        self.bind("pool_free", [POINTER], None)(pool)
        self.bind("client_free", [POINTER], None)(client)

    def publisher(self, pool, encoded=None):
        symbol = "pool_feed_json_receipt" if encoded is None else "pool_feed_lite3_receipt"
        feed = self.bind(symbol, [POINTER, c.c_char_p, c.c_size_t, POINTER, c.POINTER(Receipt), OUT])
        receipt, error = Receipt(), POINTER()

        def publish(data):
            body = data if encoded is None else encoded
            assert feed(pool, body, len(body), None, c.byref(receipt), c.byref(error)) == 0
            return receipt.seq

        return publish

    def encoded_fixture(self, directory):
        client, pool = self.pool(directory, b"seed", True)
        self.publisher(pool)(b'{"n":1,"sensor":"room"}')
        stream, error, frame = POINTER(), POINTER(), Frame()
        fetch = self.bind("pool_fetch_lite3", [POINTER, POINTER, OUT, OUT])
        read = self.bind("lite3_stream_next_wait", [POINTER, c.c_uint64, c.POINTER(Frame), OUT])
        assert fetch(pool, None, c.byref(stream), c.byref(error)) == 0
        assert read(stream, FOREVER, c.byref(frame), c.byref(error)) == 1
        encoded = c.string_at(frame.payload.data, frame.payload.size)
        self.bind("lite3_frame_free", [c.POINTER(Frame)], None)(c.byref(frame))
        self.bind("lite3_stream_free", [POINTER], None)(stream)
        self.close_pool(client, pool)
        return encoded


def counters():
    cpu = time.process_time_ns()
    if os.name != "nt":
        import resource
        usage = resource.getrusage(resource.RUSAGE_SELF)
        scale = 1 if sys.platform == "darwin" else 1024
        return cpu, usage.ru_nvcsw, usage.ru_maxrss * scale

    class Memory(c.Structure):
        _fields_ = [("size", c.c_ulong), ("faults", c.c_ulong)] + [
            (name, c.c_size_t)
            for name in ("peak", "working", "quota_peak_paged", "quota_paged",
                         "quota_peak_nonpaged", "quota_nonpaged", "pagefile", "peak_pagefile")
        ]

    current = c.windll.kernel32.GetCurrentProcess
    current.restype = POINTER
    query = c.windll.psapi.GetProcessMemoryInfo
    query.argtypes = [POINTER, c.POINTER(Memory), c.c_ulong]
    memory = Memory()
    memory.size = c.sizeof(memory)
    assert query(current(), c.byref(memory), memory.size)
    return cpu, None, memory.peak


def emit(value):
    print(json.dumps(value), flush=True)


def reader(library, directory, count, paced):
    native = Native(library)
    client, pool = native.pool(directory, b"events", False)
    stream, error = POINTER(), POINTER()
    open_reader = native.bind("pool_follow", [POINTER, POINTER, OUT, OUT])
    wait = native.bind("stream_next_wait", [POINTER, c.c_uint64, c.POINTER(Buffer), OUT])
    cancel = native.bind("stream_cancel", [POINTER], None)
    free = native.bind("buf_free", [c.POINTER(Buffer)], None)
    assert open_reader(pool, None, c.byref(stream), c.byref(error)) == 0
    cancel_times = []

    def control():
        for line in sys.stdin:
            if line.strip() == "sample":
                emit(counters())
            elif line.strip() == "stop":
                cancel_times.append(time.perf_counter_ns())
                cancel(stream)
                return

    controller = threading.Thread(target=control, daemon=True)
    controller.start()
    emit("ready")
    samples = []
    buf = Buffer()
    for expected in range(1, count + 1):
        assert wait(stream, FOREVER, c.byref(buf), c.byref(error)) == 1
        arrived = time.perf_counter_ns()
        msg = json.loads(c.string_at(buf.data, buf.size))
        free(c.byref(buf))
        assert msg["seq"] == expected
        if paced:
            assert msg["data"]["n"] == expected
            samples.append(arrived - msg["data"]["sent"])
    completed = time.perf_counter_ns()
    emit("delivered")
    assert wait(stream, FOREVER, c.byref(buf), c.byref(error)) == 3
    stopped = time.perf_counter_ns()
    controller.join()
    emit({"delivery_ns": samples, "completed_ns": completed, "cancel_ns": stopped - cancel_times[0],
          "peak_rss": counters()[2]})
    native.bind("stream_free", [POINTER], None)(stream)
    native.close_pool(client, pool)


def reply(process):
    line = process.stdout.readline()
    if not line:
        raise RuntimeError(f"worker exited without a reply ({process.poll()})")
    return json.loads(line)


def tell(process, command):
    process.stdin.write(command + "\n")
    process.stdin.flush()


def run(library, directory, readers, workload, count, phase, quiet, reader_library=None):
    native = Native(library)
    client, pool = native.pool(directory, b"events", True)
    encoded = native.encoded_fixture(directory) if workload == "encoded" else None
    publish = native.publisher(pool, encoded)
    followers = []
    paced = workload == "paced"
    try:
        for _ in range(readers):
            argv = [sys.executable, __file__, "--reader", reader_library or library, directory, str(count), str(int(paced))]
            followers.append(subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True))
        # A watchdog only bounds an interrupted or broken benchmark. It never
        # wakes a timed reader in the healthy run.
        watchdog = threading.Timer(60, lambda: [p.kill() for p in followers if p.poll() is None])
        watchdog.daemon = True
        watchdog.start()
        for p in followers:
            assert reply(p) == "ready"

        idle = []
        if paced:
            time.sleep(0.15)
            for p in followers:
                tell(p, "sample")
            before = [reply(p) for p in followers]
            start = time.perf_counter_ns()
            time.sleep(quiet)
            for p in followers:
                tell(p, "sample")
            after = [reply(p) for p in followers]
            idle = [dict(cpu_ns=end[0] - begin[0],
                         voluntary_switches=None if end[1] is None else end[1] - begin[1],
                         duration_ns=time.perf_counter_ns() - start)
                    for begin, end in zip(before, after)]
        start = time.perf_counter_ns()
        for seq in range(1, count + 1):
            body = b'{"n":1,"sensor":"room"}'
            if paced:
                time.sleep((73 + (seq * 31 + phase * 17) % 39) / 1000)
                body = json.dumps({"n": seq, "sent": time.perf_counter_ns()}).encode()
            assert publish(body) == seq
        finished = time.perf_counter_ns()
        for p in followers:
            assert reply(p) == "delivered"
        for p in followers:
            tell(p, "stop")
        rows = [reply(p) for p in followers]
        for p in followers:
            assert p.wait(timeout=5) == 0
        return dict(workload=workload, readers=readers, count=count, idle=idle,
                    writer_peak_rss=counters()[2], writer_ns=finished-start,
                    last_reader_ns=max((r["completed_ns"] - start for r in rows), default=None),
                    readers_data=rows)
    finally:
        if "watchdog" in locals():
            watchdog.cancel()
        for p in followers:
            if p.poll() is None:
                p.kill()
            p.wait()
        native.close_pool(client, pool)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=lambda p: str(Path(p).resolve()))
    parser.add_argument("candidate", type=lambda p: str(Path(p).resolve()))
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--count", type=int, default=20000)
    parser.add_argument("--quiet", type=float, default=2)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--mixed", action="store_true", help="Also test both mixed writer/reader pairs")
    args = parser.parse_args()
    variants = [("baseline", args.baseline), ("candidate", args.candidate)]
    rng = random.Random(21793)
    rows = []
    for repeat in range(args.repeats):
        cases = [(1, "paced"), (4, "paced"), (0, "normal"), (0, "encoded"),
                 (4, "normal"), (4, "encoded")]
        rng.shuffle(cases)
        for readers, workload in cases:
            for variant, library in rng.sample(variants, 2):
                with tempfile.TemporaryDirectory() as directory:
                    argv = [sys.executable, __file__, "--worker", library, directory, str(readers),
                            workload, str(24 if workload == "paced" else args.count),
                            str(repeat), str(args.quiet)]
                    row = json.loads(subprocess.check_output(argv, text=True, timeout=70))
                    rows.append(dict(row, variant=variant, repeat=repeat))
                    args.output.write_text(json.dumps(rows, indent=2) + "\n")
        print(f"Completed round {repeat + 1}/{args.repeats}", flush=True)

    if args.mixed:
        for writer, reader_library in ((args.baseline, args.candidate), (args.candidate, args.baseline)):
            with tempfile.TemporaryDirectory() as directory:
                argv = [sys.executable, __file__, "--worker", writer, directory, "4", "paced",
                        "24", "0", str(args.quiet), reader_library]
                row = json.loads(subprocess.check_output(argv, text=True, timeout=70))
                rows.append(dict(row, variant="mixed", writer=writer, reader_library=reader_library))
        args.output.write_text(json.dumps(rows, indent=2) + "\n")

    for readers, workload in sorted({(row["readers"], row["workload"]) for row in rows}):
        for variant, _ in variants:
            subset = [r for r in rows if (r["readers"], r["workload"], r["variant"]) ==
                      (readers, workload, variant)]
            completion = [r["last_reader_ns"] for r in subset if r["last_reader_ns"] is not None]
            last_delivery = sorted(max(samples) for r in subset for samples in
                                   zip(*(m["delivery_ns"] for m in r["readers_data"])))
            p95 = last_delivery[(95*len(last_delivery)-1)//100] / 1e6 if last_delivery else None
            def ratio(field):
                return round(statistics.median(r["count"] * 1e9 / r[field] for r in subset))
            emit(dict(variant=variant, readers=readers, workload=workload, all_readers_p95_ms=p95,
                      writer_messages_sec=ratio("writer_ns") if workload != "paced" else None,
                      drained_messages_sec=ratio("last_reader_ns") if completion and workload != "paced" else None,
                      writer_peak_mib=statistics.median(r["writer_peak_rss"] for r in subset) / 1024**2,
                      reader_peak_mib=statistics.median(m["peak_rss"] for r in subset for m in r["readers_data"]) / 1024**2
                      if readers else None))


if __name__ == "__main__":
    if sys.argv[1:2] == ["--reader"]:
        _, _, lib, directory, count, paced = sys.argv
        reader(lib, directory, int(count), bool(int(paced)))
    elif sys.argv[1:2] == ["--worker"]:
        _, _, lib, directory, readers, workload, count, phase, quiet, *other_lib = sys.argv
        emit(run(lib, directory, int(readers), workload, int(count), int(phase),
                 float(quiet), *other_lib))
    else:
        main()
