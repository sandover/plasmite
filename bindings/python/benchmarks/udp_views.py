from plasmite import FollowStart
"""Compare UDP raw, selected and requested native output with verified delivery.

The peer sends one DATA at a time using a test-only queue. This measures the
receive/conversion boundary, not saturation throughput or packet-loss behavior.
Run each lane in a fresh process with the same fixed release library.
"""
import argparse
import hashlib
import json
import queue
import socket
import tempfile
import threading
import time
from pathlib import Path

import plasmite
from plasmite._views import VIEW_BACKEND


def run(lane, fixture, count):
    data = {"metrics": {"power": 42}, "samples": [1, 2, 3, 4]}
    if fixture == "wide":
        data.update({f"field{i}": "x" * 64 for i in range(128)})
    elif fixture == "text32k":
        data["text"] = "x" * 32768
    elif fixture == "array":
        data["samples"] = list(range(1000))
    with tempfile.TemporaryDirectory(prefix="plasmite-udp-view-") as directory:
        with plasmite.Client(directory) as client:
            with client.create_pool("source", 1024 * 1024) as pool:
                message = pool.append(data, ["source"])
                payload = pool.get_lite3(message.seq).payload
    assert len(payload) + 40 < 65507
    permit = queue.Queue(maxsize=1)
    errors = []
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        # Match production socket capacity, including macOS's small send default.
        peer.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 128 * 1024)
        peer.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 128 * 1024)
        peer.bind(("127.0.0.1", 0))
        peer.settimeout(10)

        def serve():
            try:
                request, address = peer.recvfrom(4096)
                subscription = request[8:24]

                def send(op, body):
                    peer.sendto(b"\0PUDP\x01" + bytes([op, 0]) + subscription + body, address)

                send(3, b'{"cookie":{"expires_ms":10000,"mac":"bench"}}')
                peer.recvfrom(4096)
                for seq in range(1, count + 2):
                    if seq > 1:
                        permit.get(timeout=10)
                    send(7, seq.to_bytes(8, "big") + (123456789).to_bytes(8, "big") + payload)
            except BaseException as error:
                errors.append(error)

        worker = threading.Thread(target=serve)
        worker.start()
        try:
            with plasmite.UdpClient(f"udp://127.0.0.1:{peer.getsockname()[1]}") as client:
                with client.open_pool("source") as pool:
                    open_follow = pool.follow_views if lane.startswith("view") else pool.follow
                    with open_follow(plasmite.UdpFollowOptions(start=plasmite.FollowStart.sequence(1))) as stream:
                        warm = stream.next_event(timeout_ms=1000)
                        assert warm.seq == 1
                        cpu, thread_cpu, start = time.process_time_ns(), time.thread_time_ns(), time.perf_counter_ns()
                        checksum = 0
                        for seq in range(2, count + 2):
                            permit.put(True)
                            event = stream.next_event(timeout_ms=1000)
                            assert event.seq == seq and event.timestamp_ns == 123456789
                            if lane == "raw":
                                checksum += len(event.payload)
                            elif lane == "view-selected":
                                checksum += event.data["metrics"]["power"]
                            elif lane == "view-native":
                                checksum += plasmite.to_native(event.data)["metrics"]["power"]
                            else:
                                checksum += event.message().data["metrics"]["power"]
                        elapsed = time.perf_counter_ns() - start
                        used_cpu = time.process_time_ns() - cpu
                        used_thread_cpu = time.thread_time_ns() - thread_cpu
                    # Verify the entire final value after closing native handles.
                    final = plasmite.to_native(event.data) if lane.startswith("view") else event.message().data
                    assert final == data

        finally:
            worker.join(timeout=11)
            assert not worker.is_alive()
        if errors:
            raise errors[0]
    assert checksum == count * (len(payload) if lane == "raw" else 42)
    return {"lane": lane, "fixture": fixture, "count": count, "encoded_bytes": len(payload),
            "ns_per_message": elapsed / count, "process_cpu_ns_per_message": used_cpu / count,
            "reader_cpu_ns_per_message": used_thread_cpu / count, "checksum": checksum,
            "payload_sha256": hashlib.sha256(payload).hexdigest(), "backend": VIEW_BACKEND,
            "limits": ["one DATA in flight; test queue pacing included", "process CPU includes the peer thread",
                       "raw lane requests bytes only; view-native and legacy request identical native output"]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("lane", choices=["raw", "legacy", "view-selected", "view-native"])
    parser.add_argument("fixture", choices=["small", "wide", "text32k", "array"])
    parser.add_argument("--count", type=int, default=1000)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.count <= 0:
        parser.error("count must be positive")
    result = json.dumps(run(args.lane, args.fixture, args.count), indent=2) + "\n"
    if args.output:
        args.output.write_text(result)
    else:
        print(result, end="")
