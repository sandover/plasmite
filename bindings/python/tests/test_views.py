"""Run with native wheels and with PLASMITE_PYTHON_VIEWS=0 for fallback parity."""

import gc
import json
import math
import unittest
from collections.abc import Mapping, Sequence
from tempfile import TemporaryDirectory

import plasmite


class Views(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory()
        self.client = plasmite.Client(self.directory.name)
        self.pool = self.client.create_pool("views", 1024 * 1024)
        self.data = {
            "power": 7,
            "nested": {"x": "hello"},
            "samples": [1, {"x": 3}, 4],
            "null": None,
            "large": 9223372036854775807,
            "zero": -0.0,
            "__proto__": {"value": 2},
            "to_native": "payload",
        }
        self.message = self.pool.get_view(self.pool.append(self.data).seq)

    def tearDown(self):
        self.pool.close()
        self.client.close()
        self.directory.cleanup()

    def test_protocols_and_missing(self):
        data = self.message.data
        self.assertIsInstance(data, Mapping)
        self.assertIsInstance(data["samples"], Sequence)
        self.assertEqual(data, self.data)
        self.assertEqual(dict(data.items()), self.data)
        self.assertEqual(set(data.keys()), set(self.data))
        self.assertEqual(len(list(data.values())), len(data))
        self.assertEqual(data.get("absent", 42), 42)
        self.assertIsNone(data["null"])
        self.assertNotIn("missing\0key", data)
        self.assertNotIn(1, data)
        self.assertNotIn("bad\ud800", data)
        with self.assertRaises(KeyError):
            _ = data["absent"]
        with self.assertRaises(TypeError):
            data["power"] = 9
        with self.assertRaises(TypeError):
            hash(data)
        self.assertLess(len(repr(data)), 100)

    def test_arrays_and_slices(self):
        data = self.message.data["samples"]
        self.assertEqual(data[-1], 4)
        self.assertEqual(data, self.data["samples"])
        self.assertNotEqual(data, tuple(self.data["samples"]))
        for key in [
            slice(None, 2),
            slice(None, None, -1),
            slice(1, None),
            slice(0, 0),
            slice(None, None, 10**100),
        ]:
            self.assertEqual(plasmite.to_native(data[key]), self.data["samples"][key])
            self.assertEqual(
                json.loads(plasmite.to_json(data[key])), self.data["samples"][key]
            )
        self.assertEqual(plasmite.to_native(data[:: 10**100][:: 10**100]), [1])
        for index in [3, 1 << 100, -(1 << 100)]:
            with self.assertRaises(IndexError):
                _ = data[index]
        with self.assertRaises(TypeError):
            data[0] = 2

    def test_conversion_and_buffer_lifetime(self):
        buffer = memoryview(self.message)
        self.assertTrue(buffer.readonly)
        child = self.message.data["nested"]
        editable = plasmite.to_native(self.message.data)
        editable["nested"]["x"] = "changed"
        self.assertEqual(child["x"], "hello")
        envelope = plasmite.to_native(self.message)
        self.assertEqual(envelope["data"], self.data)
        self.assertEqual(json.loads(plasmite.to_json(self.message)), envelope)
        self.assertEqual(
            json.loads(json.dumps(self.message.data, default=plasmite.to_native)),
            self.data,
        )
        payload = bytes(buffer)
        rebuilt = plasmite.view_from_lite3(
            bytearray(payload), self.message.seq, self.message.timestamp_ns
        )
        self.assertEqual(rebuilt.data, self.data)
        self.pool.close()
        self.client.close()
        self.message = None
        gc.collect()
        self.assertEqual(child["x"], "hello")
        self.assertEqual(bytes(buffer[1:]), payload[1:])
        with self.assertRaises(TypeError):
            buffer[0] = 0

    def test_derived_buffer_is_the_last_owner(self):
        base = memoryview(self.message)
        expected = bytes(base)[1:]
        derived = base[1:]
        del base
        self.message = None
        self.pool.close()
        self.client.close()
        gc.collect()
        self.assertEqual(derived.tobytes(), expected)
        self.assertTrue(derived.readonly)
        with self.assertRaises(TypeError):
            derived[0] = 0

    def test_child_keeps_only_its_snapshot_alive(self):
        import sys

        from plasmite import _views

        message = self.pool.get_view(self.message.seq)
        before = sys.getrefcount(message) if _views.VIEW_BACKEND == "native" else None
        child = message.data["nested"]
        if before is not None:
            self.assertEqual(sys.getrefcount(message), before + 1)
        del message
        self.pool.close()
        gc.collect()
        self.assertEqual(child["x"], "hello")

    def test_exact_scalars_and_overwrite(self):
        self.assertEqual(self.message.data["large"], 9223372036854775807)
        self.assertEqual(math.copysign(1, self.message.data["zero"]), -1)
        payload = {"padding": "x" * 32768}
        for _ in range(40):
            self.pool.append(payload)
        self.assertEqual(self.message.data["nested"]["x"], "hello")

    def test_native_byte_leaf_retains_readonly_owner(self):
        import ctypes

        from plasmite import _views_fallback as ffi

        ffi._configure()
        seq = self.pool.append({"blob": "abc"}).seq
        owner = ffi.get(ctypes.cast(self.pool._ptr, ctypes.c_void_p).value, seq)
        ref = ffi.Ref()
        ffi._call(
            ffi.lib.plsm_value_get, owner.data._ref, b"blob", 4, ctypes.byref(ref)
        )
        encoded = bytearray(memoryview(owner))
        self.assertEqual(encoded[ref.offset], 5)
        encoded[ref.offset] = 4  # STRING to BYTES: retain terminator as ordinary data.
        message = plasmite.view_from_lite3(encoded, seq, 1)
        blob = message.data["blob"]
        self.assertTrue(blob.readonly)
        self.assertEqual(plasmite.to_native(blob), b"abc\0")
        self.assertEqual(json.loads(plasmite.to_json(blob)), "YWJjAA==")
        self.assertEqual(plasmite.to_native(message.data), {"blob": b"abc\0"})
        self.assertEqual(
            json.loads(plasmite.to_json(message.data)), {"blob": "YWJjAA=="}
        )
        del message
        gc.collect()
        self.assertEqual(blob.tobytes(), b"abc\0")

    def test_canonical_message_export_preserves_payload_and_known_metadata(self):
        from pathlib import Path

        payload = (Path(__file__).parent / "fixtures" / "extra-meta.lite3").read_bytes()
        message = plasmite.view_from_lite3(payload, 7, 123400000)
        self.assertEqual(plasmite.to_native(message.meta), {"tags": [], "extra": 7})
        expected = {
            "seq": 7,
            "time": "1970-01-01T00:00:00.1234Z",
            "meta": {"tags": []},
            "data": {"value": 1},
        }
        self.assertEqual(plasmite.to_native(message), expected)
        self.assertEqual(json.loads(plasmite.to_json(message)), expected)

    def test_canonical_timestamp_keeps_exact_nanoseconds(self):
        payload = bytes(memoryview(self.message))
        for timestamp in [0, 1, 100, 123400000, 123456789]:
            message = plasmite.view_from_lite3(payload, 1, timestamp)
            self.assertEqual(message.timestamp_ns, timestamp)
            self.assertEqual(
                json.loads(plasmite.to_json(message))["time"], message.time_rfc3339
            )

    def test_validates_and_isolates_bytes(self):
        payload = bytearray(memoryview(self.message))
        view = plasmite.view_from_lite3(payload, 1, 2)
        payload[:] = b"\0" * len(payload)
        self.assertEqual(view.data, self.data)
        with self.assertRaises(plasmite.PlasmiteError):
            plasmite.view_from_lite3(b"bad")
        for seq in [-1, 1 << 64, "1"]:
            with self.assertRaises(ValueError):
                self.pool.get_view(seq)


if __name__ == "__main__":
    unittest.main()
