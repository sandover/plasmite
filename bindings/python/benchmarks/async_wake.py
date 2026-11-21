"""Compare two asyncio handoffs using local Plasmite readers.

Run from the repository root with PLASMITE_LIB_DIR and PYTHONPATH set.
Both handoffs use the same native wait on one pool file.
"""

import argparse
import asyncio
import queue
import socket
import statistics
import tempfile
import threading
import time
from contextlib import ExitStack
from ctypes import CDLL, c_char_p, c_int, c_void_p
from pathlib import Path

from plasmite import Client


async def measure(native, count, mode, samples):
    loop = asyncio.get_running_loop()
    waiting = asyncio.Event()
    stop = threading.Event()
    pending = queue.SimpleQueue()

    with tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
        client = stack.enter_context(Client(directory))
        pool = stack.enter_context(client.create_pool("events"))
        readers = [stack.enter_context(pool.follow()) for _ in range(count)]
        handle = native.probe_open(str(Path(directory) / "events.plasmite").encode())
        if not handle:
            raise RuntimeError("native pool notification is unavailable")
        stack.callback(native.probe_close, handle)
        if mode == "socket":
            incoming, outgoing = socket.socketpair()
            stack.enter_context(incoming)
            stack.enter_context(outgoing)
            incoming.setblocking(False)

        def signal():
            if mode == "callback":
                loop.call_soon_threadsafe(waiting.set)
            else:
                outgoing.sendall(b"\x00")

        def scan():
            newest = None
            try:
                while not stop.is_set():
                    outcome = native.probe_wait(handle)
                    if outcome < 0:
                        raise RuntimeError("native pool notification failed")
                    if stop.is_set():
                        return
                    # Check retained records after a signal or the same
                    # one-second recovery interval the server uses.
                    if outcome == 0 and pool.info()["bounds"].get("newest") == newest:
                        continue
                    ready = []
                    for index, reader in enumerate(readers):
                        message = reader.next(wait=0)
                        if message is not None:
                            ready.append((index, message.seq))
                    if ready:
                        newest = max(seq for _, seq in ready)
                        pending.put((outcome, ready))
                        signal()
            except Exception as error:
                pending.put(error)
                signal()

        before_threads = threading.active_count()
        worker = threading.Thread(target=scan, name="pool-reader-probe")
        worker.start()
        try:
            cpu_started, idle_started = time.process_time(), time.monotonic()
            await asyncio.sleep(5)
            idle_cpu = 100 * (time.process_time() - cpu_started) / (time.monotonic() - idle_started)
            added_threads = threading.active_count() - before_threads
            samples_ms = []
            recovered = 0
            for seq in range(1, samples + 1):
                started = time.monotonic()
                await asyncio.to_thread(pool.feed, {"value": seq})
                received = set()
                while len(received) < count:
                    if mode == "callback":
                        await waiting.wait()
                        waiting.clear()
                    else:
                        assert await loop.sock_recv(incoming, 4096)
                    while not pending.empty():
                        batch = pending.get()
                        if isinstance(batch, Exception):
                            raise batch
                        outcome, records = batch
                        if outcome == 0:
                            recovered += len(records)
                        for index, actual in records:
                            assert actual == seq and index not in received
                            received.add(index)
                samples_ms.append((time.monotonic() - started) * 1000)
            p95 = statistics.quantiles(samples_ms, n=100, method="inclusive")[94]
            print(f"{count:>3} {mode:>8} {added_threads:>3} {idle_cpu:>8.3f} {p95:>8.3f} {recovered:>9}")
        finally:
            stop.set()
            worker.join()


async def main(native, samples):
    print("readers  handoff  threads  idle CPU %  p95 ms  recovered")
    for count in (1, 100):
        for mode in ("callback", "socket"):
            await asyncio.wait_for(measure(native, count, mode, samples), timeout=20)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=int, default=100)
    parser.add_argument("--native", type=Path, required=True, help="compiled native wake probe")
    args = parser.parse_args()
    if args.samples < 2:
        parser.error("--samples must be at least 2")
    native = CDLL(str(args.native.resolve()))
    native.probe_open.argtypes = [c_char_p]
    native.probe_open.restype = c_void_p
    native.probe_wait.argtypes = [c_void_p]
    native.probe_wait.restype = c_int
    native.probe_close.argtypes = [c_void_p]
    native.probe_close.restype = None
    asyncio.run(main(native, args.samples))
