"""
Purpose: Validate Python binding behaviors for payloads, tailing, and lifecycle.
Key Exports: None (unittest module).
Role: Exercise the Python binding against native pools and server processes.
Invariants: Requires libplasmite to be discoverable.
Notes: Uses temporary directories and avoids global state.
"""

from __future__ import annotations

import json
from datetime import timedelta
import os
import socket
import subprocess
import tempfile
import threading
import time
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
    RetentionGapError,
    UdpClient,
    UdpFrame,
    UdpGap,
    UdpProgress,
    parse_message,
)

TEST_POOL_SIZE_BYTES = 1024 * 1024


class BindingTests(unittest.TestCase):
    def setUp(self) -> None:
        temp = tempfile.TemporaryDirectory(prefix="plasmite-py-")
        self.addCleanup(temp.cleanup)
        self.temp = Path(temp.name)
        self.pool_dir = self.temp / "pools"
        self.pool_dir.mkdir(parents=True, exist_ok=True)

    def _new_client(self) -> Client:
        return Client(str(self.pool_dir))

    def _new_pool(self, client: Client, name: str) -> Any:
        return client.create_pool(name, TEST_POOL_SIZE_BYTES)

    def _fill_past(self, pool: Any, seq: int) -> None:
        for n in range(10_000):
            pool.feed(
                {"n": n, "padding": "x" * 4096},
                ["drop"],
                Durability.FAST,
            )
            try:
                parse_message(pool._get_json(seq))
            except NotFoundError:
                return
        self.fail(f"pool did not overwrite sequence {seq}")

    @contextmanager
    def _client_pool(self, name: str) -> Iterator[tuple[Client, Any]]:
        client = self._new_client()
        pool = self._new_pool(client, name)
        try:
            yield client, pool
        finally:
            pool.close()
            client.close()

    def test_numeric_append_receipts_match_stored_frames(self) -> None:
        with self._client_pool("identities") as (_, pool):
            value = {"nested": [1, {"text": "hello"}]}
            first = pool.feed_receipt(value, ["tag"], Durability.FLUSH)
            self.assertIsInstance(first, plasmite.FeedReceipt)
            self.assertEqual(first.seq, 1)
            stored = pool._get_lite3(first.seq)
            self.assertEqual(first.time, stored.time)
            self.assertGreater(first.time, 2**53)
            value["nested"].clear()
            self.assertEqual(parse_message(pool._get_json(first.seq)).data, {"nested": [1, {"text": "hello"}]})
            second = pool.feed_lite3_receipt(bytearray(stored.payload), Durability.FAST)
            self.assertEqual(second.seq, 2)
            self.assertEqual(second.time, pool._get_lite3(2).time)
            with self.assertRaises(PlasmiteError):
                pool.feed_json_receipt(b"bad", [], Durability.FAST)
            with self.assertRaises(PlasmiteError):
                pool.feed_lite3_receipt(b"bad", Durability.FAST)
            self.assertEqual(pool.feed_receipt({}).seq, 3)


    def test_append_get_large_payload(self) -> None:
        with self._client_pool("big") as (_, pool):
            payload = {"blob": "0123456789abcdef" * 4096}
            tags = ["alpha", "beta", "gamma"]
            msg_bytes = pool.feed_json(
                json.dumps(payload).encode("utf-8"), tags, Durability.FAST
            )
            message = parse_message(msg_bytes)
            self.assertIsInstance(message, Message)
            self.assertIsInstance(message.meta, MessageMeta)
            self.assertEqual(message.data, payload)
            self.assertEqual(message.meta.tags, tags)
            self.assertEqual(message.tags, tags)
            self.assertEqual(message.raw, msg_bytes)
            self.assertIsInstance(message.time, int)

            get_bytes = pool._get_json(message.seq)
            fetched = parse_message(get_bytes)
            self.assertEqual(fetched.data, payload)
            get_alias = parse_message(pool._get_json(message.seq))
            self.assertIsInstance(get_alias, Message)
            self.assertEqual(get_alias.seq, fetched.seq)

    def test_message_time_fraction_widths_and_raw_precision(self) -> None:
        for fraction in ("1", "123", "12345", "123456", "123456789"):
            with self.subTest(fraction=fraction):
                payload = json.dumps({"seq": 1, "time": f"1970-01-01T01:00:00.{fraction}+01:00", "meta": {"tags": []}, "data": {}}).encode()
                message = parse_message(payload)
                self.assertEqual(message.time, int(fraction.ljust(9, "0")))
                self.assertEqual(message.raw, payload)

    def test_append_borrows_immutable_input_and_keeps_owned_results(self) -> None:
        with self._client_pool("buffers") as (_, pool):
            payload = bytearray(b'{"text":"caf\\u00e9","values":[0,1,2]}')
            raw = pool.feed_json(memoryview(payload), [], Durability.FAST)
            payload[:] = b"x" * len(payload)
            expected = {"text": "caf\u00e9", "values": [0, 1, 2]}
            self.assertEqual(parse_message(raw).data, expected)
            frame = pool._get_lite3(1)
            # Lite3 contains zero bytes; a C string argument must preserve length.
            self.assertIn(b"\0", frame.payload)
            self.assertEqual(pool.feed_lite3(frame.payload, Durability.FAST).seq, 2)
            self.assertEqual(parse_message(pool._get_json(2)).data, expected)
            with self.assertRaises(PlasmiteError):
                pool.feed_json(b'{"x":1}\0{"x":2}', [], Durability.FAST)
        self.assertEqual(parse_message(raw).data, expected)

    def test_default_pool_size_constant_matches_core_default(self) -> None:
        self.assertEqual(plasmite.DEFAULT_POOL_SIZE_BYTES, 1024 * 1024)

    def test_create_on_open_preserves_pool(self) -> None:
        with self._new_client() as client:
            first = client.open_pool("work", create=True)
            second = client.open_pool("work", create=True)
            first_msg = first.feed({"kind": "one"}, ["alpha"])
            second_msg = parse_message(second._get_json(first_msg.seq))
            self.assertEqual(second_msg.data["kind"], "one")
            first.close()
            second.close()

    @unittest.skipIf(os.name == "nt", "Unix account database policy")
    def test_default_directory_uses_account_for_invalid_home(self) -> None:
        import pwd
        account_home = pwd.getpwuid(os.getuid()).pw_dir
        for value in [None, "", "relative", "/tmp/home space 東京"]:
            with mock.patch.dict(os.environ, {}, clear=False):
                if value is None:
                    os.environ.pop("HOME", None)
                else:
                    os.environ["HOME"] = value
                expected = value if value and os.path.isabs(value) else account_home
                self.assertEqual(plasmite.default_pool_dir(), os.path.join(expected, ".plasmite", "pools"))
        with mock.patch.dict(os.environ, {"HOME": "relative"}), mock.patch("pwd.getpwuid", side_effect=KeyError):
            with self.assertRaisesRegex(ValueError, "explicit pool directory"):
                plasmite.default_pool_dir()
            client = Client(str(self.pool_dir))
            client.close()

    @unittest.skipUnless(os.name == "nt", "Windows profile-directory policy")
    def test_default_directory_uses_profile_for_invalid_userprofile(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("USERPROFILE", None)
            account_directory = plasmite.default_pool_dir()
            self.assertTrue(os.path.isabs(account_directory))
            self.assertTrue(account_directory.endswith(os.path.join(".plasmite", "pools")))
            for value in ["", "relative"]:
                os.environ["USERPROFILE"] = value
                self.assertEqual(plasmite.default_pool_dir(), account_directory)
            home = str(self.temp / "home space 東京")
            os.environ["USERPROFILE"] = home
            self.assertEqual(plasmite.default_pool_dir(), os.path.join(home, ".plasmite", "pools"))

    def test_default_client_creates_pool_dir_in_fresh_home(self) -> None:
        import tempfile

        home = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="plasmite-py-home-")))
        expected_pool_dir = home / ".plasmite" / "pools"
        expected_pool_path = expected_pool_dir / "work.plasmite"
        self.assertFalse(expected_pool_dir.exists())

        with mock.patch.dict(os.environ, {"USERPROFILE" if os.name == "nt" else "HOME": str(home)}, clear=False):
            client = Client()
            pool = client.open_pool("work", create=True)
            msg = pool.feed({"kind": "one"}, ["alpha"])
            self.assertEqual(msg.data["kind"], "one")
            pool.close()
            client.close()

        self.assertTrue(expected_pool_path.exists())

    def test_follow_timeout_and_close(self) -> None:
        with self._client_pool("live") as (_, pool):
            reader = pool.follow()
            self.assertIsNone(reader.next(timedelta(milliseconds=10)))
            reader.close()
            reader.close()

    def test_stream_and_lite3_stream_iterators(self) -> None:
        with self._client_pool("iter") as (_, pool):
            first = pool.feed({"kind": "one"}, ["iter"])
            pool.feed({"kind": "two"}, ["iter"])
            pool.feed({"kind": "three"}, ["iter"])

            with pool.fetch(since=first.seq) as reader:
                self.assertEqual([item.data["kind"] for item in reader], ["one", "two", "three"])

            frame_seed = pool._get_lite3(first.seq)
            seq2 = pool.feed_lite3(frame_seed.payload).seq
            pool.feed_lite3(frame_seed.payload)
            with pool.fetch_lite3(since=seq2) as reader:
                self.assertEqual([frame.seq for frame in reader], [seq2, seq2 + 1])

    def test_tail_filters_by_tags(self) -> None:
        with self._client_pool("tail-tags") as (_, pool):
            pool.feed_json(b'{"kind":"drop"}', ["drop"], Durability.FAST)
            pool.feed_json(b'{"kind":"keep"}', ["keep"], Durability.FAST)

            with pool.fetch(tags=["keep"]) as reader:
                results = list(reader)
            self.assertEqual(len(results), 1)
            message = results[0]
            self.assertIsInstance(message, Message)
            self.assertEqual(message.data["kind"], "keep")

    def test_tail_can_fail_when_retention_overtakes_filtered_consumer(self) -> None:
        with self._new_client() as client:
            pool = client.create_pool("tail-gap", 64 * 1024)
            first = pool.feed({"kind": "first"}, ["keep"])
            self._fill_past(pool, first.seq)

            with self.assertRaises(RetentionGapError) as raised:
                pool.follow(since=first.seq, tags=["never"])
            self.assertEqual(raised.exception.kind, ErrorKind.RETENTION_GAP)
            self.assertEqual(raised.exception.missing, first.seq)

            with pool.follow(since=raised.exception.next_seq) as reader:
                continuing = next(reader)
            self.assertGreater(continuing.seq, first.seq)
            pool.close()

    def test_follow_reports_first_unread_sequence_after_overwrite(self) -> None:
        with self._new_client() as client, client.create_pool("slow-reader", 64 * 1024) as pool:
            seed = pool.feed({"kind": "seed"})
            with pool.follow(since=seed.seq) as reader:
                self.assertEqual(next(reader).seq, seed.seq)
                lost = pool.feed({"kind": "lost"})
                self._fill_past(pool, lost.seq)
                with self.assertRaises(RetentionGapError) as raised:
                    next(reader)
                self.assertEqual(raised.exception.missing, lost.seq)

    def test_lite3_reader_reports_retention_gap_at_open(self) -> None:
        with self._new_client() as client, client.create_pool("lite3-gap", 64 * 1024) as pool:
            first = pool.feed({"kind": "first"})
            self._fill_past(pool, first.seq)
            with self.assertRaises(RetentionGapError) as raised:
                pool.fetch_lite3(since=first.seq)
            self.assertEqual(raised.exception.missing, first.seq)

    def test_closed_handles_error(self) -> None:
        client = self._new_client()
        pool = self._new_pool(client, "closed")
        pool.close()

        with self.assertRaises(PlasmiteError):
            pool.feed_json(b"{}", [], Durability.FAST)

        client.close()
        with self.assertRaises(PlasmiteError):
            client.create_pool("oops", TEST_POOL_SIZE_BYTES)

    def test_lite3_append_get_stream(self) -> None:
        with self._client_pool("lite3") as (_, pool):
            msg_bytes = pool.feed_json(
                json.dumps({"x": 1}).encode("utf-8"), ["alpha"], Durability.FAST
            )
            message = parse_message(msg_bytes)
            frame = pool._get_lite3(message.seq)
            self.assertIsInstance(frame, Lite3Frame)
            self.assertEqual(frame.time, message.time)
            self.assertGreater(len(frame.payload), 0)

            seq2 = pool.feed_lite3(frame.payload, Durability.FAST).seq
            frame2 = pool._get_lite3(seq2)
            self.assertEqual(frame2.payload, frame.payload)

            with pool.fetch_lite3(since=seq2) as reader:
                frame = next(reader)
                self.assertEqual(frame.seq, seq2)
                self.assertEqual(frame.payload, frame2.payload)
                with self.assertRaises(StopIteration):
                    next(reader)

    def test_lite3_invalid_payload(self) -> None:
        with self._client_pool("lite3-bad") as (_, pool):
            with self.assertRaises(PlasmiteError):
                pool.feed_lite3(b"\x01", Durability.FAST)

    def test_context_manager_lifecycle(self) -> None:
        with self._new_client() as client:
            with client.create_pool("ctx", TEST_POOL_SIZE_BYTES) as pool:
                msg = pool.feed_json(b'{"x":1}', [], Durability.FAST)
                parsed = parse_message(msg)
                with pool.fetch(since=parsed.seq) as reader:
                    self.assertEqual(next(reader).seq, parsed.seq)

        with self.assertRaises(PlasmiteError):
            client.create_pool("after-close", TEST_POOL_SIZE_BYTES)

    def test_validation_errors_are_deterministic(self) -> None:
        with self._new_client() as client:
            pool = client.create_pool("validation", TEST_POOL_SIZE_BYTES)
            with self.assertRaises(plasmite.UsageError):
                pool.feed_json(b"", [], Durability.FAST)
            invalid_payload = "{}"
            with self.assertRaises(TypeError):
                pool.feed_json(invalid_payload, [], Durability.FAST)
            with self.assertRaises(ValueError):
                pool._get_json(-1)
            pool.close()
            with self.assertRaises(PlasmiteError):
                pool._get_json(1)

    def test_follow_starts_now_and_history_is_explicit(self):
        with self._new_client() as client, client.create_pool("follow-choice", TEST_POOL_SIZE_BYTES) as pool:
            pool.feed({"n": 1}, ["skip"])
            pool.feed({"n": 2}, ["keep"])
            with pool.follow() as reader:
                self.assertIsNone(reader.next(timedelta(0)))
                appended = pool.feed({"n": 3}, ["keep"])
                self.assertEqual(reader.next(timedelta(milliseconds=100)).seq, appended.seq)
            with pool.fetch(last=2) as reader:
                self.assertEqual([m.seq for m in reader], [2, 3])
            with pool.fetch_view() as reader:
                self.assertEqual([m.seq for m in reader], [1, 2, 3])
            with pool.fetch(last=0) as reader:
                self.assertEqual(list(reader), [])

    def test_v2_listener_and_pool_lifecycle(self):
        from datetime import timedelta

        with self._new_client() as client:
            self.assertEqual(client.list_pools(), [])
            with client.open_pool("listener", create=True) as pool:
                self.assertEqual(pool.info()["bounds"], {})
                old = pool.feed({"n": 1}, tags=["keep"])
                with pool.follow(tags=["keep"]) as reader:
                    self.assertIsNone(reader.next(timedelta(0)))
                    pool.feed({"n": 2})
                    new = pool.feed({"n": 3}, tags=["keep"])
                    self.assertEqual(next(reader).seq, new.seq)
                self.assertEqual(pool.info()["bounds"], {"oldest": old.seq, "newest": new.seq})
                self.assertEqual(len(client.list_pools()), 1)
                with pool.fetch(since=new.seq, tags=["keep"]) as reader:
                    forwarded = pool.feed(new)
                    self.assertEqual(forwarded.tags, new.tags)
                    self.assertEqual([m.seq for m in reader], [new.seq])
                with self.assertRaises(plasmite.UsageError):
                    pool.feed(new, tags=[])
                view = pool.feed_view(new)
                self.assertEqual(list(view.tags), new.tags)
                with self.assertRaises(plasmite.UsageError):
                    pool.feed_view(view, tags=[])
                with self.assertRaises(plasmite.UsageError):
                    pool.fetch(since="oldest", last=1)
                with pool.fetch(since=plasmite.at(0)) as reader:
                    self.assertEqual(next(reader).seq, old.seq)
            client.delete_pool("listener")
            self.assertEqual(client.list_pools(), [])

    def test_shared_2_0_conformance(self):
        from shutil import copyfile

        fixture = REPO_ROOT / "tests/fixtures/conformance/reads.json"
        cases = json.loads(fixture.read_text(encoding="utf8"))

        def seed(pool, records):
            for msg in records:
                pool.feed(msg["data"], tags=msg["tags"])

        def check_error(kind, call):
            with self.assertRaises(PlasmiteError) as caught:
                call()
            self.assertEqual(caught.exception.kind.name.replace("_", "").lower(), kind.lower())
            return caught.exception

        with self._new_client() as client:
            check_error(cases["pools"]["missing"], lambda: client.open_pool("events"))
            with client.open_pool("events", create=True) as pool:
                check_error(cases["pools"]["existing"], lambda: client.create_pool("events"))
                check_error(cases["pools"]["non_object_feed"], lambda: pool.feed([]))
                seed(pool, cases["seed"])
                for single, reads in [(False, cases["fetch"]), (True, cases["single_fetch"])]:
                    for case in reads:
                        with self.subTest(case=case["name"]):
                            options = {key: case[key] for key in ("since", "last", "tags") if key in case}
                            if "error" in case:
                                check_error(case["error"], lambda: pool.fetch(**options))
                                continue
                            with pool.fetch(**options) as reader:
                                if single:
                                    message = next(reader, None)
                                    got = [] if message is None else [message.seq]
                                else:
                                    got = [message.seq for message in reader]
                            self.assertEqual(got, case["expect"])
                case = cases["fetch_snapshot"]
                with pool.fetch() as reader:
                    seed(pool, case["after_open"])
                    self.assertEqual([message.seq for message in reader], case["expect"])
            client.delete_pool("events")

            for case in cases["follow"]:
                with self.subTest(case=case["name"]):
                    with client.create_pool("follow") as pool:
                        seed(pool, cases["seed"])
                        options = {key: case[key] for key in ("since", "last", "tags") if key in case}
                        cancel = threading.Event()
                        with pool.follow(**options, cancel=cancel) as reader:
                            seed(pool, case.get("after_open", []))
                            from datetime import timedelta
                            got = [reader.next(timedelta(seconds=2)).seq for _ in case["expect"]]
                            self.assertEqual(got, case["expect"])
                            cancel.set()
                            self.assertIsNone(next(reader, None))
                    client.delete_pool("follow")

            retained = cases["retention"]
            with client.create_pool("small", retained["size"]) as pool:
                for i in range(retained["inserts"]):
                    pool.feed({"i": i, "pad": "x" * retained["padding_bytes"]})
                gap = check_error(retained["error"],
                                  lambda: pool.fetch(since=retained["since"], tags=retained["tags"]))
                self.assertEqual(gap.missing, retained["since"])
                self.assertEqual(gap.next_seq, pool.info()["bounds"]["oldest"])
            previous = cases["previous_release"]
            copyfile(REPO_ROOT / previous["fixture"], self.pool_dir / "previous.plasmite")
            with client.open_pool("previous") as pool, pool.fetch() as reader:
                self.assertEqual([{"data": m.data, "tags": m.tags} for m in reader], previous["messages"])

    def test_unsigned_64_bit_arguments_do_not_wrap(self) -> None:
        overflow = (1 << 64) + 1
        with self._new_client() as client:
            with self.assertRaisesRegex(ValueError, "size_bytes must fit"):
                client.create_pool("wrapped-size", 1 << 64)
            self.assertFalse((self.pool_dir / "wrapped-size.plasmite").exists())

            with client.create_pool("no-wrap", TEST_POOL_SIZE_BYTES) as pool:
                message = pool.feed({"kind": "first"}, [])
                self.assertEqual(message.seq, 1)
                with self.assertRaisesRegex(ValueError, "seq must fit"):
                    parse_message(pool._get_json(overflow))
                with self.assertRaisesRegex(ValueError, "seq must fit"):
                    pool._get_lite3(overflow)

                for open_reader in (pool.follow, pool.follow_lite3):
                    for argument in ("since", "last"):
                        with self.subTest(reader=open_reader.__name__, argument=argument):
                            with self.assertRaisesRegex(plasmite.UsageError, f"{argument} must fit"):
                                open_reader(**{argument: overflow})

                with self.assertRaises(NotFoundError):
                    pool._get_json((1 << 64) - 1)

    def test_error_subclasses_are_raised(self) -> None:
        with self._new_client() as client:
            self.assertIs(plasmite._ERROR_KIND_TO_CLASS[ErrorKind.ALREADY_EXISTS], AlreadyExistsError)
            self.assertIs(
                plasmite._ERROR_KIND_TO_CLASS[ErrorKind.RETENTION_GAP],
                RetentionGapError,
            )
            with self.assertRaises(NotFoundError):
                client.open_pool("missing")

    def test_pool_lookup_errors_distinguish_missing_sequence_and_corrupt_file(self) -> None:
        pool_path = self.pool_dir / "damaged.plasmite"
        with self._new_client() as client:
            with self.assertRaises(NotFoundError) as missing_pool:
                client.open_pool("damaged")
            self.assertEqual(missing_pool.exception.path, str(pool_path))
            self.assertIsNone(missing_pool.exception.seq)

            with client.create_pool("damaged", TEST_POOL_SIZE_BYTES) as pool:
                with self.assertRaises(NotFoundError) as missing_message:
                    parse_message(pool._get_json(42))
                self.assertEqual(missing_message.exception.seq, 42)
                self.assertIsNone(missing_message.exception.path)

            with pool_path.open("r+b") as handle:
                handle.truncate(4)
            with self.assertRaises(plasmite.CorruptError) as corrupt:
                client.open_pool("damaged")
            self.assertEqual(corrupt.exception.path, str(pool_path))
            self.assertIsNone(corrupt.exception.seq)

    def test_udp_delivers_late_and_duplicate_frames(self) -> None:
        self._check_udp_deliveries(False)

    def test_udp_views_retain_late_and_duplicate_frames(self) -> None:
        self._check_udp_deliveries(True)

    def _check_udp_deliveries(self, views):
        from plasmite import MessageView
        with self._new_client() as client:
            with self._new_pool(client, "udp-order") as pool:
                message = pool.feed({"value": 17}, ["tag"])
                payload = pool._get_lite3(message.seq).payload
        base = 1 << 53
        timestamp = 1_700_000_000_000_000_001
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
            peer.bind(("127.0.0.1", 0))
            peer.settimeout(3)

            def serve() -> None:
                request, address = peer.recvfrom(4096)
                subscription = request[8:24]

                def send(op: int, body: bytes) -> None:
                    peer.sendto(b"\0PUDP\x01" + bytes([op, 0]) + subscription + body, address)

                cookie = {"expires_ms": 10_000, "mac": "test-cookie"}
                send(3, json.dumps({"cookie": cookie}).encode())
                verified, source = peer.recvfrom(4096)
                assert source == address
                assert json.loads(verified[24:])["cookie"] == cookie

                def data(seq: int) -> None:
                    send(7, seq.to_bytes(8, "big") + timestamp.to_bytes(8, "big") + payload)

                def progress(terminal: bool) -> None:
                    send(4, json.dumps({
                        "start_seq": base + 10, "next_seq": base + 13,
                        "end_seq": base + 12, "lease_ms": 30_000, "terminal": terminal,
                    }).encode())

                data(base + 10)
                progress(False)
                send(5, json.dumps({"first": base + 11, "last": base + 11, "reason": "pressure"}).encode())
                for seq in [base + 12, base + 11, base + 12]:
                    data(seq)
                progress(True)

            worker = threading.Thread(target=serve)
            worker.start()
            try:
                with UdpClient(f"udp://127.0.0.1:{peer.getsockname()[1]}") as client:
                    with client.open_pool("sensors") as pool:
                        open_follow = pool.fetch_view if views else pool.fetch
                        with open_follow(last=4) as follow:
                            events = [follow.next_event(timeout_ms=1000) for _ in range(7)]
                            self.assertIsNone(follow.next_event(timeout_ms=0))
                frame_type = MessageView if views else UdpFrame
                frames = [event for event in events if isinstance(event, frame_type)]
                self.assertEqual([frame.seq for frame in frames], [base + 10, base + 12, base + 11, base + 12])
                for frame in frames:
                    self.assertEqual(frame.time, timestamp)
                    if views:
                        self.assertEqual(frame.data["value"], 17)
                        self.assertEqual(list(frame.meta["tags"]), ["tag"])
                    else:
                        self.assertEqual(frame.payload, payload)
                        self.assertEqual(frame.message().data, {"value": 17})
                skips = [event for event in events if isinstance(event, UdpGap)]
                self.assertEqual([(event.first, event.last, event.reason) for event in skips], [(base + 11, base + 11, "pressure")])
                progress = [event for event in events if isinstance(event, UdpProgress)]
                self.assertEqual([event.next_seq for event in progress], [base + 13, base + 13])
                self.assertTrue(progress[-1].terminal)
            finally:
                worker.join(timeout=4)
                self.assertFalse(worker.is_alive())

    def test_udp_sdk_send_and_follow_against_server_process(self) -> None:
        binary = Path(os.environ.get("PLASMITE_BIN", REPO_ROOT / "target" / "debug" / "plasmite"))
        if not binary.exists():
            self.skipTest(f"Plasmite CLI is not built: {binary}")

        with self._new_client() as local_client:
            with local_client.create_pool("udp-sdk", TEST_POOL_SIZE_BYTES) as local_pool:
                original = local_pool.feed({"origin": "lite3", "value": 7}, ["source"])
                original_lite3 = local_pool._get_lite3(original.seq).payload
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as held:
                    held.bind(("127.0.0.1", 0))
                    port = held.getsockname()[1]

                home = self.temp / "server-home"
                home.mkdir(parents=True, exist_ok=True)
                ready = self.temp / "server-ready"
                env = {
                    **os.environ,
                    "HOME": str(home),
                    "PLASMITE_ACCESS_HOME": str(home),
                    "PLASMITE_SERVE_READY_FILE": str(ready),
                }
                server = subprocess.Popen(
                    [
                        str(binary), "--dir", str(self.pool_dir), "serve",
                        "--bind", "127.0.0.1:0", "--remote-bind", "127.0.0.1:0",
                        f"--udp=127.0.0.1:{port}", "--udp-follow",
                    ],
                    cwd=REPO_ROOT,
                    env=env,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.PIPE,
                )
                try:
                    deadline = time.monotonic() + 10
                    while not ready.exists():
                        if server.poll() is not None:
                            details = server.stderr.read().decode("utf-8", "replace")
                            self.fail(f"server exited before readiness: {details}")
                        if time.monotonic() >= deadline:
                            self.fail("server did not become ready")
                        time.sleep(0.02)

                    with UdpClient(f"udp://127.0.0.1:{port}") as udp_client:
                        with udp_client.open_pool("udp-sdk") as udp_pool:
                            with udp_pool.follow(since=original.seq + 1) as follow:
                                self.assertIsNone(
                                    udp_pool.feed({"origin": "json", "value": 8}, ["sensor"])
                                )
                                json_frame = _next_udp_frame(self, follow)
                                json_message = json_frame.message()
                                self.assertEqual(json_message.data, {"origin": "json", "value": 8})
                                self.assertEqual(json_message.tags, ["sensor"])
                                json_time = local_pool._get_lite3(json_frame.seq).time
                                self.assertEqual(json_frame.time, json_time)
                                self.assertGreater(json_frame.time, 1 << 53)
                                stored_json = parse_message(local_pool._get_json(json_frame.seq))
                                self.assertEqual(stored_json.data, json_message.data)

                                mutable_lite3 = bytearray(original_lite3)
                                self.assertIsNone(udp_pool.feed_lite3(mutable_lite3))
                                mutable_lite3[:] = b"\x00" * len(mutable_lite3)
                                lite3_frame = _next_udp_frame(self, follow)
                                self.assertEqual(lite3_frame.payload, original_lite3)
                                self.assertEqual(lite3_frame.message().data, original.data)
                                self.assertEqual(lite3_frame.message().tags, ["source"])
                                stored_lite3 = parse_message(local_pool._get_json(lite3_frame.seq))
                                self.assertEqual(stored_lite3.data, original.data)

                            large_start = (1 << 53) + 1
                            with self.assertRaises(plasmite.UsageError):
                                udp_pool.fetch(since=large_start)

                            cancel_follow = udp_pool.follow()
                            self.assertIsNotNone(cancel_follow.next_event(timeout_ms=0))
                            received = []
                            waiter = threading.Thread(
                                target=lambda: received.append(cancel_follow.next_event())
                            )
                            waiter.start()
                            time.sleep(0.02)
                            cancel_follow.close()
                            waiter.join(timeout=1)
                            self.assertFalse(waiter.is_alive(), "close did not cancel a blocked wait")
                            self.assertEqual(received, [None])
                finally:
                    server.terminate()
                    try:
                        server.communicate(timeout=5)
                    except subprocess.TimeoutExpired:
                        server.kill()
                        server.communicate()


def _next_udp_frame(test: unittest.TestCase, follow) -> UdpFrame:
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        event = follow.next_event(timeout_ms=100)
        if isinstance(event, UdpFrame):
            return event
    test.fail("UDP follower did not receive a frame")
    raise AssertionError("unreachable")


if __name__ == "__main__":
    unittest.main()
