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
from unittest import mock
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
LIB_DIR = os.environ.get("PLASMITE_LIB_DIR") or str(REPO_ROOT / "target" / "debug")
if "PLASMITE_LIB_DIR" not in os.environ:
    os.environ["PLASMITE_LIB_DIR"] = LIB_DIR

from plasmite import Client, Durability, Lite3Frame, PlasmiteError, parse_message


class BindingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = Path(self._tempdir())
        self.pool_dir = self.temp / "pools"
        self.pool_dir.mkdir(parents=True, exist_ok=True)

    def _tempdir(self) -> str:
        import tempfile

        return tempfile.mkdtemp(prefix="plasmite-py-")

    def test_append_get_large_payload(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("big", 1024 * 1024)

        payload = {"blob": "x" * (64 * 1024)}
        tags = ["alpha", "beta", "gamma"]
        msg_bytes = pool.append_json(
            json.dumps(payload).encode("utf-8"), tags, Durability.FAST
        )
        message = parse_message(msg_bytes)
        self.assertEqual(len(message["data"]["blob"]), len(payload["blob"]))
        self.assertEqual(message["meta"]["tags"], tags)

        get_bytes = pool.get_json(message["seq"])
        fetched = parse_message(get_bytes)
        self.assertEqual(len(fetched["data"]["blob"]), len(payload["blob"]))
        get_alias_bytes = pool.get(message["seq"])
        get_alias = parse_message(get_alias_bytes)
        self.assertEqual(get_alias["seq"], fetched["seq"])

        pool.close()
        client.close()

    def test_tail_timeout_and_close(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("tail", 1024 * 1024)

        stream = pool.open_stream(since_seq=9999, max_messages=1, timeout_ms=10)
        self.assertIsNone(stream.next_json())
        stream.close()
        stream.close()

        pool.close()
        client.close()

    def test_stream_and_lite3_stream_iterators(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("iter", 1024 * 1024)

        first = parse_message(pool.append({"kind": "one"}, ["iter"]))
        pool.append({"kind": "two"}, ["iter"])
        pool.append({"kind": "three"}, ["iter"])

        stream = pool.open_stream(since_seq=first["seq"], max_messages=3, timeout_ms=50)
        seen = [parse_message(raw)["data"]["kind"] for raw in stream]
        self.assertEqual(seen, ["one", "two", "three"])
        self.assertIsNone(stream.next_json())
        stream.close()

        frame_seed = pool.get_lite3(first["seq"])
        seq2 = pool.append_lite3(frame_seed.payload, Durability.FAST)
        pool.append_lite3(frame_seed.payload, Durability.FAST)

        lite3_stream = pool.open_lite3_stream(since_seq=seq2, max_messages=2, timeout_ms=50)
        frame_seqs = [frame.seq for frame in lite3_stream]
        self.assertEqual(frame_seqs, [seq2, seq2 + 1])
        self.assertIsNone(lite3_stream.next())
        lite3_stream.close()

        pool.close()
        client.close()

    def test_tail_filters_by_tags(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("tail-tags", 1024 * 1024)

        pool.append_json(b'{"kind":"drop"}', ["drop"], Durability.FAST)
        pool.append_json(b'{"kind":"keep"}', ["keep"], Durability.FAST)

        results = list(pool.tail(max_messages=1, timeout_ms=50, tags=["keep"]))
        self.assertEqual(len(results), 1)
        message = parse_message(results[0])
        self.assertEqual(message["data"]["kind"], "keep")

        pool.close()
        client.close()

    def test_replay_filters_by_tags_before_max_messages(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("replay-tags", 1024 * 1024)

        pool.append_json(b'{"kind":"drop"}', ["drop"], Durability.FAST)
        pool.append_json(b'{"kind":"keep"}', ["keep"], Durability.FAST)

        results = list(pool.replay(speed=1.0, max_messages=1, timeout_ms=50, tags=["keep"]))
        self.assertEqual(len(results), 1)
        message = parse_message(results[0])
        self.assertEqual(message["data"]["kind"], "keep")

        pool.close()
        client.close()

    def test_tail_no_tags_skips_tag_filter_helper(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("tail-no-tags-fast-path", 1024 * 1024)
        pool.append_json(b'{"kind":"keep"}', ["keep"], Durability.FAST)

        with mock.patch(
            "plasmite._message_has_tags",
            side_effect=AssertionError("tail should bypass tag helper without tags"),
        ):
            results = list(pool.tail(max_messages=1, timeout_ms=50))
        self.assertEqual(len(results), 1)
        self.assertEqual(parse_message(results[0])["data"]["kind"], "keep")

        pool.close()
        client.close()

    def test_replay_no_tags_skips_tag_filter_helper(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("replay-no-tags-fast-path", 1024 * 1024)
        pool.append_json(b'{"kind":"keep"}', ["keep"], Durability.FAST)

        with mock.patch(
            "plasmite._message_has_tags",
            side_effect=AssertionError("replay should bypass tag helper without tags"),
        ):
            results = list(pool.replay(speed=1.0, max_messages=1, timeout_ms=50))
        self.assertEqual(len(results), 1)
        self.assertEqual(parse_message(results[0])["data"]["kind"], "keep")

        pool.close()
        client.close()

    def test_closed_handles_error(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("closed", 1024 * 1024)
        pool.close()

        with self.assertRaises(PlasmiteError):
            pool.append_json(b"{}", [], Durability.FAST)

        client.close()
        with self.assertRaises(PlasmiteError):
            client.create_pool("oops", 1024 * 1024)

    def test_lite3_append_get_stream(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("lite3", 1024 * 1024)

        msg_bytes = pool.append_json(
            json.dumps({"x": 1}).encode("utf-8"), ["alpha"], Durability.FAST
        )
        message = parse_message(msg_bytes)
        frame = pool.get_lite3(message["seq"])
        self.assertIsInstance(frame, Lite3Frame)
        self.assertGreater(len(frame.payload), 0)

        seq2 = pool.append_lite3(frame.payload, Durability.FAST)
        frame2 = pool.get_lite3(seq2)
        self.assertEqual(frame2.payload, frame.payload)

        stream = pool.open_lite3_stream(since_seq=seq2, max_messages=1, timeout_ms=50)
        next_frame = stream.next()
        self.assertIsNotNone(next_frame)
        assert next_frame is not None
        self.assertEqual(next_frame.seq, seq2)
        self.assertEqual(next_frame.payload, frame.payload)
        self.assertIsNone(stream.next())
        stream.close()

        pool.close()
        client.close()

    def test_lite3_invalid_payload(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("lite3-bad", 1024 * 1024)

        with self.assertRaises(PlasmiteError):
            pool.append_lite3(b"\x01", Durability.FAST)

        pool.close()
        client.close()

    def test_context_manager_lifecycle(self) -> None:
        with Client(str(self.pool_dir)) as client:
            with client.create_pool("ctx", 1024 * 1024) as pool:
                msg = pool.append_json(b'{"x":1}', [], Durability.FAST)
                parsed = parse_message(msg)
                with pool.open_stream(
                    since_seq=parsed["seq"], max_messages=1, timeout_ms=50
                ) as stream:
                    self.assertIsNotNone(stream.next_json())

        with self.assertRaises(PlasmiteError):
            client.create_pool("after-close", 1024 * 1024)

    def test_validation_errors_are_deterministic(self) -> None:
        with Client(str(self.pool_dir)) as client:
            pool = client.create_pool("validation", 1024 * 1024)
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


if __name__ == "__main__":
    unittest.main()
