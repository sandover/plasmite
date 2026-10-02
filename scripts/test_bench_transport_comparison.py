#!/usr/bin/env python3
"""Check benchmark grant cleanup without opening a socket or issuing a grant."""

from contextlib import ExitStack
import argparse
import http.client
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from bench_transport_comparison import HttpsMcp, PROTOCOL_VERSION, run


class GrantCleanupTests(unittest.TestCase):
    def setUp(self):
        self.patches = ExitStack()
        self.addCleanup(self.patches.close)
        self.connection = Mock()
        self.patches.enter_context(patch("http.client.HTTPSConnection", return_value=self.connection))
        self.authorize = self.patches.enter_context(patch.object(
            HttpsMcp, "authorize", return_value=("test-client", "https://example.invalid/mcp", "test-grant")))
        self.initialize = self.patches.enter_context(patch.object(
            HttpsMcp, "call", return_value={"protocolVersion": PROTOCOL_VERSION}))
        self.notify = self.patches.enter_context(patch.object(HttpsMcp, "notify_initialized"))
        self.revoke = self.patches.enter_context(patch.object(
            HttpsMcp, "request_form", return_value=(200, {}, None)))

    def connect(self):
        return HttpsMcp("https://example.invalid/mcp", "test-key", None)

    def assert_revoked(self):
        self.revoke.assert_called_once_with(
            "/oauth/revoke", {"client_id": "test-client", "token": "test-grant"})
        self.connection.close.assert_called_once()

    def test_authorization_failure_closes_connection_without_revocation(self):
        self.authorize.side_effect = RuntimeError("authorization failed")
        with self.assertRaisesRegex(RuntimeError, "authorization failed"):
            self.connect()
        self.revoke.assert_not_called()
        self.connection.close.assert_called_once()

    def test_initialize_failure_revokes_grant(self):
        self.initialize.side_effect = RuntimeError("initialize failed")
        with self.assertRaisesRegex(RuntimeError, "initialize failed"):
            self.connect()
        self.assert_revoked()

    def test_protocol_mismatch_revokes_grant(self):
        self.initialize.return_value = {"protocolVersion": "unexpected"}
        with self.assertRaisesRegex(RuntimeError, "unexpected protocol version"):
            self.connect()
        self.assert_revoked()

    def test_initialized_notification_failure_revokes_grant(self):
        self.notify.side_effect = RuntimeError("notification failed")
        with self.assertRaisesRegex(RuntimeError, "notification failed"):
            self.connect()
        self.assert_revoked()

    def test_interrupted_setup_revokes_grant(self):
        self.initialize.side_effect = KeyboardInterrupt()
        with self.assertRaises(KeyboardInterrupt):
            self.connect()
        self.assert_revoked()

    def test_failed_revocation_is_reported_and_connection_closed(self):
        client = self.connect()
        self.revoke.return_value = (500, {}, None)
        with self.assertRaisesRegex(RuntimeError, "revocation failed with HTTP 500"):
            client.close()
        self.assert_revoked()

    def test_revocation_transport_failure_is_reported_and_connection_closed(self):
        client = self.connect()
        self.revoke.side_effect = http.client.RemoteDisconnected("connection lost")
        with self.assertRaises(http.client.RemoteDisconnected):
            client.close()
        self.assert_revoked()

    def test_successful_cleanup_does_not_revoke_twice(self):
        client = self.connect()
        self.connection.close.assert_not_called()
        self.revoke.assert_not_called()
        client.close()
        client.close()
        self.revoke.assert_called_once()
        self.assertEqual(self.connection.close.call_count, 2)


class RunnerCleanupTests(unittest.TestCase):
    def setUp(self):
        self.patches = ExitStack()
        self.addCleanup(self.patches.close)
        directory = self.patches.enter_context(tempfile.TemporaryDirectory())
        key_file = Path(directory) / "test-key"
        key_file.write_text("test-key", encoding="utf-8")
        key_file.chmod(0o600)
        self.args = argparse.Namespace(access_key_file=str(key_file), server="https://example.invalid",
                                       pool="test", mcp_url="https://example.invalid/mcp", ca_file=None)
        self.native = Mock()
        self.native.poll.return_value = None
        self.local = Mock()
        self.local.tool.side_effect = RuntimeError("injected benchmark failure")
        self.direct = Mock()
        self.direct.server_info = {}
        self.commands = self.patches.enter_context(patch("subprocess.run"))
        self.patches.enter_context(patch("subprocess.Popen", return_value=self.native))
        self.patches.enter_context(patch("bench_transport_comparison.native_request",
                                        return_value={"result": {"file_size": 2_097_152}}))
        self.patches.enter_context(patch("bench_transport_comparison.StdioMcp", return_value=self.local))
        self.patches.enter_context(patch("bench_transport_comparison.HttpsMcp", return_value=self.direct))
        self.patches.enter_context(patch("bench_transport_comparison.command_output", return_value="test"))

    def assert_disconnected(self):
        command = self.commands.call_args.args[0]
        self.assertEqual(command[1:], ["access", "disconnect", self.args.server])

    def test_failed_revocation_does_not_skip_other_cleanup(self):
        self.direct.close.side_effect = RuntimeError("revocation failed")
        with self.assertRaisesRegex(RuntimeError, "cleanup failed: HTTPS MCP: revocation failed"):
            run(self.args)
        self.local.close.assert_called_once()
        self.native.stdin.close.assert_called_once()
        self.native.wait.assert_called_once_with(timeout=5)
        self.assert_disconnected()

    def test_native_timeout_kills_worker_and_still_disconnects(self):
        self.native.wait.side_effect = [subprocess.TimeoutExpired("test-worker", 5), None]
        with self.assertRaisesRegex(RuntimeError, "cleanup failed: native worker"):
            run(self.args)
        self.direct.close.assert_called_once()
        self.local.close.assert_called_once()
        self.native.kill.assert_called_once()
        self.assertEqual(self.native.wait.call_count, 2)
        self.assert_disconnected()


if __name__ == "__main__":
    unittest.main()
