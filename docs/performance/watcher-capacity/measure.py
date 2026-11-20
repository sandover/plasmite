# /// script
# requires-python = ">=3.11"
# dependencies = ["h2==4.3.0", "httpx==0.28.1", "psutil==7.1.3"]
# ///

"""Measure live Plasmite readers and competing short requests on a disposable server."""

import argparse
import asyncio
from collections import Counter, defaultdict
from contextlib import AsyncExitStack, contextmanager
from dataclasses import dataclass
import json
import math
import os
from pathlib import Path
import platform
import re
import socket
import ssl
import time
import uuid

import httpx
import psutil

from fixture import temporary_server
from http2_transport import Http2Transport

VERSION = "2026-07-28"
WATCH_TAG = "watch"


@dataclass(frozen=True)
class Endpoint:
    url: str
    pid: int
    verify: ssl.SSLContext | bool = True
    http_key: str | None = None
    h2_context: ssl.SSLContext | None = None
    mcp_token: str | None = None


def https_endpoint(url, pid, certificate, key, token):
    match = re.fullmatch(r"pk1\.[0-9a-f]{64}\.([0-9a-f]{64})", key)
    if not match:
        raise ValueError("--http-key-file must contain the full access key from plasmite access invite")
    context = ssl.create_default_context(cafile=certificate)
    h2_context = ssl.create_default_context(cafile=certificate)
    h2_context.set_alpn_protocols(["h2"])
    return Endpoint(url, pid, context, match[1], h2_context, token)


@contextmanager
def endpoint(args):
    if args.url:
        yield https_endpoint(
            args.url.rstrip("/"), args.pid, args.ca_file,
            args.http_key_file.read_text().strip(), args.mcp_token_file.read_text().strip(),
        )
        return

    with temporary_server(args.binary, args.https, args.server_arg) as (
        url, pid, certificate, key, token
    ):
        if certificate:
            yield https_endpoint(url, pid, certificate, key, token)
        else:
            yield Endpoint(url, pid)


def client(server, http2=False, stall=False):
    if http2:
        transport = Http2Transport(server.h2_context)
        return httpx.AsyncClient(base_url=server.url, transport=transport, trust_env=False)
    transport = httpx.AsyncHTTPTransport(
        verify=server.verify,
        trust_env=False,
        limits=httpx.Limits(max_connections=1, max_keepalive_connections=1),
        socket_options=[(socket.SOL_SOCKET, socket.SO_RCVBUF, 8192)] if stall else None,
    )
    return httpx.AsyncClient(base_url=server.url, transport=transport, trust_env=False, timeout=15)


def headers(server):
    return {"Authorization": "Bearer " + server.http_key} if server.http_key else {}


def mcp_request(server, method, params, request_id=1):
    request_headers = {
        "Accept": "application/json, text/event-stream",
        "MCP-Protocol-Version": VERSION,
        "Mcp-Method": method,
    }
    if server.mcp_token:
        request_headers["Authorization"] = "Bearer " + server.mcp_token
    if "name" in params:
        request_headers["Mcp-Name"] = params["name"]
    return {
        "headers": request_headers,
        "json": {
            "jsonrpc": "2.0", "id": request_id, "method": method,
            "params": {
                **params,
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": VERSION,
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        },
    }


async def rpc(server, session, method, params):
    response = await session.post("/mcp", **mcp_request(server, method, params))
    response.raise_for_status()
    value = response.json()
    if "error" in value or value.get("result", {}).get("isError"):
        raise RuntimeError(value)
    return value["result"]


async def create_pool(server, session, pool, size):
    response = await session.post("/v2/pools", json={"pool": pool, "size_bytes": size},
                                  headers=headers(server))
    response.raise_for_status()


async def feed(server, session, pool, body, tag):
    response = await session.post(
        f"/v2/pools/{pool}/feed", headers=headers(server),
        json={"data": body, "tags": [tag], "durability": "fast"},
    )
    response.raise_for_status()
    return int(response.json()["message"]["seq"])


def error_label(error):
    if isinstance(error, httpx.HTTPStatusError):
        return f"HTTP {error.response.status_code}"
    return str(error) or type(error).__name__


def process_sample(process):
    cpu = process.cpu_times()
    return {
        "cpu_seconds": cpu.user + cpu.system,
        "threads": process.num_threads(),
        "rss_bytes": process.memory_info().rss,
        "descriptors_or_handles": (
            process.num_handles() if os.name == "nt" else process.num_fds()
        ),
    }


async def idle_sample(process, seconds):
    before = process_sample(process)
    start = time.monotonic()
    await asyncio.sleep(seconds)
    after = process_sample(process)
    after["cpu_percent_of_one_core"] = 100 * (
        after["cpu_seconds"] - before["cpu_seconds"]
    ) / (time.monotonic() - start)
    return after


def p95(values):
    return round(sorted(values)[math.ceil(len(values) * .95) - 1], 3) if values else None


@dataclass
class Delivery:
    sent: float
    expected: int
    count: int = 0
    latest_ms: float = 0


class Deliveries:
    def __init__(self):
        self.messages = {}
        self.followers = Counter()
        self.errors = Counter()

    def receive(self, pool, seq, previous):
        item = self.messages.get((pool, seq))
        if seq <= previous:
            self.errors["duplicate_or_out_of_order"] += 1
        elif not item or not item.expected:
            self.errors["unexpected_message"] += 1
        else:
            item.count += 1
            item.latest_ms = (time.perf_counter() - item.sent) * 1000

    def result(self):
        matched = [item for item in self.messages.values() if item.expected]
        complete = [item.latest_ms for item in matched if item.count == item.expected]
        return {
            "messages_matched": len(matched),
            "expected_deliveries": sum(item.expected for item in matched),
            "received_deliveries": sum(item.count for item in matched),
            "incomplete_messages": len(matched) - len(complete),
            "last_receipt_p95_ms": p95(complete),
            "errors": dict(self.errors),
        }


def event_sequence(line, pool, active):
    if not line.startswith("data: "):
        return None
    frame = json.loads(line.removeprefix("data: "))
    if "error" in frame:
        raise RuntimeError(frame["error"])
    method = frame.get("method")
    params = frame.get("params", {})
    if method == "notifications/events/active":
        if params["truncated"]:
            raise RuntimeError(f"Event reader lost retained messages in {pool}")
        active.set()
    elif method == "notifications/events/event":
        if params["data"]["pool"] != pool:
            raise RuntimeError("Event names the wrong pool")
        return int(params["data"]["seq"])
    elif method == "notifications/events/terminated":
        raise RuntimeError(params)
    return None


async def watch(server, session, mode, pool, delivery, started, request_id):
    active = asyncio.Event()
    previous = 0
    try:
        if mode == "event":
            params = {"name": "pool.message", "arguments": {"pool": pool, "tags": [WATCH_TAG]}}
            stream = session.stream("POST", "/mcp",
                                    timeout=httpx.Timeout(15, read=None),
                                    **mcp_request(server, "events/stream", params, request_id))
        else:
            stream = session.stream("GET", f"/v2/pools/{pool}/follow",
                                    params={"tag": WATCH_TAG}, headers=headers(server),
                                    timeout=httpx.Timeout(15, read=None))
        async with stream as response:
            response.raise_for_status()
            if mode == "event" and "text/event-stream" not in response.headers.get("content-type", ""):
                body = (await response.aread()).decode(errors="replace")
                try:
                    error = json.loads(body).get("error", "missing Events response")
                except (ValueError, AttributeError):
                    error = "expected an Events stream"
                raise RuntimeError(error)
            if mode == "http":
                started.set_result(response.http_version)
            async for line in response.aiter_lines():
                if mode == "event":
                    seq = event_sequence(line, pool, active)
                else:
                    row = json.loads(line)
                    if "seq" not in row:
                        raise RuntimeError(row)
                    seq = int(row["seq"])
                if active.is_set() and not started.done():
                    started.set_result(response.http_version)
                if seq is not None:
                    delivery.receive(pool, seq, previous)
                    previous = seq
            raise RuntimeError("Watch ended")
    except (httpx.HTTPError, RuntimeError, ValueError) as error:
        if not started.done():
            started.set_result(error_label(error))
        else:
            delivery.errors[f"{mode} watch ended: {error_label(error)}"] += 1
    finally:
        if not started.done():
            started.cancel()


async def mcp_follow(server, session, pool, delivery, started):
    previous = 1
    try:
        # The call has no open acknowledgement. The seeded cursor also allows
        # the first call to find a write that reached the pool before the call.
        started.set_result("MCP calls scheduled")
        while True:
            result = await rpc(server, session, "tools/call", {
                "name": "plasmite_follow",
                "arguments": {
                    "pool": pool, "tags": [WATCH_TAG], "since": previous + 1,
                    "count": 1, "timeout_ms": 5000,
                },
            })
            payload = result["structuredContent"]
            for row in payload["messages"]:
                seq = int(row["seq"])
                delivery.receive(pool, seq, previous)
                previous = seq
            if payload.get("next_seq") is not None:
                previous = max(previous, int(payload["next_seq"]) - 1)
    except (httpx.HTTPError, RuntimeError, ValueError) as error:
        delivery.errors[f"MCP follow ended: {error_label(error)}"] += 1


async def timed_controls(server, session, pool, count, pause=0):
    samples = defaultdict(list)
    errors = Counter()
    for turn in range(count):
        if pause and turn:
            await asyncio.sleep(pause)
        for name in ("feed", "fetch", "mcp_info"):
            start = time.perf_counter()
            try:
                if name == "feed":
                    await feed(server, session, pool, {"control": turn}, "control")
                elif name == "fetch":
                    response = await session.get(f"/v2/pools/{pool}/fetch", params={"last": 1},
                                                 headers=headers(server))
                    response.raise_for_status()
                    rows = [json.loads(line) for line in response.text.splitlines()]
                    if rows[0]["data"]["control"] != turn or rows[-1]["type"] != "complete":
                        raise RuntimeError("Finite fetch did not return the control message and completion")
                else:
                    await rpc(server, session, "tools/call", {
                        "name": "plasmite_pool_info", "arguments": {"pool": pool},
                    })
            except (httpx.HTTPError, RuntimeError, KeyError) as error:
                errors[f"{name}: {error_label(error)}"] += 1
            else:
                samples[name].append((time.perf_counter() - start) * 1000)
    return {"p95_ms": {name: p95(values) for name, values in samples.items()},
            "successful": {name: len(values) for name, values in samples.items()},
            "errors": dict(errors)}


async def publish(server, session, pools, delivery, args, rate):
    start = time.perf_counter()
    sequences = {pool: 1 for pool in pools}
    expected = math.ceil(rate * args.seconds)
    payload = {"text": "x" * args.payload_bytes}
    failures = Counter()
    posted = 0
    for index in range(expected):
        delay = start + index / rate - time.perf_counter()
        if delay > 0:
            await asyncio.sleep(delay)
        pool = pools[index % len(pools)]
        seq = sequences[pool] + 1
        matched = seq % 2 == 0
        item = Delivery(time.perf_counter(), delivery.followers[pool] if matched else 0)
        delivery.messages[(pool, seq)] = item
        try:
            actual = await feed(server, session, pool, payload, WATCH_TAG if matched else "skip")
        except httpx.HTTPStatusError as error:
            failures[f"HTTP {error.response.status_code}"] += 1
            del delivery.messages[(pool, seq)]
            continue
        if actual != seq:
            raise RuntimeError(f"Expected sequence {seq} in {pool}, got {actual}")
        sequences[pool] = actual
        posted += 1
    elapsed = time.perf_counter() - start
    return {
        "attempted": expected,
        "committed": posted,
        "elapsed_seconds": elapsed,
        "achieved_writes_per_second": posted / elapsed,
        "errors": dict(failures),
    }


async def start_stalled_reader(server, stack, writer, pool):
    # Leave the response body unread and give TCP a small receive buffer.
    # Send 24 MiB, enough to fill both the transport buffers and the server's
    # output queue. The separate pool keeps these writes out of delivery data.
    reader = await stack.enter_async_context(client(server, stall=True))
    response = await stack.enter_async_context(reader.stream(
        "GET", f"/v2/pools/{pool}/follow", headers=headers(server),
    ))
    response.raise_for_status()
    data = {"text": "x" * (512 * 1024)}
    for _ in range(48):
        await feed(server, writer, pool, data, "stall")


async def check_agent_connection(server, session, h2, control_pool):
    # Shared HTTP/2 streams use this session for short calls too. An HTTP/1.1
    # agent needs a fresh connection while its stream occupies its first one.
    try:
        if h2:
            await rpc(server, session, "tools/call", {
                "name": "plasmite_pool_info", "arguments": {"pool": control_pool},
            })
        else:
            async with client(server) as fresh:
                await rpc(server, fresh, "tools/call", {
                    "name": "plasmite_pool_info", "arguments": {"pool": control_pool},
                })
    except (httpx.HTTPError, RuntimeError) as error:
        return error_label(error)
    return "ok"


async def measure(server, args, rate):
    h2 = args.connections == "shared-h2"
    process = psutil.Process(server.pid)
    trial = uuid.uuid4().hex[:12]
    pools = [f"bench-{trial}-{index}" for index in range(args.pools)]
    control = f"bench-{trial}-control"
    stall_pool = f"bench-{trial}-stall"
    modes = ["event"] * args.events + ["http"] * args.http_follows + ["mcp"] * args.mcp_follows
    async with AsyncExitStack() as stack:
        control_session = await stack.enter_async_context(client(server, h2))
        publish_session = await stack.enter_async_context(client(server, h2))
        await create_pool(server, control_session, control, 1024 * 1024)
        for pool in pools:
            await create_pool(server, control_session, pool, args.pool_mib * 1024 * 1024)
            await feed(server, control_session, pool, {"seed": True}, "skip")
        if args.stall:
            await create_pool(server, control_session, stall_pool, 64 * 1024 * 1024)
        await timed_controls(server, control_session, control, 1)
        controls_idle = await timed_controls(
            server, control_session, control, args.controls, args.seconds / args.controls,
        )
        baseline = await idle_sample(process, args.idle_seconds)
        delivery = Deliveries()
        tasks = []
        active = []
        shared = None
        for number, mode in enumerate(modes):
            if h2 and number < 50:
                shared = control_session
            elif not h2 or number % 50 == 0:
                shared = await stack.enter_async_context(client(server, h2))
            pool = pools[number % len(pools)]
            ready = asyncio.get_running_loop().create_future()
            active.append((mode, pool, ready))
            if mode == "mcp":
                runner = mcp_follow(server, shared, pool, delivery, ready)
            else:
                runner = watch(server, shared, mode, pool, delivery, ready, number + 1)
            tasks.append(asyncio.create_task(runner))
            if (number + 1) % 50 == 0:
                # Build up the steady load without overflowing the kernel's
                # queue for new TCP connections.
                await asyncio.wait([ready for _, _, ready in active[-50:]], timeout=5)
        try:
            await asyncio.wait([ready for _, _, ready in active], timeout=20)
            admitted = Counter()
            rejected = Counter()
            versions = Counter()
            for mode, pool, ready in active:
                result = ready.result() if ready.done() and not ready.cancelled() else "open timeout"
                if result.startswith("HTTP/") or result == "MCP calls scheduled":
                    if mode != "mcp":
                        versions[result] += 1
                    delivery.followers[pool] += 1
                    admitted[mode] += 1
                else:
                    rejected[f"{mode}: {result[:200]}"] += 1
            admission = process_sample(process)
            if args.idle_settle_seconds:
                await asyncio.sleep(args.idle_settle_seconds)
            idle = await idle_sample(process, args.idle_seconds)
            extra_agent_call = await check_agent_connection(server, control_session, h2, control)
            stalled = None
            if args.stall:
                try:
                    await start_stalled_reader(server, stack, publish_session, stall_pool)
                except httpx.HTTPError as error:
                    stalled = {"error": error_label(error)}
                else:
                    stalled = process_sample(process)
            control_task = asyncio.create_task(timed_controls(
                server, control_session, control, args.controls, args.seconds / args.controls,
            ))
            tasks.append(control_task)
            published = await publish(server, publish_session, pools, delivery, args, rate)
            await asyncio.sleep(args.drain_seconds)
            controls_loaded = await control_task
            return {
                "target_writes_per_second": rate,
                "connections": args.connections,
                "stream_versions": dict(versions),
                "pools": len(pools),
                "requested_readers": dict(Counter(modes)),
                "admitted_or_scheduled": dict(admitted),
                "rejected": dict(rejected),
                "before_readers": baseline,
                "admission": admission,
                "idle_with_readers": idle,
                "with_stalled_follower": stalled,
                "extra_agent_call": extra_agent_call,
                "after_load": process_sample(process),
                "idle_controls": controls_idle,
                "loaded_controls": controls_loaded,
                "published": published,
                "delivery": delivery.result(),
            }
        finally:
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    origin = parser.add_mutually_exclusive_group(required=True)
    origin.add_argument("--binary", type=Path, help="start a disposable server and use its local listener")
    origin.add_argument("--url", help="HTTPS origin of a dedicated, disposable benchmark server")
    parser.add_argument("--https", action="store_true", help="use HTTPS on the disposable binary fixture")
    parser.add_argument("--pid", type=int, help="PID of the server at --url")
    parser.add_argument("--server-arg", action="append", default=[], help="extra argument for --binary")
    parser.add_argument("--ca-file", type=Path)
    parser.add_argument("--http-key-file", type=Path)
    parser.add_argument("--mcp-token-file", type=Path)
    parser.add_argument("--events", type=int, required=True)
    parser.add_argument("--http-follows", type=int, default=0)
    parser.add_argument("--mcp-follows", type=int, default=0)
    parser.add_argument("--pools", type=int, default=1)
    parser.add_argument("--connections", choices=["separate-h1", "shared-h2"], default="separate-h1")
    parser.add_argument("--rates", type=int, nargs="+", default=[20])
    parser.add_argument("--seconds", type=float, default=5)
    parser.add_argument("--idle-seconds", type=float, default=5)
    parser.add_argument("--idle-settle-seconds", type=float, default=0,
                        help="wait after admission before timing idle CPU")
    parser.add_argument("--drain-seconds", type=float, default=2)
    parser.add_argument("--controls", type=int, default=10)
    parser.add_argument("--stall", action="store_true", help="leave an extra HTTP reader unread")
    parser.add_argument("--payload-bytes", type=int, default=1024)
    parser.add_argument("--pool-mib", type=int, default=32)
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--revision", required=True, help="source revision of the tested release build")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.url and (not args.url.startswith("https://") or not all(
        (args.pid, args.ca_file, args.http_key_file, args.mcp_token_file)
    )):
        parser.error("--url requires HTTPS, --pid, --ca-file, --http-key-file and --mcp-token-file")
    if args.url and (args.https or args.server_arg):
        parser.error("--https and --server-arg require --binary")
    if args.binary and not args.https and args.connections == "shared-h2":
        parser.error("use the HTTPS listener to test HTTP/2")
    if min(args.events, args.http_follows, args.mcp_follows) < 0 or not (
        args.events or args.http_follows or args.mcp_follows
    ):
        parser.error("request at least one reader and use nonnegative counts")
    if min(args.pools, args.controls, args.seconds, args.idle_seconds, args.pool_mib,
           args.drain_seconds, args.payload_bytes, args.repeats, *args.rates) <= 0:
        parser.error("all workload sizes, durations and rates must be positive")
    if args.idle_settle_seconds < 0:
        parser.error("--idle-settle-seconds cannot be negative")
    return args


def main():
    args = parse_args()
    trials = []
    # Each rate starts from fresh readers and pools. Local runs also start a
    # fresh server, so cached blocking workers cannot carry over to later rates.
    for _ in range(args.repeats):
        for rate in args.rates:
            with endpoint(args) as server:
                trials.append(asyncio.run(measure(server, args, rate)))
    result = {
        "revision": args.revision,
        "platform": platform.platform(),
        "machine": platform.machine(),
        "python": platform.python_version(),
        "listener": "https" if args.url or args.https else "local",
        "workload": {
            "payload_text_bytes": args.payload_bytes,
            "pool_capacity_mib": args.pool_mib,
            "target_duration_seconds": args.seconds,
            "idle_sample_seconds": args.idle_seconds,
            "idle_settle_seconds": args.idle_settle_seconds,
            "drain_seconds": args.drain_seconds,
            "controls": args.controls,
            "server_arguments": args.server_arg,
        },
        "trials": trials,
    }
    args.output.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
