"""
Purpose: Exercise the Python cookbook snippet path in smoke automation.
Key Exports: main (script entrypoint).
Role: Validate local client.pool(), typed append return fields, and not-found handling.
Invariants: Runs against local pool directories only; no network usage.
Invariants: Produces deterministic JSON output for shell assertions.
Notes: Designed for scripts/cookbook_smoke.sh integration.
"""

from __future__ import annotations

import json
from pathlib import Path
import shutil
import sys

from plasmite import Client, Durability, NotFoundError


def main() -> int:
    if len(sys.argv) != 2:
        raise RuntimeError("usage: cookbook_smoke_fixture.py <pool-dir>")
    pool_dir = Path(sys.argv[1])
    if pool_dir.exists():
        shutil.rmtree(pool_dir)
    pool_dir.mkdir(parents=True, exist_ok=True)

    with Client(str(pool_dir)) as client:
        with client.pool("cookbook-smoke", 1024 * 1024) as pool:
            msg = pool.append({"task": "resize", "id": 1}, ["cookbook"], Durability.FAST)
            if msg.seq < 1:
                raise RuntimeError("expected positive seq")
            if msg.tags != ["cookbook"]:
                raise RuntimeError(f"unexpected tags: {msg.tags}")
            if msg.data.get("task") != "resize":
                raise RuntimeError(f"unexpected data: {msg.data}")

        try:
            client.open_pool("missing-cookbook-smoke-pool")
            raise RuntimeError("expected not-found error")
        except NotFoundError:
            pass

        print(json.dumps({"seq": msg.seq, "tags": msg.tags, "data": msg.data}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
