"""Exercise shared Python handles and close during native operations."""

from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta
import os
from pathlib import Path
from tempfile import TemporaryDirectory
from threading import Barrier, Event
import unittest
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parents[3]
os.environ.setdefault("PLASMITE_LIB_DIR", str(REPO_ROOT / "target" / "debug"))

import plasmite
from plasmite import _views


class ThreadSafetyTests(unittest.TestCase):
    def setUp(self):
        self.temp = TemporaryDirectory(prefix="plasmite-py-threads-")
        self.addCleanup(self.temp.cleanup)
        self.client = plasmite.Client(self.temp.name)
        self.addCleanup(self.client.close)
        self.pool = self.client.create_pool("threads", 1024 * 1024)
        self.addCleanup(self.pool.close)

    def test_shared_pool_concurrent_append_and_get(self):
        ready = Barrier(2)

        def exchange(worker):
            ready.wait(timeout=5)
            sequences = []
            for index in range(100):
                value = {"worker": worker, "index": index}
                appended = self.pool.feed(value)
                with self.pool.fetch(since=appended.seq) as reader:
                    self.assertEqual(next(reader).data, value)
                sequences.append(appended.seq)
            return sequences

        with ThreadPoolExecutor(max_workers=2) as threads:
            jobs = [threads.submit(exchange, worker) for worker in range(2)]
            sequences = [seq for job in jobs for seq in job.result(timeout=10)]
        self.assertEqual(len(set(sequences)), 200)

    def _assert_close_waits(self, handle, operation, patch_target, native_free, block):
        """Hold a native call/result conversion while another thread closes it."""
        entered, release, closing, freed = (Event() for _ in range(4))
        original_free = getattr(plasmite._LIB, native_free)

        def blocked(*args, **kwargs):
            entered.set()
            if not release.wait(timeout=5):
                raise TimeoutError("test did not release native operation")
            return block(*args, **kwargs)

        def tracked_free(ptr):
            freed.set()
            return original_free(ptr)

        def close():
            closing.set()
            handle.close()

        with mock.patch(patch_target, side_effect=blocked), mock.patch.object(
            plasmite._LIB, native_free, side_effect=tracked_free
        ), ThreadPoolExecutor(max_workers=2) as threads:
            active = threads.submit(operation)
            closer = None
            try:
                self.assertTrue(entered.wait(timeout=5))
                closer = threads.submit(close)
                self.assertTrue(closing.wait(timeout=5))
                self.assertFalse(freed.wait(timeout=0.05), "handle freed during native use")
            finally:
                release.set()
                result = active.result(timeout=5)
                if closer is not None:
                    closer.result(timeout=5)
            self.assertTrue(freed.is_set())
            self.assertIsNone(handle._ptr)
            handle.close()
            self.assertEqual(getattr(plasmite._LIB, native_free).call_count, 1)
        return result

    def test_client_close_waits_for_open_pool(self):
        original = plasmite._LIB.plsm_pool_open
        opened = self._assert_close_waits(
            self.client, lambda: self.client.open_pool("threads"),
            "plasmite._LIB.plsm_pool_open", "plsm_client_free", original,
        )
        try:
            self.assertEqual(opened.feed({"survives": "client close"}).data,
                             {"survives": "client close"})
        finally:
            opened.close()
        with self.assertRaises(plasmite.PlasmiteError):
            self.client.open_pool("threads")

    def test_pool_close_waits_for_read_open(self):
        message = self.pool.feed({"owned": True})
        original = plasmite._LIB.plsm_pool_fetch
        reader = self._assert_close_waits(
            self.pool, lambda: self.pool.fetch(since=message.seq),
            "plasmite._LIB.plsm_pool_fetch", "plsm_pool_free", original,
        )
        with reader:
            self.assertEqual(next(reader).data, {"owned": True})
        with self.assertRaises(plasmite.PlasmiteError):
            self.pool.fetch(since=message.seq)

    def test_stream_close_waits_for_native_next(self):
        reader = self.pool.follow()
        native = reader._reader._stream
        self.addCleanup(reader.close)

        def cancelled(*_):
            raise StopIteration

        def pull():
            try:
                return reader.next(wait=1)
            except StopIteration:
                return None

        result = self._assert_close_waits(
            native, pull, "plasmite._views.backend.next_message",
            "plsm_stream_free", cancelled,
        )
        self.assertIsNone(result)
        with self.assertRaises(StopIteration):
            next(reader)

    def test_stream_close_waits_for_owned_native_result(self):
        message = self.pool.feed({"copy": "before close"})
        reader = self.pool.follow(since=message.seq)
        native = reader._reader._stream
        self.addCleanup(reader.close)
        original = _views.backend.next_message
        with mock.patch.object(plasmite._LIB, "plsm_stream_cancel"):
            received = self._assert_close_waits(
                native, lambda: next(reader), "plasmite._views.backend.next_message",
                "plsm_stream_free", original,
            )
        self.assertEqual(received.data, message.data)
        self.assertEqual(received.raw, message.raw)

    def test_tail_yield_does_not_hold_pool_lock(self):
        first = self.pool.feed({"index": 1})
        tail = self.pool.follow(since=first.seq)
        self.addCleanup(tail.close)
        self.assertEqual(next(tail).data, first.data)
        with ThreadPoolExecutor(max_workers=1) as threads:
            second = threads.submit(self.pool.feed, {"index": 2}).result(timeout=5)
            threads.submit(self.pool.close).result(timeout=5)
        # The stream remains usable after its originating Pool handle closes.
        self.assertEqual(next(tail).data, second.data)
        self.assertIsNone(tail.next(timedelta(0)))

    def test_failed_client_constructor_can_close_safely(self):
        client = plasmite.Client.__new__(plasmite.Client)
        with self.assertRaises(ValueError):
            client.__init__("")
        client.close()
        self.assertIsNone(client._ptr)
        with mock.patch.object(plasmite._LIB, "plsm_client_new", return_value=-1):
            with self.assertRaises(plasmite.PlasmiteError):
                client.__init__(self.temp.name)
        client.close()
        self.assertIsNone(client._ptr)

    def test_bounded_waits_keep_position_and_allow_concurrent_cancel(self):
        with self.pool.follow(since=1) as reader:
            self.assertIsNone(reader.next(0))
            self.pool.feed({"after_wait": True})
            self.assertEqual(reader.next(0).seq, 1)
            self.assertIsNone(reader.next(0))

        with self.pool.follow() as reader, ThreadPoolExecutor(max_workers=1) as threads:
            pending = threads.submit(next, reader, None)
            reader.cancel()
            self.assertIsNone(pending.result(timeout=1))

    def test_tail_event_cancels_a_quiet_wait(self):
        cancel = Event()
        with self.pool.follow(cancel=cancel) as reader, ThreadPoolExecutor(max_workers=1) as threads:
            pending = threads.submit(list, reader)
            cancel.set()
            self.assertEqual(pending.result(timeout=1), [])


if __name__ == "__main__":
    unittest.main()
