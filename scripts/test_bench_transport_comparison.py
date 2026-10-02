#!/usr/bin/env python3
"""Check benchmark grant cleanup without opening a socket or issuing a grant."""

from contextlib import ExitStack
import http.client
import unittest
from unittest.mock import Mock, patch

from bench_transport_comparison import HttpsMcp, PROTOCOL_VERSION


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


if __name__ == "__main__":
    unittest.main()
