"""Direct producer semantics shared by the native and compatibility backends."""

import gc
import math
import unittest
from tempfile import TemporaryDirectory

import plasmite


class AppendViews(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory()
        self.client = plasmite.Client(self.directory.name)
        self.pool = self.client.create_pool("producer", 8 * 1024 * 1024)

    def tearDown(self):
        self.pool.close()
        self.client.close()
        self.directory.cleanup()

    def test_exact_values_and_retained_result(self):
        shared = {"value": 7}
        data = {
            "values": (None, True, False, -(1 << 63), (1 << 63) - 1, -0.0),
            "unicode": "é\0😀",
            "nested": [shared, shared],
        }
        message = self.pool.append_view(data, ["tag", "nul\0tag"])
        data["nested"][0]["value"] = 9
        self.assertEqual(message.data["nested"][0]["value"], 7)
        self.assertEqual(message.data["nested"][1]["value"], 7)
        self.assertEqual(message.data["unicode"], "é\0😀")
        self.assertEqual(message.data["values"][3], -(1 << 63))
        self.assertEqual(message.data["values"][4], (1 << 63) - 1)
        self.assertEqual(math.copysign(1, message.data["values"][-1]), -1)
        self.assertEqual(list(message.tags), ["tag", "nul\0tag"])
        self.assertEqual(self.pool.get_view(message.seq).data, message.data)
        child = message.data["nested"]
        del message
        self.pool.close()
        self.client.close()
        gc.collect()
        self.assertEqual(child[0]["value"], 7)

    def test_binary_inputs_are_snapshots(self):
        mutable = bytearray(b"mutable")
        buffer = memoryview(mutable)
        message = self.pool.append_view({"a": b"immutable", "b": mutable, "c": buffer})
        mutable[:] = b"changed"
        self.assertEqual(
            plasmite.to_native(message.data),
            {"a": b"immutable", "b": b"mutable", "c": b"mutable"},
        )
        self.assertTrue(message.data["b"].readonly)
        with self.assertRaises(BufferError):
            self.pool.append_view({"bad": memoryview(b"abcd")[::2]})

    def test_cycles_and_invalid_values_do_not_publish(self):
        first = self.pool.append_view({}).seq
        cycle = []
        cycle.append(cycle)
        for value in [
            {"cycle": cycle},
            {"number": 1 << 63},
            {"number": -(1 << 63) - 1},
            {"number": float("nan")},
            {"number": float("inf")},
            {"bad\0key": 1},
            {1: "key"},
            {"unicode": "\ud800"},
        ]:
            with self.assertRaises(
                (ValueError, OverflowError, TypeError, plasmite.PlasmiteError)
            ):
                self.pool.append_view(value)
        self.assertEqual(self.pool.append_view({}).seq, first + 1)

    def test_subclasses_reject_without_running_hooks(self):
        calls = []

        class CustomList(list):
            def __iter__(self):
                calls.append("iter")
                return super().__iter__()

        class CustomInt(int):
            def __int__(self):
                calls.append("int")
                return 7

        class CustomString(str):
            def encode(self, *args, **kwargs):
                calls.append("encode")
                return b"changed"

        for value in [CustomList([1]), CustomInt(2), CustomString("text"), object()]:
            with self.assertRaises(TypeError):
                self.pool.append_view({"custom": value})
        with self.assertRaises(TypeError):
            self.pool.append_view({CustomString("key"): 1})
        self.assertEqual(calls, [])
        # Existing JSON behavior stays available explicitly.
        self.assertEqual(self.pool.append({"custom": CustomInt(2)}).data["custom"], 2)

    def test_tag_iterator_can_close_handle_before_native_use(self):
        def tags():
            self.pool.close()
            yield "tag"

        from unittest.mock import patch

        from plasmite import _views

        with patch.object(_views.backend, "append") as native_append:
            with self.assertRaises(plasmite.PlasmiteError) as caught:
                self.pool.append_view({"value": 1}, tags())
            native_append.assert_not_called()
        self.assertEqual(caught.exception.kind, plasmite.ErrorKind.USAGE)
        self.assertIn("pool is closed", str(caught.exception))

    def test_durability_conversion_can_close_handle_before_native_use(self):
        pool = self.pool

        class ClosePool:
            def __int__(self):
                pool.close()
                return 0

        from unittest.mock import patch

        from plasmite import _views

        with patch.object(_views.backend, "append") as native_append:
            with self.assertRaises(plasmite.PlasmiteError) as caught:
                self.pool.append_view({"value": 1}, durability=ClosePool())
            native_append.assert_not_called()
        self.assertEqual(caught.exception.kind, plasmite.ErrorKind.USAGE)
        self.assertIn("pool is closed", str(caught.exception))

    def test_growth_runs_tag_iterable_once(self):
        calls = []

        def tags():
            calls.append("tags")
            yield "large"

        message = self.pool.append_view({"large": "x" * (1024 * 1024)}, tags())
        self.assertEqual(calls, ["tags"])
        self.assertEqual(len(message.data["large"]), 1024 * 1024)
        self.assertEqual(list(message.tags), ["large"])

    @unittest.skipIf(__import__("sys").platform == "win32", "uses Unix flock")
    def test_waiting_for_storage_does_not_hold_the_gil(self):
        import subprocess
        import sys

        code = r"""
import fcntl
from pathlib import Path
from tempfile import TemporaryDirectory
import threading
import time
import plasmite

with TemporaryDirectory() as directory, plasmite.Client(directory) as client:
    with client.pool("blocked", 1048576) as pool:
        with (Path(directory) / "blocked.plasmite").open("r+b") as file:
            fcntl.flock(file, fcntl.LOCK_EX)
            output = []
            worker = threading.Thread(target=lambda: output.append(pool.append_view({"value": 1})))
            worker.start()
            # The main Python thread must run while native append waits on flock.
            time.sleep(.05)
            assert not output
            fcntl.flock(file, fcntl.LOCK_UN)
            worker.join(2)
            assert not worker.is_alive() and output[0].data["value"] == 1
"""
        subprocess.run([sys.executable, "-c", code], check=True, timeout=5)

    def test_udp_conversion_snapshots_mutable_bytes(self):
        owner = self.pool.append_view({"value": 7})
        payload = bytearray(memoryview(owner))
        message = plasmite.UdpFrame(owner.seq, 1, memoryview(payload)).message()
        payload[:] = b"\0" * len(payload)
        self.assertIsInstance(message, plasmite.Message)
        self.assertEqual(message.data, {"value": 7})
        self.assertEqual(message.seq, owner.seq)

    def test_legacy_dataclass_stays_editable(self):
        legacy = self.pool.append({"value": 7})
        self.assertIsInstance(legacy, plasmite.Message)
        legacy.data["value"] = 9
        self.assertEqual(legacy.data["value"], 9)
        self.assertEqual(self.pool.get(legacy.seq).data["value"], 7)


if __name__ == "__main__":
    unittest.main()
