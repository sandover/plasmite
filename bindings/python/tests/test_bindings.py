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
    Message,
    NotFoundError,
    PlasmiteError,
    RetentionGapError,
    UdpClient,
    UdpSkip,
    UdpProgress,
)

TEST_POOL_SIZE_BYTES = 1024 * 1024


def get_message(pool, seq):
    with pool.fetch(since=seq) as reader:
        message = next(reader)
    assert message.seq == seq
    return message


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
                get_message(pool, seq)
            except RetentionGapError:
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

    def test_embedded_nul_in_pool_identifiers_is_rejected(self) -> None:
        with self._new_client() as client:
            client.create_pool("events").close()
            invalid = "events\0other"
            for operation in (client.create_pool, client.open_pool, client.delete_pool):
                with self.subTest(operation=operation.__name__):
                    with self.assertRaisesRegex(ValueError, "NUL"):
                        operation(invalid)
            with client.open_pool("events") as pool:
                self.assertIsNotNone(pool.info())

        with self.assertRaisesRegex(ValueError, "NUL"):
            Client(str(self.pool_dir) + "\0other")
        with self.assertRaisesRegex(ValueError, "NUL"):
            UdpClient("udp://127.0.0.1:9701\0other")
        with UdpClient("udp://127.0.0.1:9701") as client:
            with self.assertRaisesRegex(ValueError, "NUL"):
                client.open_pool(invalid)


    def test_committed_identity_matches_stored_message(self) -> None:
        with self._client_pool("identities") as (_, pool):
            value = {"nested": [1, {"text": "hello"}]}
            first = pool.feed(value, ["tag"], Durability.FLUSH)
            stored = get_message(pool, first.seq)
            value["nested"].clear()
            self.assertEqual(stored.data, {"nested": [1, {"text": "hello"}]})
            self.assertEqual((first.seq, first.time), (stored.seq, stored.time))
            self.assertGreater(first.time, 2**53)
            with self.assertRaises(PlasmiteError):
                pool.feed(b"bad", format="json")
            with self.assertRaises(PlasmiteError):
                pool.feed(b"bad", format="lite3")
            self.assertEqual(pool.feed({}).seq, first.seq + 1)


    def test_append_get_large_payload(self) -> None:
        with self._client_pool("big") as (_, pool):
            payload = {"blob": "0123456789abcdef" * 4096}
            tags = ["alpha", "beta", "gamma"]
            message = pool.feed(json.dumps(payload).encode("utf-8"), tags, format="json")
            self.assertIsInstance(message, Message)
            self.assertEqual(message.data, payload)
            self.assertEqual(message.tags, tags)
            self.assertIsInstance(message.time, int)
            fetched = get_message(pool, message.seq)
            self.assertEqual(fetched.data, payload)
            self.assertEqual(fetched.raw, message.raw)

    def test_one_message_owns_its_bytes_and_decodes_data_once(self) -> None:
        with self._client_pool("message") as (_, pool):
            payload = bytearray(b'{"nested":{"n":1}}')
            source = pool.feed(payload, tags=["tag"], format="json")
            payload.clear()
            self.assertIsInstance(source, Message)
            self.assertTrue(source.raw.readonly)
            self.assertIsNone(source.sender)
            with self.assertRaises(TypeError):
                pool.feed(source.raw)
            with self.assertRaises(plasmite.UsageError):
                pool.feed(source.raw, tags=[], format="lite3")
            forwarded = pool.feed(source)
            encoded = pool.feed(source.raw, format="lite3")
            with pool.fetch(tags=["tag"]) as reader:
                messages = list(reader)
        self.assertEqual([message.seq for message in messages], [source.seq, forwarded.seq, encoded.seq])
        self.assertTrue(all(type(message) is Message for message in messages))
        self.assertEqual(messages[0].time, source.time)
        self.assertEqual(forwarded.raw, source.raw)
        self.assertEqual(encoded.raw, source.raw)
        self.assertEqual(messages[0].field("nested"), {"n": 1})
        self.assertIsNone(messages[0].field("absent"))
        self.assertIs(messages[0].field("absent", source), source)
        self.assertIsNone(messages[0]._data)
        data = messages[0].data
        self.assertEqual(data, {"nested": {"n": 1}})
        self.assertIs(data, messages[0].data)
        data["nested"]["n"] = 2
        self.assertEqual(messages[0].field("nested"), {"n": 2})

    def test_create_replace_and_clear_pool_description(self) -> None:
        with self._new_client() as client:
            with client.create_pool("described", about="Handoffs", example={"state": "done"}) as pool:
                pool.feed({"state": "running"})
                self.assertEqual(pool.info()["about"], "Handoffs")
                self.assertEqual(pool.info()["example"], {"state": "done"})
                self.assertEqual(client.list_pools()[0]["about"], "Handoffs")
                pool.set_description(about="Build handoffs")
                self.assertEqual(pool.info()["about"], "Build handoffs")
                self.assertNotIn("example", pool.info())
                pool.set_description()
            with client.open_pool("described") as pool:
                self.assertNotIn("about", pool.info())


    def test_read_position_time_fraction_widths(self) -> None:
        for fraction in ("1", "123", "12345", "123456", "123456789"):
            with self.subTest(fraction=fraction):
                timestamp = f"1970-01-01T01:00:00.{fraction}+01:00"
                self.assertEqual(plasmite._parse_time_ns(timestamp), int(fraction.ljust(9, "0")))


    def test_append_snapshots_input_and_preserves_the_physical_byte_length(self) -> None:
        with self._client_pool("buffers") as (_, pool):
            payload = bytearray(br'{"text":"caf\u00e9","values":[0,1,2]}')
            message = pool.feed(memoryview(payload), format="json")
            payload[:] = b"x" * len(payload)
            expected = {"text": "café", "values": [0, 1, 2]}
            self.assertEqual(message.data, expected)
            self.assertIn(0, message.raw)
            self.assertEqual(pool.feed(message.raw, format="lite3").data, expected)
            with self.assertRaises(PlasmiteError):
                pool.feed(b'{"x":1}\0{"x":2}', format="json")
        self.assertEqual(message.data, expected)

    def test_default_pool_size_constant_matches_core_default(self) -> None:
        self.assertEqual(plasmite.DEFAULT_POOL_SIZE_BYTES, 1024 * 1024)

    def test_create_on_open_preserves_pool(self) -> None:
        with self._new_client() as client:
            first = client.open_pool("work", create=True)
            second = client.open_pool("work", create=True)
            first_msg = first.feed({"kind": "one"}, ["alpha"])
            second_msg = get_message(second, first_msg.seq)
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


    def test_stream_iterators_use_the_same_message_type(self) -> None:
        with self._client_pool("iter") as (_, pool):
            first = pool.feed({"kind": "one"}, ["iter"])
            pool.feed({"kind": "two"}, ["iter"])
            pool.feed({"kind": "three"}, ["iter"])
            with pool.fetch(since=first.seq) as reader:
                self.assertEqual([item.data["kind"] for item in reader], ["one", "two", "three"])
            with pool.follow() as reader:
                forwarded = pool.feed(first.raw, format="lite3")
                received = next(reader)
                self.assertIs(type(received), Message)
                self.assertEqual((received.seq, received.raw), (forwarded.seq, forwarded.raw))

    def test_tail_filters_by_tags(self) -> None:
        with self._client_pool("tail-tags") as (_, pool):
            pool.feed(b'{"kind":"drop"}', ["drop"], format="json")
            pool.feed(b'{"kind":"keep"}', ["keep"], format="json")

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
                pool.fetch(since=first.seq)
            self.assertEqual(raised.exception.missing, first.seq)

    def test_closed_handles_error(self) -> None:
        client = self._new_client()
        pool = self._new_pool(client, "closed")
        pool.close()

        with self.assertRaises(PlasmiteError):
            pool.feed({})

        client.close()
        with self.assertRaises(PlasmiteError):
            client.create_pool("oops", TEST_POOL_SIZE_BYTES)



    def test_lite3_invalid_payload(self) -> None:
        with self._client_pool("lite3-bad") as (_, pool):
            with self.assertRaises(PlasmiteError):
                pool.feed(b"\x01", format="lite3")


    def test_context_manager_lifecycle(self) -> None:
        with self._new_client() as client:
            with client.create_pool("ctx", TEST_POOL_SIZE_BYTES) as pool:
                sent = pool.feed(b'{"x":1}', format="json")
                with pool.fetch(since=sent.seq) as reader:
                    self.assertEqual(next(reader).seq, sent.seq)
        with self.assertRaises(PlasmiteError):
            client.create_pool("after-close", TEST_POOL_SIZE_BYTES)


    def test_validation_errors_are_deterministic(self) -> None:
        with self._new_client() as client:
            pool = client.create_pool("validation", TEST_POOL_SIZE_BYTES)
            with self.assertRaises(plasmite.UsageError):
                pool.feed(b"", format="json")
            with self.assertRaises(TypeError):
                pool.feed("{}", format="json")
            with self.assertRaises(plasmite.UsageError):
                pool.fetch(since=-1)
            pool.close()
            with self.assertRaises(PlasmiteError):
                pool.fetch(since=1)

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
            with pool.fetch() as reader:
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
                view = pool.feed(new)
                self.assertEqual(list(view.tags), new.tags)
                with self.assertRaises(plasmite.UsageError):
                    pool.feed(view, tags=[])
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
                for open_reader in (pool.follow, pool.fetch):
                    for argument in ("since", "last"):
                        with self.subTest(reader=open_reader.__name__, argument=argument):
                            with self.assertRaisesRegex(plasmite.UsageError, f"{argument} must fit"):
                                open_reader(**{argument: overflow})

                with self.assertRaises(plasmite.UsageError):
                    pool.fetch(since=(1 << 64) - 1)

    def test_feed_forwards_messages_and_closed_handles_raise_usage(self):
        with self._client_pool("forward") as (_, pool):
            source = pool.feed({"n": 1}, tags=["tag"])
            message = pool.feed(source)
            self.assertEqual(message.data, {"n": 1})
            self.assertEqual(message.tags, ["tag"])
            self.assertGreater(pool.feed(source.raw, format="lite3").seq, message.seq)
            with self.assertRaises(plasmite.UsageError):
                pool.feed(source, tags=[])
            pool.close()
            with self.assertRaises(plasmite.UsageError):
                pool.feed({"n": 2})

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
                with pool.fetch(since=1) as reader:
                    self.assertEqual(list(reader), [])

            with pool_path.open("r+b") as handle:
                handle.truncate(4)
            with self.assertRaises(plasmite.CorruptError) as corrupt:
                client.open_pool("damaged")
            self.assertEqual(corrupt.exception.path, str(pool_path))
            self.assertIsNone(corrupt.exception.seq)

    def test_udp_delivers_late_and_duplicate_frames(self) -> None:
        with self._new_client() as client:
            with self._new_pool(client, "udp-order") as pool:
                message = pool.feed({"value": 17}, ["tag"])
                payload = bytes(message.raw)
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
                        with pool.fetch(last=4) as follow:
                            events = [follow.next_event(wait=1) for _ in range(7)]
                            self.assertIsNone(follow.next_event(wait=0))
                frames = [event for event in events if isinstance(event, Message)]
                self.assertEqual([frame.seq for frame in frames], [base + 10, base + 12, base + 11, base + 12])
                for frame in frames:
                    self.assertEqual(frame.time, timestamp)
                    self.assertEqual(frame.raw, payload)
                    self.assertTrue(frame.raw.readonly)
                    self.assertEqual(frame.data, {"value": 17})
                    self.assertEqual(frame.tags, ["tag"])
                skips = [event for event in events if isinstance(event, UdpSkip)]
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
                original_lite3 = original.raw
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
                                json_message = follow.next(wait=5)
                                self.assertIsInstance(json_message, Message)
                                self.assertEqual(json_message.data, {"origin": "json", "value": 8})
                                self.assertEqual(json_message.tags, ["sensor"])
                                json_time = get_message(local_pool, json_message.seq).time
                                self.assertEqual(json_message.time, json_time)
                                self.assertGreater(json_message.time, 1 << 53)
                                stored_json = get_message(local_pool, json_message.seq)
                                self.assertEqual(stored_json.data, json_message.data)

                                mutable_lite3 = bytearray(original_lite3)
                                self.assertIsNone(udp_pool.feed(mutable_lite3, format="lite3"))
                                mutable_lite3[:] = b"\x00" * len(mutable_lite3)
                                lite3_frame = follow.next(wait=5)
                                self.assertEqual(lite3_frame.raw, original_lite3)
                                self.assertEqual(lite3_frame.data, original.data)
                                self.assertEqual(lite3_frame.tags, ["source"])
                                stored_lite3 = get_message(local_pool, lite3_frame.seq)
                                self.assertEqual(stored_lite3.data, original.data)

                            large_start = (1 << 53) + 1
                            with self.assertRaises(plasmite.UsageError):
                                udp_pool.fetch(since=large_start)

                            cancel_follow = udp_pool.follow()
                            self.assertIsNotNone(cancel_follow.next_event(wait=0))
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



if __name__ == "__main__":
    unittest.main()
