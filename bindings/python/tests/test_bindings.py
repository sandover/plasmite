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
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
LIB_DIR = os.environ.get("PLASMITE_LIB_DIR") or str(REPO_ROOT / "target" / "debug")
if "PLASMITE_LIB_DIR" not in os.environ:
    os.environ["PLASMITE_LIB_DIR"] = LIB_DIR

from plasmite import Client, Durability, PlasmiteError, parse_message


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
        descrips = ["alpha", "beta", "gamma"]
        msg_bytes = pool.append_json(
            json.dumps(payload).encode("utf-8"), descrips, Durability.FAST
        )
        message = parse_message(msg_bytes)
        self.assertEqual(len(message["data"]["blob"]), len(payload["blob"]))
        self.assertEqual(message["meta"]["descrips"], descrips)

        get_bytes = pool.get_json(message["seq"])
        fetched = parse_message(get_bytes)
        self.assertEqual(len(fetched["data"]["blob"]), len(payload["blob"]))

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

    def test_closed_handles_error(self) -> None:
        client = Client(str(self.pool_dir))
        pool = client.create_pool("closed", 1024 * 1024)
        pool.close()

        with self.assertRaises(PlasmiteError):
            pool.append_json(b"{}", [], Durability.FAST)

        client.close()
        with self.assertRaises(PlasmiteError):
            client.create_pool("oops", 1024 * 1024)


if __name__ == "__main__":
    unittest.main()
