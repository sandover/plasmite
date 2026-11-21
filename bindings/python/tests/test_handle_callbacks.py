"""Finish Python callbacks before borrowing a native handle."""

import ctypes
import unittest
from tempfile import TemporaryDirectory
from unittest.mock import patch

import plasmite
from plasmite import _views


def closing_int(close):
    class Number(int):
        def __lt__(self, other):
            close()
            return int.__lt__(self, other)

        def __int__(self):
            close()
            return 0

        def __bool__(self):
            close()
            return True

    return Number(1)


def closing_tags(close):
    class Tags:
        def __iter__(self):
            close()
            return iter(["tag"])

    return Tags()


class HandleCallbacks(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory()
        self.client = plasmite.Client(self.directory.name)
        self.pool = self.client.create_pool("callbacks", 1024 * 1024)

    def tearDown(self):
        self.pool.close()
        self.client.close()
        self.directory.cleanup()

    def assert_closed_before_native(self, symbol, call):
        with patch.object(plasmite._LIB, symbol) as native:
            with self.assertRaisesRegex(plasmite.PlasmiteError, "closed"):
                call()
            native.assert_not_called()

    def fresh_pool(self):
        self.pool.close()
        self.pool = self.client.open_pool("callbacks")
        return self.pool

    def test_client_and_udp_arguments_finish_before_handle_check(self):
        with plasmite.Client(self.directory.name) as client:
            self.assert_closed_before_native(
                "plsm_pool_open",
                lambda: client.open_pool("callbacks", create=closing_int(client.close)),
            )

        with plasmite.Client(self.directory.name) as client:
            class PoolName(str):
                def encode(self, *args, **kwargs):
                    client.close()
                    return super().encode(*args, **kwargs)

            self.assert_closed_before_native(
                "plsm_pool_delete", lambda: client.delete_pool(PoolName("callbacks"))
            )

        with plasmite.UdpClient("udp://127.0.0.1:9701") as client:
            with client.open_pool("callbacks") as pool:
                self.assert_closed_before_native(
                    "plsm_internal_udp_pool_feed_lite3",
                    lambda: pool.feed(b"unused", durability=closing_int(pool.close), format="lite3"),
                )

    def test_append_durability(self):
        for format, payload in [("json", b"{}"), ("lite3", b"unused")]:
            with self.subTest(format=format):
                pool = self.fresh_pool()
                self.assert_closed_before_native(
                    "plsm_pool_feed",
                    lambda: pool.feed(payload, format=format, durability=closing_int(pool.close)),
                )

    def test_append_tags(self):
        pool = self.fresh_pool()
        self.assert_closed_before_native(
            "plsm_pool_feed",
            lambda: pool.feed(b"{}", format="json", tags=closing_tags(pool.close)),
        )

    def test_open_read_options(self):
        for method, symbol in [("fetch", "plsm_pool_fetch"), ("follow", "plsm_pool_follow")]:
            with self.subTest(method=method):
                pool = self.fresh_pool()
                self.assert_closed_before_native(
                    symbol, lambda: getattr(pool, method)(since=closing_int(pool.close))
                )

    def test_stream_timeout_validation(self):
        with self.pool.follow() as reader, patch.object(_views.backend, "next_message") as native:
            with self.assertRaises(plasmite.UsageError):
                reader.next(closing_int(reader.close))
            native.assert_not_called()

    def test_client_reference(self):
        client = self.client

        class Reference(str):
            def encode(self, *args, **kwargs):
                client.close()
                return b"callbacks"

        for method, symbol in [
            ("open_pool", "plsm_pool_open"),
            ("create_pool", "plsm_pool_create"),
        ]:
            with self.subTest(method=method):
                self.client = client = plasmite.Client(self.directory.name)
                args = (Reference("callbacks"),) + (
                    (1024 * 1024,) if method == "create_pool" else ()
                )
                self.assert_closed_before_native(
                    symbol, lambda: getattr(client, method)(*args)
                )

    def test_udp_callbacks_without_network(self):
        # No native UDP handle exists: mocks make accidental boundary calls fail safely.
        for cls, pointer_type, method, symbol, args in [
            (
                plasmite.UdpClient,
                plasmite.plsm_udp_client_t,
                "open_pool",
                "plsm_udp_client_open_pool",
                "name",
            ),
            (
                plasmite.UdpPool,
                plasmite.plsm_udp_pool_t,
                "feed",
                "plsm_internal_udp_pool_feed_json",
                "tags",
            ),
            (
                plasmite.UdpPool,
                plasmite.plsm_udp_pool_t,
                "follow",
                "plsm_internal_udp_pool_read",
                "read_tags",
            ),
            (
                plasmite.UdpFollow,
                plasmite.plsm_udp_follow_t,
                "next_event",
                "plsm_udp_follow_next_view",
                "timeout",
            ),
        ]:
            with self.subTest(method=method):
                handle = cls.__new__(cls)
                if cls is plasmite.UdpClient:
                    handle._lock = plasmite.RLock()
                    handle._ptr = ctypes.cast(1, ctypes.POINTER(pointer_type))
                else:
                    cls.__init__(handle, ctypes.cast(1, ctypes.POINTER(pointer_type)))
                handle.close = lambda: setattr(handle, "_ptr", None)
                if args == "name":

                    class Name(str):
                        def encode(self, *unused):
                            handle.close()
                            return b"pool"

                    call = lambda: handle.open_pool(Name("pool"))
                elif args == "tags":
                    call = lambda: handle.feed({}, tags=closing_tags(handle.close))
                elif args == "read_tags":
                    call = lambda: handle.follow(tags=closing_tags(handle.close))
                else:
                    with patch.object(plasmite._LIB, symbol) as native:
                        with self.assertRaises(plasmite.UsageError):
                            handle.next_event(object())
                        native.assert_not_called()
                    handle.close()
                    continue
                self.assert_closed_before_native(symbol, call)
                handle.close()

    def test_bytes_subclass_uses_physical_extent(self):
        class Payload(bytes):
            def __len__(self):
                return 100000

            def __bytes__(self):
                raise AssertionError(
                    "Legacy bytes inputs must not acquire a new conversion hook"
                )

        value = plasmite._ensure_bytes(Payload(b"{}"), "payload")
        self.assertIs(type(value), bytes)
        self.assertEqual(value, b"{}")
        sent = self.pool.feed(Payload(b"{}"), format="json")
        with self.pool.fetch(since=sent.seq) as reader:
            self.assertEqual(next(reader).raw, sent.raw)


if __name__ == "__main__":
    unittest.main()
