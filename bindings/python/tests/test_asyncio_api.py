import asyncio
import gc
import os
import socket
import subprocess
import sys
import threading
import unittest
import weakref
from contextlib import AsyncExitStack
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from plasmite import Message, NotFoundError, RetentionGapError, UsageError, aio


class AsyncReadDeadlineTests(unittest.IsolatedAsyncioTestCase):
    async def test_udp_read_deadline_includes_opening(self):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as silent:
            silent.bind(("127.0.0.1", 0))
            async with aio.UdpClient(f"udp://127.0.0.1:{silent.getsockname()[1]}") as client:
                async with client.open_pool("events") as pool:
                    for wait in (0, 1e-9, 0.01):
                        with self.subTest(wait=wait):
                            reader = pool.follow()
                            try:
                                self.assertIsNone(await asyncio.wait_for(reader.next(wait), 0.5))
                            finally:
                                await reader.aclose()


class NetworkResultTests(unittest.TestCase):
    def test_closing_stopped_loop_releases_queued_message(self):
        loop = asyncio.new_event_loop()
        token = None

        def start(result_token, _report, _out, _error):
            nonlocal token
            token = result_token
            return 0

        async def open_job():
            return aio._NetworkJob(start)

        try:
            job = loop.run_until_complete(open_job())
            with patch.object(aio._LIB, "plsm_view_release") as release:
                aio._network_result(token, 1, 123, None)
                self.assertFalse(job.future.done())
                loop.close()
                release.assert_called_once_with(123)
        finally:
            loop.close()


class LocalAsyncTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        scope = AsyncExitStack()
        self.addAsyncCleanup(scope.aclose)
        directory = scope.enter_context(TemporaryDirectory())
        self.client = await scope.enter_async_context(aio.Client(directory))
        self.pool = await scope.enter_async_context(self.client.create_pool("events", 65536))

    async def test_follow_tags_and_fetch_freeze_their_positions_on_open(self):
        follow = self.pool.follow(tags=["keep"])
        await self.pool.feed({"index": 0}, tags=["keep"])
        async with follow:
            fetch = self.pool.fetch(last=2)
            await self.pool.feed({"index": 1})
            async with fetch:
                sent = await self.pool.feed({"index": 2}, tags=["keep"], durability="flush")
                received = await asyncio.wait_for(follow.next(), 2)
                self.assertIsInstance(received, Message)
                self.assertEqual(received.raw, sent.raw)
                self.assertEqual([message.seq async for message in fetch], [1, 2])
        self.assertEqual([m.field("index") async for m in self.pool.fetch(tags=["keep"])], [0, 2])

    async def test_blocked_opens_leave_workers_for_an_open_follower(self):
        async with self.client.create_pool("locked", 65536) as locked, self.pool.follow() as follower:
            locked_native = await locked._get_pool()
            loop = asyncio.get_running_loop()
            started = asyncio.Queue()
            release = threading.Event()
            original = aio._open_reader

            def blocked_open(pool, *args):
                if pool is locked_native:
                    loop.call_soon_threadsafe(started.put_nowait, None)
                    if not release.wait(5):
                        raise AssertionError("test never released the opening worker")
                return original(pool, *args)

            readers = [locked.follow() for _ in range(aio._workers._max_workers)]
            with patch.object(aio, "_open_reader", blocked_open):
                opening = [asyncio.create_task(reader.__aenter__()) for reader in readers]
                try:
                    for _ in readers:
                        await asyncio.wait_for(started.get(), 2)
                    sent = await self.pool.feed({"ready": True})
                    received = await asyncio.wait_for(follower.next(), 2)
                    self.assertEqual(received.seq, sent.seq)
                finally:
                    release.set()
                    await asyncio.gather(*opening, return_exceptions=True)
                    await asyncio.gather(*(reader.aclose() for reader in readers))

    async def test_wait_expiry_concurrent_reads_and_close(self):
        async with self.pool.follow() as reader:
            self.assertIsNone(await reader.next(0))
            self.assertIsNone(await reader.next(0.01))
            pending = asyncio.create_task(reader.next())
            await asyncio.sleep(0)
            with self.assertRaises(UsageError):
                await anext(reader)
            first = await self.pool.feed({"index": 1})
            self.assertEqual((await asyncio.wait_for(pending, 2)).seq, first.seq)
            pending = asyncio.create_task(reader.next())
            await asyncio.sleep(0)
            await reader.aclose()
            with self.assertRaises(StopAsyncIteration):
                await asyncio.wait_for(pending, 2)

    async def test_cancelled_read_preserves_message_already_pulled(self):
        await self.pool.feed({"index": 1})
        pulled = asyncio.Event()
        ready = threading.Event()
        loop = asyncio.get_running_loop()
        original = aio._poll

        def hold_result(reader):
            message = original(reader)
            loop.call_soon_threadsafe(pulled.set)
            if not ready.wait(3):
                raise AssertionError("test never released the worker")
            return message

        async with self.pool.follow(since=1) as reader:
            with patch.object(aio, "_poll", hold_result):
                task = asyncio.create_task(reader.next())
                try:
                    await asyncio.wait_for(pulled.wait(), 2)
                    task.cancel()
                    with self.assertRaises(asyncio.CancelledError):
                        await task
                finally:
                    ready.set()
            self.assertEqual((await asyncio.wait_for(reader.next(), 2)).seq, 1)

    async def test_retention_gap_survives_cancelled_wait(self):
        async with self.pool.follow() as reader:
            task = asyncio.create_task(reader.next())
            await asyncio.sleep(0)
            task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await task
            for _ in range(100):
                await self.pool.feed({"fill": "x" * 2048})
            with self.assertRaises(RetentionGapError):
                await asyncio.wait_for(reader.next(), 2)

    @unittest.skipUnless(sys.implementation.name == "cpython", "reference counting test")
    async def test_break_releases_a_temporary_reader(self):
        await self.pool.feed({})
        opened, closed = [], threading.Event()
        original_open = aio._open_reader

        def tracked_open(*args):
            native = original_open(*args)
            original_close = native.close

            def tracked_close():
                original_close()
                closed.set()

            native.close = tracked_close
            return native

        def follow():
            reader = self.pool.follow(since=1)
            opened.append(weakref.ref(reader))
            return reader

        with patch.object(aio, "_open_reader", tracked_open):
            async for _ in follow():
                break
        self.assertIsNone(opened[0]())
        self.assertTrue(await asyncio.to_thread(closed.wait, 2))

    async def test_many_quiet_followers_receive_the_same_write(self):
        readers = await asyncio.gather(*(self.pool.follow().__aenter__() for _ in range(100)))
        try:
            waits = [asyncio.create_task(reader.next()) for reader in readers]
            await asyncio.sleep(0.02)
            first = await self.pool.feed({"ready": True})
            results = await asyncio.wait_for(asyncio.gather(*waits), 4)
            self.assertEqual([result.seq for result in results], [first.seq] * len(readers))
        finally:
            await asyncio.gather(*(reader.aclose() for reader in readers))

    async def test_waiting_reader_reports_pool_deletion(self):
        async with self.pool.follow() as reader:
            pending = asyncio.create_task(reader.next())
            await asyncio.sleep(0)
            await self.client.delete_pool("events")
            with self.assertRaises(NotFoundError):
                await asyncio.wait_for(pending, 3)


class NetworkAsyncTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        root = Path(__file__).resolve().parents[3]
        binary = Path(os.environ.get("PLASMITE_BIN", root / "target" / "debug" / "plasmite"))
        if not binary.exists():
            self.skipTest("build the Plasmite CLI to exercise network transports")
        self.binary = str(binary)
        scope = AsyncExitStack()
        self.addAsyncCleanup(scope.aclose)
        directory = Path(scope.enter_context(TemporaryDirectory()))
        self.pool_dir = directory / "pools"
        self.pool_dir.mkdir()
        access = directory / "access"
        access.mkdir()
        scope.enter_context(patch.dict(os.environ, {"PLASMITE_ACCESS_HOME": str(access)}))
        self.client = await scope.enter_async_context(aio.Client(str(self.pool_dir)))
        self.local = await scope.enter_async_context(self.client.create_pool("events", 65536))
        ready = directory / "ready"
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as held:
            held.bind(("127.0.0.1", 0))
            self.udp = held.getsockname()[1]
        self.server = await asyncio.create_subprocess_exec(
            self.binary, "--dir", str(self.pool_dir), "serve", "--bind", "127.0.0.1:0",
            "--remote-bind", "127.0.0.1:0", f"--udp=127.0.0.1:{self.udp}", "--udp-follow",
            "--shared-address", "https://localhost:9743",
            env={**os.environ, "PLASMITE_SERVE_READY_FILE": str(ready)},
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
        )
        self.addAsyncCleanup(self.stop_server)

        async def ready_address():
            while not ready.exists() or not ready.read_text().strip():
                if self.server.returncode is not None:
                    self.fail((await self.server.stderr.read()).decode())
                await asyncio.sleep(0.02)
            return ready.read_text().strip()

        address = await asyncio.wait_for(ready_address(), 10)
        self.origin = "https://localhost:" + address.rsplit(":", 1)[-1]

    async def stop_server(self):
        if self.server.returncode is None:
            self.server.terminate()
        try:
            await asyncio.wait_for(self.server.communicate(), 5)
        except asyncio.TimeoutError:
            self.server.kill()
            await self.server.communicate()

    async def test_https_feed_follow_and_finite_fetch(self):
        import json

        invite = await asyncio.create_subprocess_exec(
            self.binary, "--dir", str(self.pool_dir), "access", "invite", "async-test",
            "--format", "json", stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        output, error = await invite.communicate()
        self.assertEqual(invite.returncode, 0, error.decode())
        key = json.loads(output)["access_key"]
        connect = await asyncio.create_subprocess_exec(
            self.binary, "access", "connect", self.origin, stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
        )
        _, error = await connect.communicate((key + "\n").encode())
        self.assertEqual(connect.returncode, 0, error.decode())

        async with self.client.open_pool(self.origin + "/events") as pool:
            with self.assertRaises(OverflowError):
                await pool.feed({"n": 2**63 + 1})
            async with pool.follow(tags=["keep"]) as reader:
                self.assertIsNone(await reader.next(0.02))
                sent = await pool.feed({"value": b"binary"}, ["keep"], durability="flush")
                message = await reader.next(3)
                self.assertEqual(message.raw, sent.raw)
                self.assertEqual(message.sender, "async-test")
            self.assertEqual([m.seq async for m in pool.fetch(last=1)], [sent.seq])
            await pool.feed(sent)

    async def test_udp_feed_follow_and_finite_fetch(self):
        async with aio.UdpClient(f"udp://127.0.0.1:{self.udp}") as client:
            async with client.open_pool("events") as pool, pool.follow() as reader:
                self.assertIsNone(await reader.next(0.02))
                self.assertIsNone(await pool.feed({"n": 1}))
                message = await reader.next(3)
                self.assertEqual(message.data, {"n": 1})
                self.assertEqual([m.seq async for m in pool.fetch(last=1)], [message.seq])
