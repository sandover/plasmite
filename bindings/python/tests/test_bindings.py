"""
Purpose: Validate Python binding behaviors for payloads, tailing, and lifecycle.
Key Exports: None (unittest module).
Role: Exercise edge cases beyond conformance manifests.
Invariants: Requires libplasmite to be discoverable.
Notes: Uses temporary directories and avoids global state.
"""

from __future__ import annotations

import json
import os
import unittest
from contextlib import contextmanager
from unittest import mock
from pathlib import Path
from typing import Any, Iterator


REPO_ROOT = Path(__file__).resolve().parents[3]
LIB_DIR = os.environ.get("PLASMITE_LIB_DIR") or str(REPO_ROOT / "target" / "debug")
if "PLASMITE_LIB_DIR" not in os.environ:
    os.environ["PLASMITE_LIB_DIR"] = LIB_DIR

import plasmite
from plasmite import (
    AlreadyExistsError,
    Client,
    Durability,
    ErrorKind,
    Lite3Frame,
    Message,
    MessageMeta,
    NotFoundError,
    PlasmiteError,
    parse_message,
)

TEST_POOL_SIZE_BYTES = 1024 * 1024


class BindingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = Path(self._tempdir())
        self.pool_dir = self.temp / "pools"
        self.pool_dir.mkdir(parents=True, exist_ok=True)

    def _tempdir(self) -> str:
        import tempfile

        return tempfile.mkdtemp(prefix="plasmite-py-")

    def _new_client(self) -> Client:
        return Client(str(self.pool_dir))

    def _new_pool(self, client: Client, name: str) -> Any:
        return client.create_pool(name, TEST_POOL_SIZE_BYTES)

    @contextmanager
    def _client_pool(self, name: str) -> Iterator[tuple[Client, Any]]:
        client = self._new_client()
        pool = self._new_pool(client, name)
        try:
            yield client, pool
        finally:
            pool.close()
            client.close()

    def test_append_get_large_payload(self) -> None:
        with self._client_pool("big") as (_, pool):
            payload = {"blob": "x" * (64 * 1024)}
            tags = ["alpha", "beta", "gamma"]
            msg_bytes = pool.append_json(
                json.dumps(payload).encode("utf-8"), tags, Durability.FAST
            )
            message = parse_message(msg_bytes)
            self.assertIsInstance(message, Message)
            self.assertIsInstance(message.meta, MessageMeta)
            self.assertEqual(len(message.data["blob"]), len(payload["blob"]))
            self.assertEqual(message.meta.tags, tags)
            self.assertEqual(message.tags, tags)
            self.assertEqual(message.raw, msg_bytes)
            self.assertIsNotNone(message.time.tzinfo)

            get_bytes = pool.get_json(message.seq)
            fetched = parse_message(get_bytes)
            self.assertEqual(len(fetched.data["blob"]), len(payload["blob"]))
            get_alias = pool.get(message.seq)
            self.assertIsInstance(get_alias, Message)
            self.assertEqual(get_alias.seq, fetched.seq)

    def test_default_pool_size_constant_matches_core_default(self) -> None:
        self.assertEqual(plasmite.DEFAULT_POOL_SIZE_BYTES, 1024 * 1024)

    def test_client_pool_creates_then_reopens(self) -> None:
        with self._new_client() as client:
            first = client.pool("work", TEST_POOL_SIZE_BYTES)
            second = client.pool("work", 2 * TEST_POOL_SIZE_BYTES)
            first_msg = first.append({"kind": "one"}, ["alpha"])
            second_msg = second.get(first_msg.seq)
            self.assertEqual(second_msg.data["kind"], "one")
            first.close()
            second.close()

    def test_default_client_creates_pool_dir_in_fresh_home(self) -> None:
        import tempfile

        home = Path(tempfile.mkdtemp(prefix="plasmite-py-home-"))
        expected_pool_dir = home / ".plasmite" / "pools"
        expected_pool_path = expected_pool_dir / "work.plasmite"
        self.assertFalse(expected_pool_dir.exists())

        with mock.patch.dict(os.environ, {"HOME": str(home)}, clear=False):
            client = Client()
            pool = client.pool("work", TEST_POOL_SIZE_BYTES)
            msg = pool.append({"kind": "one"}, ["alpha"])
            self.assertEqual(msg.data["kind"], "one")
            pool.close()
            client.close()

        self.assertTrue(expected_pool_path.exists())

    def test_tail_timeout_and_close(self) -> None:
        with self._client_pool("tail") as (_, pool):
            stream = pool.open_stream(since_seq=9999, max_messages=1, timeout_ms=10)
            self.assertIsNone(stream.next_json())
            stream.close()
            stream.close()

    def test_stream_and_lite3_stream_iterators(self) -> None:
        with self._client_pool("iter") as (_, pool):
            first = pool.append({"kind": "one"}, ["iter"])
            pool.append({"kind": "two"}, ["iter"])
            pool.append({"kind": "three"}, ["iter"])

            stream = pool.open_stream(
                since_seq=first.seq, max_messages=3, timeout_ms=50
            )
            seen = [parse_message(raw).data["kind"] for raw in stream]
            self.assertEqual(seen, ["one", "two", "three"])
            self.assertIsNone(stream.next_json())
            stream.close()

            frame_seed = pool.get_lite3(first.seq)
            seq2 = pool.append_lite3(frame_seed.payload, Durability.FAST)
            pool.append_lite3(frame_seed.payload, Durability.FAST)

            lite3_stream = pool.open_lite3_stream(
                since_seq=seq2, max_messages=2, timeout_ms=50
            )
            frame_seqs = [frame.seq for frame in lite3_stream]
            self.assertEqual(frame_seqs, [seq2, seq2 + 1])
            self.assertIsNone(lite3_stream.next())
            lite3_stream.close()

    def test_tail_filters_by_tags(self) -> None:
        with self._client_pool("tail-tags") as (_, pool):
            pool.append_json(b'{"kind":"drop"}', ["drop"], Durability.FAST)
            pool.append_json(b'{"kind":"keep"}', ["keep"], Durability.FAST)

            results = list(pool.tail(max_messages=1, timeout_ms=50, tags=["keep"]))
            self.assertEqual(len(results), 1)
            message = results[0]
            self.assertIsInstance(message, Message)
            self.assertEqual(message.data["kind"], "keep")

    def test_replay_filters_by_tags_before_max_messages(self) -> None:
        with self._client_pool("replay-tags") as (_, pool):
            pool.append_json(b'{"kind":"drop"}', ["drop"], Durability.FAST)
            pool.append_json(b'{"kind":"keep"}', ["keep"], Durability.FAST)

            results = list(
                pool.replay(speed=1.0, max_messages=1, timeout_ms=50, tags=["keep"])
            )
            self.assertEqual(len(results), 1)
            message = results[0]
            self.assertIsInstance(message, Message)
            self.assertEqual(message.data["kind"], "keep")

    def test_tail_no_tags_skips_tag_filter_helper(self) -> None:
        with self._client_pool("tail-no-tags-fast-path") as (_, pool):
            pool.append_json(b'{"kind":"keep"}', ["keep"], Durability.FAST)

            with mock.patch(
                "plasmite._message_has_tags",
                side_effect=AssertionError("tail should bypass tag helper without tags"),
            ):
                results = list(pool.tail(max_messages=1, timeout_ms=50))
            self.assertEqual(len(results), 1)
            self.assertEqual(results[0].data["kind"], "keep")

    def test_replay_no_tags_skips_tag_filter_helper(self) -> None:
        with self._client_pool("replay-no-tags-fast-path") as (_, pool):
            pool.append_json(b'{"kind":"keep"}', ["keep"], Durability.FAST)

            with mock.patch(
                "plasmite._message_has_tags",
                side_effect=AssertionError("replay should bypass tag helper without tags"),
            ):
                results = list(pool.replay(speed=1.0, max_messages=1, timeout_ms=50))
            self.assertEqual(len(results), 1)
            self.assertEqual(results[0].data["kind"], "keep")

    def test_closed_handles_error(self) -> None:
        client = self._new_client()
        pool = self._new_pool(client, "closed")
        pool.close()

        with self.assertRaises(PlasmiteError):
            pool.append_json(b"{}", [], Durability.FAST)

        client.close()
        with self.assertRaises(PlasmiteError):
            client.create_pool("oops", TEST_POOL_SIZE_BYTES)

    def test_lite3_append_get_stream(self) -> None:
        with self._client_pool("lite3") as (_, pool):
            msg_bytes = pool.append_json(
                json.dumps({"x": 1}).encode("utf-8"), ["alpha"], Durability.FAST
            )
            message = parse_message(msg_bytes)
            frame = pool.get_lite3(message.seq)
            self.assertIsInstance(frame, Lite3Frame)
            self.assertIsNotNone(frame.time.tzinfo)
            self.assertGreater(len(frame.payload), 0)

            seq2 = pool.append_lite3(frame.payload, Durability.FAST)
            frame2 = pool.get_lite3(seq2)
            self.assertEqual(frame2.payload, frame.payload)

            stream = pool.open_lite3_stream(
                since_seq=seq2, max_messages=1, timeout_ms=50
            )
            next_frame = stream.next()
            self.assertIsNotNone(next_frame)
            assert next_frame is not None
            self.assertEqual(next_frame.seq, seq2)
            self.assertEqual(next_frame.payload, frame.payload)
            self.assertIsNone(stream.next())
            stream.close()

    def test_lite3_invalid_payload(self) -> None:
        with self._client_pool("lite3-bad") as (_, pool):
            with self.assertRaises(PlasmiteError):
                pool.append_lite3(b"\x01", Durability.FAST)

    def test_context_manager_lifecycle(self) -> None:
        with self._new_client() as client:
            with client.create_pool("ctx", TEST_POOL_SIZE_BYTES) as pool:
                msg = pool.append_json(b'{"x":1}', [], Durability.FAST)
                parsed = parse_message(msg)
                with pool.open_stream(
                    since_seq=parsed.seq, max_messages=1, timeout_ms=50
                ) as stream:
                    self.assertIsNotNone(stream.next_json())

        with self.assertRaises(PlasmiteError):
            client.create_pool("after-close", TEST_POOL_SIZE_BYTES)

    def test_validation_errors_are_deterministic(self) -> None:
        with self._new_client() as client:
            pool = client.create_pool("validation", TEST_POOL_SIZE_BYTES)
            with self.assertRaises(ValueError):
                pool.append_json(b"", [], Durability.FAST)
            invalid_payload = "{}"
            with self.assertRaises(TypeError):
                pool.append_json(invalid_payload, [], Durability.FAST)
            with self.assertRaises(ValueError):
                pool.get_json(-1)
            pool.close()
            with self.assertRaises(PlasmiteError):
                pool.get_json(1)

    def test_error_subclasses_are_raised(self) -> None:
        with self._new_client() as client:
            self.assertIs(plasmite._ERROR_KIND_TO_CLASS[ErrorKind.ALREADY_EXISTS], AlreadyExistsError)
            with self.assertRaises(NotFoundError):
                client.open_pool("missing")


if __name__ == "__main__":
    unittest.main()
