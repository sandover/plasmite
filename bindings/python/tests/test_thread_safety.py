"""Exercise shared Python handles and close during native operations."""

from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
from tempfile import TemporaryDirectory
from threading import Barrier, Event
import unittest
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parents[3]
os.environ.setdefault("PLASMITE_LIB_DIR", str(REPO_ROOT / "target" / "debug"))

import plasmite


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
                appended = self.pool.append(value)
                self.assertEqual(self.pool.get(appended.seq).data, value)
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
            self.assertEqual(opened.append({"survives": "client close"}).data,
                             {"survives": "client close"})
        finally:
            opened.close()
        with self.assertRaises(plasmite.PlasmiteError):
            self.client.open_pool("threads")

    def test_pool_close_waits_for_get_and_owned_result(self):
        message = self.pool.append({"owned": True})
        original = plasmite._buf_to_bytes
        payload = self._assert_close_waits(
            self.pool, lambda: self.pool.get_json(message.seq),
            "plasmite._buf_to_bytes", "plsm_pool_free", original,
        )
        self.assertEqual(plasmite.parse_message(payload).data, {"owned": True})
        with self.assertRaises(plasmite.PlasmiteError):
            self.pool.get(message.seq)

    def test_stream_close_waits_for_native_next(self):
        cases = (
            (self.pool.open_stream, "next_json", "plsm_stream_next", "plsm_stream_free"),
            (self.pool.open_lite3_stream, "next", "plsm_lite3_stream_next", "plsm_lite3_stream_free"),
        )
        for open_stream, method, native_next, native_free in cases:
            with self.subTest(method=method):
                stream = open_stream(timeout_ms=20)
                self.addCleanup(stream.close)
                result = self._assert_close_waits(
                    stream, getattr(stream, method), "plasmite._LIB." + native_next,
                    native_free, lambda *_: 0,
                )
                self.assertIsNone(result)
                with self.assertRaises(plasmite.PlasmiteError):
                    getattr(stream, method)()

    def test_stream_close_waits_for_result_copy_and_free(self):
        message = self.pool.append({"copy": "before close"})
        cases = (
            (self.pool.open_stream, "next_json", "_buf_to_bytes", "plsm_stream_free"),
            (self.pool.open_lite3_stream, "next", "_frame_to_py", "plsm_lite3_stream_free"),
        )
        for open_stream, method, converter, native_free in cases:
            with self.subTest(method=method):
                stream = open_stream(since_seq=message.seq, max_messages=1, timeout_ms=20)
                self.addCleanup(stream.close)
                result = self._assert_close_waits(
                    stream, getattr(stream, method), "plasmite." + converter,
                    native_free, getattr(plasmite, converter),
                )
                if method == "next_json":
                    self.assertEqual(plasmite.parse_message(result).data, message.data)
                else:
                    self.assertEqual(result.seq, message.seq)
                    self.assertTrue(result.payload)

    def test_tail_yield_does_not_hold_pool_lock(self):
        first = self.pool.append({"index": 1})
        tail = self.pool.tail(since_seq=first.seq, max_messages=2, timeout_ms=50)
        self.addCleanup(tail.close)
        self.assertEqual(next(tail).data, first.data)
        with ThreadPoolExecutor(max_workers=1) as threads:
            second = threads.submit(self.pool.append, {"index": 2}).result(timeout=5)
            threads.submit(self.pool.close).result(timeout=5)
        # The stream remains usable after its originating Pool handle closes.
        self.assertEqual(next(tail).data, second.data)
        with self.assertRaises(StopIteration):
            next(tail)

    def test_replay_sleep_does_not_hold_pool_lock(self):
        self.pool.append({"index": 1})
        self.pool.append({"index": 2})
        replay = self.pool.replay(max_messages=2, timeout_ms=50)
        self.addCleanup(replay.close)
        next(replay)
        entered, release = Event(), Event()

        def pause(_delay):
            entered.set()
            if not release.wait(timeout=5):
                raise TimeoutError("test did not release replay delay")

        with mock.patch("plasmite._time.sleep", side_effect=pause), ThreadPoolExecutor(
            max_workers=2
        ) as threads:
            replaying = threads.submit(next, replay)
            try:
                self.assertTrue(entered.wait(timeout=5))
                threads.submit(self.pool.append, {"index": 3}).result(timeout=5)
            finally:
                release.set()
                self.assertEqual(replaying.result(timeout=5).data, {"index": 2})

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


if __name__ == "__main__":
    unittest.main()
