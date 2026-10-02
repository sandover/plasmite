#!/usr/bin/env python3
"""Compare the long-lived native API and direct HTTPS MCP."""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.client
from html.parser import HTMLParser
import json
import os
import platform
import secrets
import socket
import ssl
import statistics
import subprocess
import sys
import time
from pathlib import Path
from urllib.parse import parse_qs, urlencode, urlsplit


PROTOCOL_VERSION = "2025-11-25"
LANES = ("native_api", "https_mcp")


def command_output(*command: str) -> str:
    try:
        return subprocess.run(command, check=True, capture_output=True, text=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unavailable"


class HttpsMcp:
    def __init__(self, endpoint: str, access_key: str, ca_file: str | None):
        parts = urlsplit(endpoint)
        if parts.scheme != "https" or not parts.hostname or parts.query or parts.fragment:
            raise ValueError("--mcp-url must be an HTTPS URL without query or fragment")
        if parts.path.rstrip("/") != "/mcp":
            raise ValueError("--mcp-url must name the exact /mcp endpoint")
        self.path = parts.path or "/mcp"
        self.host = parts.hostname
        self.port = parts.port or 443
        self.context = ssl.create_default_context(cafile=ca_file)
        self.connection = http.client.HTTPSConnection(
            self.host, self.port, context=self.context, timeout=60
        )
        self.next_id = 1
        self.refresh_token = ""
        try:
            self.client_id, self.resource, self.refresh_token = self.authorize(access_key)
            self.expires_at = time.monotonic() + 840
            self.refresh_count = 0
            initialized = self.call("initialize", {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "plasmite-transport-benchmark", "version": "1"},
            })
            if initialized.get("protocolVersion") != PROTOCOL_VERSION:
                raise RuntimeError(f"HTTPS MCP negotiated unexpected protocol version: {initialized.get('protocolVersion')}")
            self.notify_initialized()
            self.server_info = initialized.get("serverInfo", {})
        except BaseException:
            self.close()
            raise

    def request_json(self, path: str, method: str, payload: dict[str, object],
                     headers: dict[str, str] | None = None) -> tuple[int, dict[str, object], http.client.HTTPResponse]:
        body = json.dumps(payload, separators=(",", ":"))
        request_headers = {"Content-Type": "application/json", "Accept": "application/json"}
        if headers:
            request_headers.update(headers)
        self.connection.request(method, path, body=body, headers=request_headers)
        response = self.connection.getresponse()
        raw = response.read()
        value = json.loads(raw) if raw else {}
        return response.status, value, response

    def request_form(self, path: str, values: dict[str, str],
                     headers: dict[str, str] | None = None) -> tuple[int, dict[str, object], http.client.HTTPResponse]:
        body = urlencode(values)
        request_headers = {"Content-Type": "application/x-www-form-urlencoded", "Accept": "application/json"}
        if headers:
            request_headers.update(headers)
        self.connection.request("POST", path, body=body, headers=request_headers)
        response = self.connection.getresponse()
        raw = response.read()
        try:
            value = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            value = {}
        return response.status, value, response

    def authorize(self, access_key: str) -> tuple[str, str, str]:
        issuer = f"https://{self.host}:{self.port}" if self.port != 443 else f"https://{self.host}"
        self.resource = f"{issuer}/mcp"
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            redirect_uri = f"http://127.0.0.1:{listener.getsockname()[1]}/benchmark-callback"

        status, registration, _ = self.request_json(
            "/oauth/register", "POST",
            {"client_name": "Plasmite transport benchmark", "redirect_uris": [redirect_uri],
             "token_endpoint_auth_method": "none"},
        )
        if status != 201 or not registration.get("client_id"):
            raise RuntimeError(f"OAuth dynamic client registration failed with HTTP {status}: {registration}")
        client_id = registration["client_id"]
        verifier = secrets.token_urlsafe(48)
        challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
        state = secrets.token_urlsafe(24)
        query = urlencode({
            "response_type": "code",
            "client_id": client_id,
            "redirect_uri": redirect_uri,
            "code_challenge": challenge,
            "code_challenge_method": "S256",
            "resource": self.resource,
            "state": state,
        })
        self.connection.request("GET", f"/oauth/authorize?{query}", headers={"Accept": "text/html"})
        page = self.connection.getresponse()
        html = page.read().decode("utf-8", "replace")
        if page.status != 200:
            raise RuntimeError(f"OAuth authorization request failed with HTTP {page.status}")
        cookie = page.getheader("Set-Cookie", "").split(";", 1)[0]
        request_id = AuthorizationRequestId().parse(html)
        if not cookie.startswith("plasmite_oauth=") or not request_id:
            raise RuntimeError("OAuth authorization page did not return its approval cookie and request id")

        print(
            f"Authorizing benchmark client for {self.resource} using a one-time DCR + PKCE grant.",
            file=sys.stderr,
        )
        status, approval, response = self.request_form(
            "/oauth/approve", {"request": request_id, "access_key": access_key},
            {"Cookie": cookie, "Origin": issuer},
        )
        location = response.getheader("Location", "")
        if status not in (302, 303) or not location:
            raise RuntimeError(f"OAuth approval failed with HTTP {status}: {approval}")
        callback = urlsplit(location)
        callback_values = parse_qs(callback.query)
        if callback_values.get("state") != [state] or callback_values.get("iss") != [issuer]:
            raise RuntimeError("OAuth approval callback did not match the benchmark request")
        code = callback_values.get("code", [None])[0]
        if not code:
            raise RuntimeError("OAuth approval callback did not include a code")
        status, tokens, _ = self.request_form(
            "/oauth/token", {"grant_type": "authorization_code", "client_id": client_id,
             "code": code, "redirect_uri": redirect_uri, "code_verifier": verifier,
             "resource": self.resource},
        )
        if status != 200 or not tokens.get("access_token") or not tokens.get("refresh_token"):
            raise RuntimeError(f"OAuth code exchange failed with HTTP {status}: {tokens}")
        self.bearer = tokens["access_token"]
        self.refresh_expiry_seconds = int(tokens.get("expires_in", 900))
        return client_id, self.resource, tokens["refresh_token"]

    def refresh_if_needed(self) -> None:
        if time.monotonic() < self.expires_at - 60:
            return
        status, tokens, _ = self.request_form(
            "/oauth/token", {"grant_type": "refresh_token", "client_id": self.client_id,
             "refresh_token": self.refresh_token, "resource": self.resource},
        )
        if status != 200 or not tokens.get("access_token") or not tokens.get("refresh_token"):
            raise RuntimeError(f"OAuth token refresh failed with HTTP {status}: {tokens}")
        self.bearer = tokens["access_token"]
        self.refresh_token = tokens["refresh_token"]
        self.expires_at = time.monotonic() + int(tokens.get("expires_in", 900))
        self.refresh_count += 1

    def call(self, method: str, params: dict[str, object]) -> dict[str, object]:
        self.refresh_if_needed()
        request_id = self.next_id
        self.next_id += 1
        headers = {"Authorization": f"Bearer {self.bearer}"}
        if method != "initialize":
            headers["MCP-Protocol-Version"] = PROTOCOL_VERSION
        status, value, _ = self.request_json(
            self.path, "POST", {"jsonrpc": "2.0", "id": request_id,
                                 "method": method, "params": params}, headers
        )
        if status != 200:
            raise RuntimeError(f"HTTPS MCP {method} returned HTTP {status}: {value}")
        if "error" in value:
            raise RuntimeError(f"HTTPS MCP {method} failed: {value['error']}")
        if value.get("id") != request_id:
            raise RuntimeError(f"HTTPS MCP returned mismatched request id for {method}")
        return value["result"]

    def notify_initialized(self) -> None:
        headers = {"Authorization": f"Bearer {self.bearer}",
                   "MCP-Protocol-Version": PROTOCOL_VERSION}
        status, _, _ = self.request_json(
            self.path, "POST",
            {"jsonrpc": "2.0", "method": "notifications/initialized"}, headers,
        )
        if status not in (200, 202, 204):
            raise RuntimeError(f"HTTPS MCP initialized notification returned HTTP {status}")

    def tool(self, name: str, arguments: dict[str, object]) -> dict[str, object]:
        result = self.call("tools/call", {"name": name, "arguments": arguments})
        if result.get("isError"):
            raise RuntimeError(f"HTTPS MCP tool {name} failed: {result.get('content')}")
        return result["structuredContent"]

    def close(self) -> None:
        try:
            if self.refresh_token:
                status, _, _ = self.request_form(
                    "/oauth/revoke", {"client_id": self.client_id, "token": self.refresh_token}
                )
                if status != 200:
                    raise RuntimeError(f"OAuth token revocation failed with HTTP {status}")
                self.refresh_token = ""
        finally:
            self.connection.close()


class AuthorizationRequestId(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.request_id: str | None = None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag != "input":
            return
        fields = dict(attrs)
        if fields.get("name") == "request":
            self.request_id = fields.get("value")

    def parse(self, html: str) -> str | None:
        self.feed(html)
        return self.request_id


def serialized_payload(size: int, run: int, index: int, lane: str) -> dict[str, str]:
    prefix = f"bench|r={run:04d}|n={index:06d}|lane={lane}|"
    empty_size = len(json.dumps({"payload": ""}, separators=(",", ":")).encode())
    content = prefix + ("x" * max(0, size - empty_size - len(prefix)))
    value = {"payload": content}
    if len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()) != size:
        raise ValueError(f"payload size {size} is too small for its benchmark marker")
    return value


def percentile(values: list[int], fraction: float) -> int:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, round((len(ordered) - 1) * fraction))]


def summary(latencies_ns: list[int], total_ns: int) -> dict[str, object]:
    count = len(latencies_ns)
    return {
        "count": count,
        "elapsed_ns": total_ns,
        "throughput_ops_s": round(count * 1_000_000_000 / total_ns, 2),
        "latency_ms": {
            "median": round(statistics.median(latencies_ns) / 1_000_000, 3),
            "p95": round(percentile(latencies_ns, 0.95) / 1_000_000, 3),
            "min": round(min(latencies_ns) / 1_000_000, 3),
            "max": round(max(latencies_ns) / 1_000_000, 3),
        },
        "latencies_ns": latencies_ns,
    }


def native_request(process: subprocess.Popen[str], request: dict[str, object]) -> dict[str, object]:
    assert process.stdin is not None and process.stdout is not None
    process.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
    process.stdin.flush()
    line = process.stdout.readline()
    if not line:
        raise RuntimeError("native API helper exited before replying")
    response = json.loads(line)
    if "error" in response:
        raise RuntimeError(f"native API helper failed: {response['error']}")
    return response


def append_batch(lane: str, data: list[dict[str, str]], native: subprocess.Popen[str],
                 direct: HttpsMcp, pool: str) -> dict[str, object]:
    started = time.perf_counter_ns()
    if lane == "native_api":
        response = native_request(native, {"op": "append", "items": data})
        latencies = response["latencies_ns"]
        sequences = response["result"]["sequences"]
        total = response["elapsed_ns"]
    else:
        latencies = []
        sequences = []
        for item in data:
            call_started = time.perf_counter_ns()
            result = direct.tool("plasmite_feed", {"pool": pool, "data": item, "create": False})
            latencies.append(time.perf_counter_ns() - call_started)
            sequences.append(result["message"]["seq"])
        total = time.perf_counter_ns() - started
    if len(sequences) != len(data) or len(set(sequences)) != len(data):
        raise RuntimeError(f"{lane} append returned {len(sequences)} distinct sequences for {len(data)} writes")
    return {"sequences": sequences, "latencies": latencies, "total": total}


def read_batch(lane: str, sequences: list[int], expected: list[dict[str, str]], native: subprocess.Popen[str],
               direct: HttpsMcp, pool: str) -> tuple[list[int], int]:
    started = time.perf_counter_ns()
    if lane == "native_api":
        response = native_request(native, {"op": "read", "sequences": sequences, "expected": expected})
        if response["result"]["verified_count"] != len(sequences):
            raise RuntimeError("native API helper verified the wrong read count")
        return response["latencies_ns"], response["elapsed_ns"]
    latencies = []
    for sequence, data in zip(sequences, expected, strict=True):
        call_started = time.perf_counter_ns()
        result = direct.tool("plasmite_fetch", {"pool": pool, "seq": sequence})
        latencies.append(time.perf_counter_ns() - call_started)
        message = result["message"]
        if message["seq"] != sequence or message["data"] != data:
            raise RuntimeError(f"{lane} readback mismatch for sequence {sequence}")
    return latencies, time.perf_counter_ns() - started


def run(args: argparse.Namespace) -> dict[str, object]:
    started_at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    root = Path(__file__).resolve().parents[1]
    binary = root / "target" / "release" / "plasmite"
    helper = root / "target" / "release" / "examples" / "bench_transport_native"
    print("Building release CLI and native API helper…", file=sys.stderr)
    subprocess.run(["cargo", "build", "--release", "--bin", "plasmite", "--example", "bench_transport_native"],
                   cwd=root, check=True)
    access_key_path = Path(args.access_key_file)
    key_stat = access_key_path.stat()
    if not access_key_path.is_file() or (os.name == "posix" and key_stat.st_mode & 0o077):
        raise RuntimeError("access-key file must be a regular file readable only by its owner")
    key_contents = access_key_path.read_text(encoding="utf-8").strip()
    try:
        access_key = json.loads(key_contents)["access_key"]
    except (json.JSONDecodeError, KeyError, TypeError):
        access_key = key_contents
    if not isinstance(access_key, str) or not access_key:
        raise RuntimeError("access-key file did not contain an access key")
    native = subprocess.Popen([str(helper), args.server, args.pool, str(access_key_path)], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, stderr=None, text=True, bufsize=1)
    direct: HttpsMcp | None = None
    raw_runs: list[dict[str, object]] = []
    try:
        native_info = native_request(native, {"op": "info"})["result"]["file_size"]
        direct = HttpsMcp(args.mcp_url, access_key, args.ca_file)
        clients = {"native_api": native, "https_mcp": direct}
        versions = {
            "plasmite_cli": command_output(str(binary), "--version"),
            "cargo": command_output("cargo", "--version"),
            "rustc": command_output("rustc", "--version"),
            "python": platform.python_version(),
            "python_implementation": platform.python_implementation(),
            "os": platform.platform(),
            "machine": platform.machine(),
            "cpu": platform.processor() or "unavailable",
            "server_mcp": direct.server_info,
        }
        direct_info = direct.tool("plasmite_pool_info", {"pool": args.pool})["pool"]["file_size"]
        if native_info != direct_info:
            raise RuntimeError("the native API and HTTPS MCP reported different pool sizes")
        retained_messages = (args.messages * args.repeats + len(args.sizes)) * len(LANES)
        required = retained_messages * (max(args.sizes) + 128) + 1_048_576
        if native_info < required:
            raise RuntimeError(
                f"pool is {native_info} bytes; this run needs at least {required} bytes to retain every measured message"
            )
        # Warm append and exact fetch through both clients for each size.
        for size in args.sizes:
            for lane, client in clients.items():
                warm = serialized_payload(size, 0, 0, lane)
                if lane == "native_api":
                    response = native_request(native, {"op": "append", "items": [warm]})
                    sequence = response["result"]["sequences"][0]
                    check = native_request(native, {"op": "read", "sequences": [sequence], "expected": [warm]})
                    if check["result"]["verified_count"] != 1:
                        raise RuntimeError("native warmup readback failed")
                else:
                    result = client.tool("plasmite_feed", {"pool": args.pool, "data": warm, "create": False})
                    sequence = result["message"]["seq"]
                    check = client.tool("plasmite_fetch", {"pool": args.pool, "seq": sequence})
                    if check["message"]["data"] != warm:
                        raise RuntimeError(f"{lane} warmup readback failed")

        for size in args.sizes:
            for repeat in range(1, args.repeats + 1):
                rotation = (repeat + size) % len(LANES)
                order = list(LANES[rotation:]) + list(LANES[:rotation])
                data_by_lane = {
                    lane: [serialized_payload(size, repeat, item, "common") for item in range(args.messages)]
                    for lane in LANES
                }
                run_records: dict[str, dict[str, object]] = {}
                for lane in order:
                    append = append_batch(lane, data_by_lane[lane], native, direct, args.pool)
                    run_records[lane] = {
                        "append": summary(append["latencies"], append["total"]),
                        "sequences": append["sequences"],
                        "append_count_verified": len(append["sequences"]),
                    }
                for lane in order:
                    sequences = run_records[lane]["sequences"]
                    latency, elapsed = read_batch(lane, sequences, data_by_lane[lane], native, direct, args.pool)
                    run_records[lane]["read"] = summary(latency, elapsed)
                    run_records[lane]["readback_verified"] = len(latency)
                raw_runs.append({"payload_bytes": size, "repeat": repeat, "lane_order": order,
                                 "lanes": run_records})
                print(f"Completed {size}-byte repeat {repeat}/{args.repeats}", file=sys.stderr)
    finally:
        cleanup_errors = []
        if direct is not None:
            try:
                direct.close()
            except Exception as error:
                cleanup_errors.append(f"HTTPS MCP: {error}")
        try:
            if native.poll() is None:
                native.stdin.close()
                try:
                    native.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    native.kill()
                    native.wait(timeout=5)
                    raise
        except Exception as error:
            cleanup_errors.append(f"native worker: {error}")
        try:
            subprocess.run([str(binary), "access", "disconnect", args.server], cwd=root,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        except Exception as error:
            cleanup_errors.append(f"saved connection: {error}")
        if cleanup_errors:
            raise RuntimeError("client cleanup failed: " + "; ".join(cleanup_errors))

    return {
        "schema_version": 1,
        "benchmark": "plasmite-native-api-https-mcp",
        "started_at_utc": started_at,
        "versions": versions,
        "configuration": {
            "server_url": args.server,
            "mcp_url": args.mcp_url,
            "pool": args.pool,
            "pool_file_size_bytes": native_info,
            "messages_per_phase": args.messages,
            "mcp_protocol_version": PROTOCOL_VERSION,
            "payload_serialized_json_bytes": args.sizes,
            "payload_definition": "UTF-8 byte length of compact JSON object {'payload': string}; MCP/HTTP protocol fields are excluded",
            "repeats": args.repeats,
            "concurrency": 1,
            "durability": "fast",
            "warmup": "HTTPS MCP completed initialize/initialized and pool-info checks. Both lanes performed an untimed append and exact fetch for every payload size before timing.",
            "oauth": "Registered a dynamic public client, approved the exact MCP resource with the protected key file, exchanged a PKCE code, and revoked the grant at exit; bearer and refresh tokens stayed in memory.",
            "oauth_refreshes": direct.refresh_count,
            "timed_calls": "One append or one exact fetch per operation; no batching and no model provider.",
            "limitations": "Native API uses the saved-connection RemoteClient. Timing excludes process startup and measures client calls; HTTPS MCP uses one persistent HTTPS connection.",
        },
        "runs": raw_runs,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True, help="HTTPS server origin used by the native saved connection")
    parser.add_argument("--mcp-url", required=True, help="Exact HTTPS MCP endpoint, normally SERVER/mcp")
    parser.add_argument("--pool", required=True, help="Existing dedicated pool with enough free capacity")
    parser.add_argument("--access-key-file", required=True, help="Mode-600 file containing one native access key")
    parser.add_argument("--messages", type=int, default=100)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--sizes", type=int, nargs="+", default=[512, 4096])
    parser.add_argument("--ca-file", help="Optional trusted CA file for direct HTTPS MCP")
    parser.add_argument("--output", required=True, help="Path for raw JSON results")
    args = parser.parse_args()
    if args.messages < 1 or args.repeats < 2 or any(size < 128 for size in args.sizes):
        parser.error("messages must be positive, repeats at least 2, and payload sizes at least 128 bytes")
    server = urlsplit(args.server)
    mcp = urlsplit(args.mcp_url)
    if server.scheme != "https" or not server.hostname or server.path not in ("", "/") or server.query or server.fragment:
        parser.error("--server must be the HTTPS server origin without a path, query, or fragment")
    if (server.hostname, server.port or 443) != (mcp.hostname, mcp.port or 443):
        parser.error("--server and --mcp-url must name the same host and port")
    result = run(args)
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote raw benchmark results to {output}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
