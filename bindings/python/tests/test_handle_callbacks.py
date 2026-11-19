"""Finish Python callbacks before borrowing a native handle."""

import ctypes
import unittest
from tempfile import TemporaryDirectory
from unittest.mock import patch

import plasmite


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
                    "plsm_udp_pool_feed_lite3",
                    lambda: pool.feed_lite3(b"unused", closing_int(pool.close)),
                )

    def test_append_durability(self):
        for method, symbol, payload in [
            ("feed_json", "plsm_pool_feed_json", b"{}"),
            ("feed_json_receipt", "plsm_pool_feed_json_receipt", b"{}"),
            ("feed_lite3", "plsm_pool_feed_lite3", b"unused"),
            ("feed_lite3_receipt", "plsm_pool_feed_lite3_receipt", b"unused"),
        ]:
            with self.subTest(method=method):
                pool = self.fresh_pool()
                self.assert_closed_before_native(
                    symbol,
                    lambda: getattr(pool, method)(
                        payload,
                        **({"tags": []} if "json" in method else {}),
                        durability=closing_int(pool.close),
                    ),
                )

    def test_append_tags(self):
        for method in ["feed_json", "feed_json_receipt"]:
            with self.subTest(method=method):
                pool = self.fresh_pool()
                self.assert_closed_before_native(
                    "plsm_pool_" + method,
                    lambda: getattr(pool, method)(
                        b"{}",
                        tags=closing_tags(pool.close),
                        durability=plasmite.Durability.FAST,
                    ),
                )

    def test_get_sequence_and_open_follow_options(self):
        for method, symbol in [
            ("_get_json", "plsm_pool_fetch"),
            ("_get_lite3", "plsm_pool_fetch_lite3"),
        ]:
            with self.subTest(method=method):
                pool = self.fresh_pool()
                self.assert_closed_before_native(
                    symbol, lambda: getattr(pool, method)(closing_int(pool.close))
                )
        for method, symbol in [
            ("follow", "plsm_pool_follow"),
            ("follow_lite3", "plsm_pool_follow_lite3"),
        ]:
            with self.subTest(method=method):
                pool = self.fresh_pool()
                self.assert_closed_before_native(
                    symbol, lambda: getattr(pool, method)(since=closing_int(pool.close))
                )

    def test_stream_timeout(self):
        for method, symbol in [
            ("follow", "plsm_stream_next_wait"),
            ("follow_lite3", "plsm_lite3_stream_next_wait"),
        ]:
            with self.subTest(method=method):
                with getattr(self.pool, method)() as reader:
                    stream = reader._stream
                    self.assert_closed_before_native(
                        symbol, lambda: stream.next_wait(closing_int(stream.close))
                    )

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
                "plsm_udp_pool_feed_json",
                "tags",
            ),
            (
                plasmite.UdpPool,
                plasmite.plsm_udp_pool_t,
                "follow",
                "plsm_udp_pool_read",
                "read_tags",
            ),
            (
                plasmite.UdpFollow,
                plasmite.plsm_udp_follow_t,
                "next_event",
                "plsm_udp_follow_next",
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
        self.assertEqual(
            self.pool.feed_json(Payload(b"{}"), [], plasmite.Durability.FAST),
            self.pool._get_json(1),
        )


if __name__ == "__main__":
    unittest.main()
