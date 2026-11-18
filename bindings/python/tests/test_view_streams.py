"""Owned streaming preserves per-record tail rules across explicit batch sizes."""

import gc
import threading
import time
import unittest
from tempfile import TemporaryDirectory

import plasmite


class ViewStreams(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory()
        self.client = plasmite.Client(self.directory.name)
        self.pool = self.client.create_pool("streams", 1024 * 1024)

    def tearDown(self):
        self.pool.close()
        self.client.close()
        self.directory.cleanup()

    def test_mixed_batches_iteration_and_independent_lifetimes(self):
        for i in range(9):
            self.pool.feed({"index": i, "nested": [i]})
        with self.pool.fetch_view(since=1) as stream:
            first = next(stream)
            middle = stream.next_batch(4, 1024 * 1024)
            rest = list(stream)
            self.assertEqual(
                [m.data["index"] for m in [first, *middle, *rest]], list(range(9))
            )
            child = middle[-1].data["nested"]
        del middle, first, rest
        self.pool.close()
        self.client.close()
        gc.collect()
        self.assertEqual(list(child), [4])

    def test_byte_limit_retry_and_poll(self):
        message = self.pool.feed_view({"index": 0})
        extent = len(memoryview(message))
        with self.pool.follow_view(since=1) as stream:
            with self.assertRaises(plasmite.PlasmiteError):
                stream.next_batch(16, extent - 1, 0)
            self.assertEqual([m.seq for m in stream.next_batch(16, extent, 0)], [1])
            self.assertEqual(stream.next_batch(16, extent, 0), [])
            self.pool.feed_view({"index": 1})
            self.assertEqual(next(stream).seq, 2)

    def test_byte_budget_ignores_records_before_requested_sequence(self):
        self.pool.feed_view({"padding": "x" * 65536})
        wanted = self.pool.feed_view({"index": 2})
        with self.pool.fetch_view(since=wanted.seq) as stream:
            result = stream.next_batch(16, len(memoryview(wanted)), 0)
            self.assertEqual([message.seq for message in result], [wanted.seq])

    def test_tags_count_accepted_records(self):
        for i in range(12):
            self.pool.feed_view({"index": i}, tags=["yes\0tag"] if i % 3 == 1 else [])
        with self.pool.fetch_view(since=1, tags=["yes\0tag"]) as stream:
            result = stream.next_batch(16, 1024 * 1024)
            # A batch takes available companions; filtering does not change sequence policy.
            result.extend(list(stream))
            self.assertEqual([m.data["index"] for m in result], [1, 4, 7, 10])
            with self.assertRaises(StopIteration):
                stream.next_batch(16, 1024 * 1024, 0)

    def test_poll_scans_available_filtered_records(self):
        self.pool.feed_view({"index": 0}, [])
        accepted = self.pool.feed_view({"index": 1}, ["keep"])
        with self.pool.follow_view(since=1, tags=["keep"]) as stream:
            self.assertEqual(
                [item.seq for item in stream.next_batch(16, 4096, 0)], [accepted.seq]
            )
            self.assertEqual(stream.next_batch(16, 4096, 0), [])

    def test_mixed_c_json_and_owner_reads_share_filtered_checkpoint(self):
        for i in range(4):
            self.pool.feed_view({"index": i}, ["keep"] if i % 2 else [])
        with self.pool.fetch_view(since=1, tags=["keep"]) as stream:
            status, raw = stream._stream.next_wait(0)
            self.assertEqual(status, 1)
            self.assertEqual(plasmite.parse_message(raw).seq, 2)
            self.assertEqual(next(stream).seq, 4)
            with self.assertRaises(StopIteration):
                next(stream)

    def test_wait_for_first_without_filling(self):
        with self.pool.follow_view(since=1) as stream:
            writer = threading.Thread(
                target=lambda: (time.sleep(0.02), self.pool.feed_view({"x": 7}))
            )
            writer.start()
            try:
                # With no fill wait, this returns the one record without waiting for15 more.
                result = stream.next_batch(16, 1024 * 1024, 1)
                self.assertEqual([m.data["x"] for m in result], [7])
            finally:
                writer.join()

    def test_concurrent_writers_keep_per_record_order(self):
        failures = []
        handles = [self.client.open_pool("streams") for _ in range(2)]

        def publish(writer, handle):
            try:
                for i in range(50):
                    handle.feed_view({"writer": writer, "index": i})
            except BaseException as error:
                failures.append(error)

        writers = [
            threading.Thread(target=publish, args=(i, handle))
            for i, handle in enumerate(handles)
        ]
        try:
            with self.pool.follow_view(since=1) as stream:
                for writer in writers:
                    writer.start()
                messages = []
                while len(messages) < 100:
                    batch = stream.next_batch(4, 4096, wait=5)
                    self.assertTrue(batch, "writer made no progress")
                    messages.extend(batch)
                self.assertEqual(
                    [message.seq for message in messages], list(range(1, 101))
                )
                self.assertEqual(
                    {
                        (message.data["writer"], message.data["index"])
                        for message in messages
                    },
                    {(writer, i) for writer in range(2) for i in range(50)},
                )
        finally:
            for writer in writers:
                if writer.ident is not None:
                    writer.join(5)
            for handle in handles:
                handle.close()
        self.assertFalse(failures)
        self.assertTrue(all(not writer.is_alive() for writer in writers))

    def test_cancel_deadline_and_bounds(self):
        for key, value in [
            ("max_count", 0),
            ("max_bytes", 0),
            ("max_count", -1),
            ("max_bytes", 1 << 100),
        ]:
            with self.subTest(key=key, value=value), self.pool.follow_view(since="oldest") as stream:
                args = {"max_count": 1, "max_bytes": 1000, key: value}
                with self.assertRaises(ValueError):
                    stream.next_batch(**args)
        with self.pool.follow_view(since="oldest") as stream:
            self.assertEqual(stream.next_batch(4, 1000, wait=0), [])
        with self.pool.follow_view(since="oldest") as stream:
            stream.cancel()
            with self.assertRaises(StopIteration):
                stream.next_batch(4, 1000)

    def test_cancel_blocked_read_and_no_hidden_prefetch(self):
        with self.pool.follow_view(since=1) as stream:
            outcome = []

            def wait():
                try:
                    stream.next_batch(16, 1024 * 1024)
                except StopIteration:
                    outcome.append("cancelled")

            reader = threading.Thread(target=wait)
            reader.start()
            time.sleep(0.02)
            stream.cancel()
            reader.join(2)
            self.assertFalse(reader.is_alive())
            self.assertEqual(outcome, ["cancelled"])
        small = self.client.create_pool("overrun", 64 * 1024)
        try:
            for i in range(3):
                small.feed_view({"index": i})
            with small.follow_view(since=1) as stream:
                retained = next(stream)
                self.assertEqual(retained.seq, 1)
                for i in range(100):
                    small.feed_view({"index": i, "text": "x" * 1000})
                self.assertEqual(retained.data["index"], 0)
                # The iterator did not secretly acquire the old second record.
                with self.assertRaises(plasmite.PlasmiteError):
                    next(stream)
        finally:
            small.close()

    def test_event_cancellation_after_acquisition(self):
        from unittest.mock import patch
        from plasmite import _views

        cancel = threading.Event()
        message = self.pool.feed_view({"x": 1})
        with self.pool.follow_view(since="oldest", cancel=cancel) as stream:

            def acquire(*args):
                cancel.set()
                return 1, [message]

            with patch.object(_views.backend, "next_batch", acquire):
                with self.assertRaises(StopIteration):
                    next(stream)

    def test_argument_close_before_handle_borrow(self):
        with self.pool.follow_view(since="oldest") as stream:

            class Count(int):
                def __int__(self):
                    stream.close()
                    return 1

            with self.assertRaises(StopIteration):
                stream.next_batch(Count(1), 1000, 0)


if __name__ == "__main__":
    unittest.main()
