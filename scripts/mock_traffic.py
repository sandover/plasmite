"""Feed mock messages into the map's sample pools at different rates.

Opens one long-running `plasmite feed <pool>` per pool and writes JSON lines
to it, so a busy pool costs one process, not one per message. Stops after
--minutes, or on Ctrl-C. The nine named pools must already exist.

Usage: python3 scripts/mock_traffic.py --dir DIR [--bin PATH] [--minutes N]
"""

import argparse
import json
import random
import subprocess
import time
from pathlib import Path

WORDS = (
    "queue worker retry upload cache shard lease token deploy rollback index "
    "compaction snapshot replica heartbeat timeout backlog cursor offset ring "
    "writer reader tail head flush commit batch window latency throughput"
).split()
NICKS = ["brandon", "ava", "kenji", "mira", "ola", "sam", "tariq", "zoe"]
HOSTS = ["a1", "a2", "b1", "edge-3", "db-primary"]


def sentence(low, high):
    return " ".join(random.choice(WORDS) for _ in range(random.randint(low, high)))


def tick(n):
    return {"message": f"tick {n}", "level": "info"}


def metric(n):
    return {
        "host": random.choice(HOSTS),
        "cpu": round(random.random(), 3),
        "mem_mb": random.randint(200, 4000),
        "rps": random.randint(0, 900),
    }


def chat(n):
    return {"nick": random.choice(NICKS), "message": sentence(3, 14)}


def mixed(n):
    size = random.choice([20, 60, 200, 800, 3000])
    return {"message": f"event {n}", "level": random.choice(["info", "info", "warn"]), "body": "x" * size}


def event(n):
    return {"kind": random.choice(["login", "signup", "purchase", "logout"]), "user": random.randint(1, 5000),
            "message": sentence(4, 10)}


def alert(n):
    level = random.choice(["warn", "warn", "error"])
    return {"level": level, "message": sentence(5, 18), "host": random.choice(HOSTS)}


def build_line(n):
    return {"level": "info", "message": f"step {n % 40}: " + sentence(10, 60)}


def deployment(n):
    return {"kind": "deploy", "service": random.choice(["api", "web", "worker"]), "version": f"1.{n}.0",
            "message": f"rolled out to {random.randint(1, 40)} hosts",
            "manifest": [sentence(8, 20) for _ in range(random.randint(12, 45))]}


def record(n):
    return {"kind": "archive", "id": n, "payload": "".join(random.choice("abcdef0123456789") for _ in range(4000))}


# pool: (message maker, seconds between messages, or a (burst size, burst gap) pair)
PLAN = {
    "ticker": (tick, 0.05),
    "metrics": (metric, 0.125),
    "chat": (chat, (1, 0.5)),
    "mixed": (mixed, 0.33),
    "events": (event, 1.0),
    "alerts": (alert, (1, 7.0)),
    "builds": (build_line, (30, 20.0)),
    "deployment-events": (deployment, 2.0),
    "archive": (record, 0.04),
}


def next_gap(schedule):
    """Seconds until the next send, and how many messages to send then."""
    if isinstance(schedule, tuple):
        burst, gap = schedule
        count = random.randint(max(1, burst // 2), burst + burst // 3) if burst > 1 else 1
        return random.uniform(gap * 0.5, gap * 1.5), count
    return schedule, 1


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dir", required=True)
    parser.add_argument("--bin", default=str(Path(__file__).resolve().parents[1] / "target/debug/plasmite"))
    parser.add_argument("--minutes", type=float, default=30)
    args = parser.parse_args()

    missing = [pool for pool in PLAN if not (Path(args.dir) / f"{pool}.plasmite").is_file()]
    if missing:
        parser.error("missing pools: " + ", ".join(missing))

    feeds = {pool: subprocess.Popen([args.bin, "--dir", args.dir, "feed", pool, "--in", "jsonl"], stdin=subprocess.PIPE,
                                    stdout=subprocess.DEVNULL, text=True)
             for pool in PLAN}
    counts = dict.fromkeys(PLAN, 0)
    due = {pool: time.monotonic() for pool in PLAN}
    stop = time.monotonic() + args.minutes * 60
    last_report = time.monotonic()
    try:
        while time.monotonic() < stop:
            now = time.monotonic()
            for pool, (make, schedule) in PLAN.items():
                if now < due[pool]:
                    continue
                gap, count = next_gap(schedule)
                for _ in range(count):
                    counts[pool] += 1
                    feeds[pool].stdin.write(json.dumps(make(counts[pool])) + "\n")
                feeds[pool].stdin.flush()
                due[pool] = now + gap
            if now - last_report > 10:
                print(" ".join(f"{pool}={n}" for pool, n in counts.items()), flush=True)
                last_report = now
            time.sleep(max(0.005, min(due.values()) - time.monotonic()))
    except KeyboardInterrupt:
        pass
    finally:
        for feed in feeds.values():
            feed.stdin.close()
            feed.wait(timeout=5)
        print("sent", counts, flush=True)


if __name__ == "__main__":
    main()
