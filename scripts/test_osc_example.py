"""Check OSC capture and the cookbook's persistent feed pipe.

Run: uv run --with python-osc==1.10.2 python scripts/test_osc_example.py
Requires target/debug/plasmite (or PLASMITE_BIN).
"""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import time
import threading
import unittest

from pythonosc.udp_client import SimpleUDPClient
from pythonosc.osc_bundle_builder import OscBundleBuilder
from pythonosc.osc_message_builder import OscMessageBuilder

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "examples" / "osc_to_json.py"
SPEC = importlib.util.spec_from_file_location("osc_to_json", SCRIPT)
EXAMPLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXAMPLE)


def lines_from(stream):
    lines = queue.Queue()
    def read():
        with stream:
            for line in stream:
                lines.put(line)
    threading.Thread(target=read, daemon=True).start()
    return lines


def stop(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)


class OscExampleTests(unittest.TestCase):
    def test_json_values_and_unsupported_arguments(self):
        output, errors = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            EXAMPLE.emit_message("/controls", 7, 0.75, "café", True, None, [1, 2])
            EXAMPLE.emit_message("/blob", b"binary")
            EXAMPLE.emit_message("/nan", float("nan"))
        self.assertEqual(json.loads(output.getvalue()), {
            "address": "/controls", "args": [7, 0.75, "café", True, None, [1, 2]],
        })
        self.assertIn("Skipping /blob", errors.getvalue())
        self.assertIn("Skipping /nan", errors.getvalue())

    def test_receiver_pipes_live_messages_into_pool(self):
        binary = os.environ.get("PLASMITE_BIN", str(ROOT / "target" / "debug" / "plasmite"))
        with tempfile.TemporaryDirectory(prefix="plasmite-osc-") as directory:
            command = [binary, "--dir", directory]
            subprocess.run(command + ["pool", "create", "osc"], check=True, capture_output=True)
            receiver = subprocess.Popen(
                [sys.executable, str(SCRIPT), "--port", "0"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            self.addCleanup(stop, receiver)
            ready = lines_from(receiver.stderr).get(timeout=10)
            self.assertIn("Listening for OSC on 127.0.0.1:", ready)
            port = int(ready.split(":")[1].split(";")[0])
            feeder = subprocess.Popen(
                command + ["feed", "osc", "--data", "--tag", "osc", "--format", "json"],
                stdin=receiver.stdout, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            self.addCleanup(stop, feeder)
            self.addCleanup(feeder.stderr.close)
            receiver.stdout.close()
            receipts = lines_from(feeder.stdout)
            subprocess.run(
                [sys.executable, str(SCRIPT), "--port", str(port), "--send-test"],
                check=True, capture_output=True, timeout=10,
            )
            self.assertEqual(json.loads(receipts.get(timeout=10))["seq"], 1)
            with contextlib.closing(SimpleUDPClient("127.0.0.1", port)) as client:
                client.send_message("/label", "hello ☃")
            self.assertEqual(json.loads(receipts.get(timeout=10))["seq"], 2)
            bundle = OscBundleBuilder(time.time() + 3600)
            message = OscMessageBuilder(address="/future")
            message.add_arg(9)
            bundle.add_content(message.build())
            with contextlib.closing(SimpleUDPClient("127.0.0.1", port)) as client:
                client.send(bundle.build())
            self.assertEqual(json.loads(receipts.get(timeout=10))["seq"], 3)
            self.assertIsNone(receiver.poll())
            self.assertIsNone(feeder.poll())
            stored = subprocess.run(
                command + ["follow", "osc", "--tail", "3", "--no-follow", "--format", "json"],
                check=True, capture_output=True, text=True, timeout=10,
            )
            messages = [json.loads(line) for line in stored.stdout.splitlines()]
            self.assertEqual([item["data"] for item in messages], [
                {"address": "/fader/1", "args": [0.75]},
                {"address": "/label", "args": ["hello ☃"]},
                {"address": "/future", "args": [9]},
            ])
            self.assertEqual([item["meta"]["tags"] for item in messages], [["osc"], ["osc"], ["osc"]])
            filtered = subprocess.run(
                command + ["follow", "osc", "--tail", "3", "--no-follow", "--format", "json",
                           "--where", '.data.address == "/fader/1"'],
                check=True, capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(json.loads(filtered.stdout)["data"]["args"], [0.75])
            stop(receiver)
            self.assertEqual(feeder.wait(timeout=10), 0)


if __name__ == "__main__":
    unittest.main()
