#!/usr/bin/env python3
"""Measure local and remote Plasmite access on one Windows host, without a model."""
from __future__ import annotations

import argparse
import http.client
import json
import os
import platform
import ssl
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from urllib.parse import urlsplit

from bench_transport_comparison import HttpsMcp, PROTOCOL_VERSION, native_request, serialized_payload, summary


class Worker:
    def __init__(self, command):
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        text=True, encoding="utf-8", bufsize=1)

    def phase(self, op, items, sequences=None):
        request = {"op": op, "items": items, "expected": items, "sequences": sequences}
        result = native_request(self.process, request)
        return result["latencies_ns"], result["elapsed_ns"], result["result"]

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                raise RuntimeError("worker did not stop after stdin closed")
        if self.process.returncode:
            raise RuntimeError(f"worker exited with {self.process.returncode}")


class CWorker(Worker):
    def phase(self, op, items, sequences=None):
        lines = [f"{op} {len(items)}"]
        if op == "append":
            lines.extend(json.dumps(item, separators=(",", ":")) for item in items)
        else:
            lines.extend(str(seq) for seq in sequences)
        self.process.stdin.write("\n".join(lines) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError("C worker exited before replying")
        result = json.loads(line)
        messages = result["result"]["messages"]
        if len(messages) != len(items):
            raise RuntimeError("C worker returned incorrect count")
        seqs = [message["seq"] for message in messages]
        if op == "read":
            for message, seq, item in zip(messages, sequences, items, strict=True):
                if message["seq"] != seq or message["data"] != item:
                    raise RuntimeError("C readback mismatch")
        return result["latencies_ns"], result["elapsed_ns"], {"sequences": seqs, "verified_count": len(items)}


class HttpPool:
    def __init__(self, origin, pool, key, ca, browser=False):
        parts = urlsplit(origin)
        self.path = f"/v0/pools/{pool}"
        self.origin = origin
        self.headers = {}
        if parts.scheme == "https":
            self.connection = http.client.HTTPSConnection(parts.hostname, parts.port,
                context=ssl.create_default_context(cafile=ca), timeout=60)
            if browser:
                response, headers = self.request("POST", "/v0/browser/session", {"access_key": key},
                                                {"Origin": origin})
                self.headers = {"Cookie": headers["set-cookie"].split(";", 1)[0], "Origin": origin}
            else:
                self.headers = {"Authorization": f"Bearer {key.rsplit('.', 1)[1]}"}
        else:
            self.connection = http.client.HTTPConnection(parts.hostname, parts.port, timeout=60)

    def request(self, method, path, value=None, headers=None):
        body = None if value is None else json.dumps(value, separators=(",", ":"))
        self.connection.request(method, path, body=body,
            headers={"Content-Type": "application/json", **self.headers, **(headers or {})})
        response = self.connection.getresponse()
        raw = response.read()
        if response.status != 200:
            raise RuntimeError(f"{method} {path} returned {response.status}: {raw[:300]!r}")
        return json.loads(raw), {name.lower(): value for name, value in response.getheaders()}

    def append(self, item):
        return self.request("POST", self.path + "/append", {"data": item, "tags": [], "durability": "fast"})[0]["message"]

    def get(self, seq):
        return self.request("GET", self.path + f"/messages/{seq}")[0]["message"]

    def close(self):
        if "Cookie" in self.headers:
            self.request("DELETE", "/v0/browser/session")
        self.connection.close()


class McpPool:
    def __init__(self, client, pool):
        self.client, self.pool = client, pool

    def append(self, item):
        return self.client.tool("plasmite_feed", {"pool": self.pool, "data": item, "create": False})["message"]

    def get(self, seq):
        return self.client.tool("plasmite_fetch", {"pool": self.pool, "seq": seq})["message"]

    def close(self):
        self.client.close()


class LocalDiskMcp:
    def __init__(self, executable, directory):
        self.process = subprocess.Popen([str(executable), "--dir", directory, "mcp"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1)
        self.next_id = 1
        try:
            response = self.call("initialize", {"protocolVersion": PROTOCOL_VERSION, "capabilities": {},
                "clientInfo": {"name": "access-benchmark", "version": "1"}})
            if response.get("protocolVersion") != PROTOCOL_VERSION:
                raise RuntimeError("unexpected local MCP protocol")
            self.notify_initialized()
        except BaseException:
            self.close()
            raise

    def call(self, method, params):
        if self.process.poll() is not None:
            raise RuntimeError(f"local MCP process exited with {self.process.returncode}")
        request_id = self.next_id
        self.next_id += 1
        request = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
        self.process.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError("local MCP closed stdout before replying")
        response = json.loads(line)
        if "error" in response:
            raise RuntimeError(f"local MCP {method} failed: {response['error']}")
        if response.get("id") != request_id:
            raise RuntimeError(f"local MCP returned mismatched request id for {method}")
        return response["result"]

    def notify_initialized(self):
        notification = {"jsonrpc": "2.0", "method": "notifications/initialized"}
        self.process.stdin.write(json.dumps(notification, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def tool(self, name, arguments):
        result = self.call("tools/call", {"name": name, "arguments": arguments})
        if result.get("isError"):
            raise RuntimeError(f"local MCP tool {name} failed: {result.get('content')}")
        return result["structuredContent"]

    def close(self):
        if self.process.poll() is not None:
            return
        try:
            self.process.stdin.close()
        except OSError:
            pass
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
            raise RuntimeError("local MCP process did not stop after stdin closed")


class CliPool:
    def __init__(self, binary, directory, ref):
        self.command = [str(binary), "--dir", directory]
        self.ref = ref

    def append(self, item):
        result = subprocess.run([*self.command, "feed", self.ref, "--json"],
            input=json.dumps(item, separators=(",", ":")) + "\n", capture_output=True,
            text=True, encoding="utf-8", check=True)
        return json.loads(result.stdout)

    def get(self, seq):
        result = subprocess.run([*self.command, "fetch", self.ref, str(seq), "--json"],
            capture_output=True, text=True, encoding="utf-8", check=True)
        return json.loads(result.stdout)

    def close(self):
        pass


class PythonPool:
    def __init__(self, root, directory, pool):
        sys.path.insert(0, str(root / "bindings" / "python"))
        import plasmite
        self.client = plasmite.Client(directory)
        self.pool = self.client.open_pool(pool)

    def append(self, item):
        import plasmite
        return json.loads(self.pool.append_json(json.dumps(item, separators=(",", ":")).encode(), [], plasmite.Durability.FAST))

    def get(self, seq):
        message = self.pool.get(seq)
        return {"seq": message.seq, "data": message.data}

    def close(self):
        self.pool.close()
        self.client.close()


def phase(client, op, items, sequences=None):
    if isinstance(client, Worker):
        latencies, elapsed, result = client.phase(op, items, sequences)
        if op == "append":
            seqs = result["sequences"]
            if len(seqs) != len(items) or len(set(seqs)) != len(items):
                raise RuntimeError("worker returned incorrect append count")
            return latencies, elapsed, seqs
        if result["verified_count"] != len(items):
            raise RuntimeError("worker verified incorrect read count")
        return latencies, elapsed, sequences
    latencies, seqs = [], []
    started = time.perf_counter_ns()
    for index, item in enumerate(items):
        before = time.perf_counter_ns()
        message = client.append(item) if op == "append" else client.get(sequences[index])
        latencies.append(time.perf_counter_ns() - before)
        if op == "read" and (message["seq"] != sequences[index] or message["data"] != item):
            raise RuntimeError(f"readback mismatch at {index}")
        seqs.append(message["seq"])
    elapsed = time.perf_counter_ns() - started
    if len(set(seqs)) != len(items):
        raise RuntimeError("duplicate sequences")
    return latencies, elapsed, seqs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--pool-dir", required=True)
    parser.add_argument("--pool", required=True)
    parser.add_argument("--server", required=True)
    parser.add_argument("--local-server", required=True)
    parser.add_argument("--access-key-file", required=True)
    parser.add_argument("--ca-file", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--messages", type=int, default=100)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--sizes", type=int, nargs="+", default=[512, 4096])
    parser.add_argument("--node", default="node")
    parser.add_argument("--lanes", nargs="+")
    args = parser.parse_args()
    if args.messages < 1 or args.repeats < 2 or any(n < 128 for n in args.sizes):
        parser.error("need positive messages, at least two repeats, and payloads of at least 128 bytes")
    root = args.root
    ext = ".exe" if os.name == "nt" else ""
    binary = root / "target" / "release" / f"plasmite{ext}"
    helper = root / "target" / "release" / "examples" / f"bench_transport_native{ext}"
    key_path = Path(args.access_key_file)
    if os.name == "posix" and key_path.stat().st_mode & 0o077:
        raise RuntimeError("access-key file must be owner-only")
    key_text = key_path.read_text(encoding="utf-8-sig").strip()
    key = json.loads(key_text)["access_key"] if key_text.startswith("{") else key_text
    os.environ["NODE_EXTRA_CA_CERTS"] = args.ca_file
    os.environ["PLASMITE_LIB_DIR"] = str(binary.parent)
    factories = {
        "local_rust": lambda: Worker([str(helper), "--local", args.pool_dir, args.pool]),
        "local_lite3": lambda: Worker([str(helper), "--local-lite3", args.pool_dir, args.pool]),
        "local_c": lambda: CWorker([str(root / "target" / "release" / f"bench_access_c{ext}"), args.pool_dir, args.pool]),
        "local_python": lambda: PythonPool(root, args.pool_dir, args.pool),
        "local_node": lambda: Worker([args.node, str(root / "scripts" / "bench_access_node.cjs"), "local", str(root), args.pool_dir, args.pool]),
        "local_http_cli": lambda: CliPool(binary, args.pool_dir, args.local_server + "/" + args.pool),
        "local_cli": lambda: CliPool(binary, args.pool_dir, args.pool),
        "local_mcp": lambda: McpPool(LocalDiskMcp(binary, args.pool_dir), args.pool),
        "local_http": lambda: HttpPool(args.local_server, args.pool, key, args.ca_file),
        "local_http_rust": lambda: Worker([str(helper), "--http", args.local_server, args.pool]),
        "https_rust_key": lambda: Worker([str(helper), "--key", args.server, args.pool, args.access_key_file]),
        "https_lite3_key": lambda: Worker([str(helper), "--lite3-key", args.server, args.pool, args.access_key_file]),
        "https_rust": lambda: Worker([str(helper), args.server, args.pool, args.access_key_file]),
        "https_lite3": lambda: Worker([str(helper), "--lite3", args.server, args.pool, args.access_key_file]),
        "https_json": lambda: HttpPool(args.server, args.pool, key, args.ca_file),
        "https_browser_api": lambda: HttpPool(args.server, args.pool, key, args.ca_file, browser=True),
        "local_http_node": lambda: Worker([args.node, str(root / "scripts" / "bench_access_node.cjs"), "http", str(root), args.local_server, args.pool]),
        "https_node": lambda: Worker([args.node, str(root / "scripts" / "bench_access_node.cjs"), "https", str(root), args.server, args.pool, args.access_key_file]),
        "https_cli": lambda: CliPool(binary, args.pool_dir, args.server + "/" + args.pool),
        "direct_https_mcp": lambda: McpPool(HttpsMcp(args.server + "/mcp", key, args.ca_file), args.pool),
    }
    lanes = args.lanes or list(factories)
    unknown = set(lanes) - factories.keys()
    if unknown:
        parser.error(f"unknown lanes: {sorted(unknown)}")
    result = {"schema_version": 1, "benchmark": "plasmite-all-access-windows",
        "started_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "versions": {"python": sys.version, "os": platform.platform(), "machine": platform.machine(),
                     "plasmite": subprocess.check_output([str(binary), "--version"], text=True).strip() if binary.is_file() else "no local Plasmite executable"},
        "configuration": {"pool_dir": args.pool_dir, "pool": args.pool, "server": args.server,
            "local_server": args.local_server, "sizes": args.sizes, "messages": args.messages,
            "repeats": args.repeats, "durability": "fast", "concurrency": 1, "lanes": lanes,
            "timing": "Warm single append/exact-fetch calls. CLI includes a fresh process per call. Other clients persist. Excludes authorization, model time, and UI rendering. Lite3 encode/decode occurs outside individual call latency."},
        "setup_ms": {}, "runs": []}
    clients = {}
    access_home = tempfile.TemporaryDirectory(prefix="plasmite-access-bench-")
    os.environ["PLASMITE_ACCESS_HOME"] = str(Path(access_home.name) / "credentials")
    result["configuration"]["credential_store"] = "Fresh private directory for this run; removed after all clients stop. Existing saved connections remain untouched."
    try:
        for lane in lanes:
            before = time.perf_counter_ns()
            clients[lane] = factories[lane]()
            for size in args.sizes:
                warm = [serialized_payload(size, 0, 0, lane)]
                _, _, seqs = phase(clients[lane], "append", warm)
                phase(clients[lane], "read", warm, seqs)
            result["setup_ms"][lane] = (time.perf_counter_ns() - before) / 1e6
            print(f"Ready: {lane}", file=sys.stderr)
        for size in args.sizes:
            for repeat in range(args.repeats):
                offset = (repeat + size) % len(lanes)
                order = lanes[offset:] + lanes[:offset]
                data = {lane: [serialized_payload(size, repeat + 1, i, lane) for i in range(args.messages)] for lane in lanes}
                record = {"payload_bytes": size, "repeat": repeat + 1, "lane_order": order, "lanes": {}}
                for lane in order:
                    latency, elapsed, seqs = phase(clients[lane], "append", data[lane])
                    record["lanes"][lane] = {"append": summary(latency, elapsed), "sequences": seqs}
                for lane in order:
                    seqs = record["lanes"][lane]["sequences"]
                    latency, elapsed, _ = phase(clients[lane], "read", data[lane], seqs)
                    record["lanes"][lane].update(read=summary(latency, elapsed), readback_verified=len(data[lane]))
                result["runs"].append(record)
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
                print(f"Completed {size} bytes, repeat {repeat + 1}/{args.repeats}", file=sys.stderr)
    finally:
        cleanup_errors = []
        for lane, client in reversed(list(clients.items())):
            try:
                client.close()
            except Exception as error:
                cleanup_errors.append(f"{lane}: {error}")
        access_home.cleanup()
        if cleanup_errors:
            raise RuntimeError("client cleanup failed: " + "; ".join(cleanup_errors))
    print(f"Wrote {args.output}", file=sys.stderr)
if __name__ == "__main__":
    main()
