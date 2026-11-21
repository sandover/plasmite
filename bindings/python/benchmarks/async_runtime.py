"""Measure the installed asyncio API with quiet readers and local writes."""

import argparse
import asyncio
import json
import os
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import AsyncExitStack

from plasmite import Client, aio


def threads():
    if sys.platform == "darwin":
        rows = subprocess.check_output(["ps", "-M", "-p", str(os.getpid())]).splitlines()
        return len(rows) - 1
    if sys.platform == "linux":
        return len(os.listdir("/proc/self/task"))
    return None


def p95(samples):
    return statistics.quantiles(samples, n=100, method="inclusive")[94]


async def measure(readers, pools, seconds, samples, contention):
    with tempfile.TemporaryDirectory() as directory, Client(directory) as sync_client:
        async with AsyncExitStack() as stack:
            client = await stack.enter_async_context(aio.Client(directory))
            baseline_threads = threads()
            capacity = 1024 * 1024 if contention else 65536
            opened = []
            for index in range(pools):
                opened.append(await stack.enter_async_context(client.create_pool(f"pool-{index}", capacity)))
            followers = [
                await stack.enter_async_context(opened[index % pools].follow(tags=["probe"]))
                for index in range(readers)
            ]

            def read_all():
                return [asyncio.create_task(reader.next()) for reader in followers]

            waiting = read_all()
            await asyncio.sleep(0.1)
            quiet_threads = threads()
            idle_cpu_start, idle_wall_start = time.process_time(), time.monotonic()
            await asyncio.sleep(seconds)
            idle_cpu = 100 * (time.process_time() - idle_cpu_start) / (time.monotonic() - idle_wall_start)

            delay_ms, receipt_ms = [], []
            stop = threading.Event()

            async def heartbeat():
                loop = asyncio.get_running_loop()
                while True:
                    before = loop.time()
                    await asyncio.sleep(0.001)
                    delay_ms.append(max(0, (loop.time() - before) * 1000 - 1))

            def rival():
                with sync_client.open_pool("pool-0") as pool:
                    while not stop.is_set():
                        pool.feed({"other": 1})
                        stop.wait(0.0002)

            worker = (
                threading.Thread(target=rival, name="competing-writer") if contention else None
            )
            if worker is not None:
                worker.start()
            tick = asyncio.create_task(heartbeat())
            try:
                for sample in range(samples):
                    started = time.monotonic()
                    for pool in opened:
                        await pool.feed({"sample": sample}, tags=["probe"])
                    received = await asyncio.wait_for(asyncio.gather(*waiting), 5)
                    assert all(message.field("sample") == sample for message in received)
                    receipt_ms.append((time.monotonic() - started) * 1000)
                    if sample < samples - 1:
                        waiting = read_all()
                    await asyncio.sleep(0.001)
            finally:
                tick.cancel()
                await asyncio.gather(tick, return_exceptions=True)
                if worker is not None:
                    stop.set()
                    worker.join()
            return {
                "readers": readers,
                "pools": pools,
                "competing_writer": contention,
                "added_threads": None if quiet_threads is None else quiet_threads - baseline_threads,
                "idle_cpu_pct": round(idle_cpu, 3),
                "p95_last_receipt_ms": round(p95(receipt_ms), 3),
                "p95_loop_delay_ms": round(p95(delay_ms), 3),
                "max_loop_delay_ms": round(max(delay_ms), 3),
            }


async def main(args):
    for readers, pools, contention in (
        (1, 1, False), (100, 1, False), (100, 100, False), (100, 1, True),
    ):
        print(json.dumps(await measure(readers, pools, args.idle_seconds, args.samples, contention)))
        await asyncio.sleep(1.1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--idle-seconds", type=float, default=5)
    parser.add_argument("--samples", type=int, default=100)
    args = parser.parse_args()
    if args.idle_seconds <= 0 or args.samples < 2:
        parser.error("idle duration must be positive and samples must be at least 2")
    asyncio.run(main(args))
