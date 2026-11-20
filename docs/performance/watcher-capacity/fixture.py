"""Start a disposable Plasmite server with synthetic benchmark credentials."""

from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time


def loopback_port():
    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        return reserved.getsockname()[1]


@contextmanager
def running(binary, pools, ready, access, origin, port, extra_args):
    env = {**os.environ, "PLASMITE_SERVE_READY_FILE": str(ready), "PLASMITE_ACCESS_HOME": str(access)}
    executable = [str(binary.resolve()), "--dir", str(pools)]
    command = [
        *executable, "serve", *extra_args,
        "--bind", "127.0.0.1:0", "--remote-bind", f"127.0.0.1:{port}",
        "--shared-address", origin,
    ]
    log_path = ready.parent / "server.log"
    ready.unlink(missing_ok=True)
    with log_path.open("wb") as log, subprocess.Popen(command, env=env, stdout=log, stderr=log) as server:
        try:
            address = pools / ".plasmite-serve" / "local.json"
            deadline = time.monotonic() + 15
            while not (ready.exists() and address.exists()):
                if server.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(log_path.read_text(errors="replace"))
                time.sleep(.02)
            yield server.pid, "http://" + json.loads(address.read_text()), executable, env
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


@contextmanager
def temporary_server(binary, https, extra_args):
    with tempfile.TemporaryDirectory(prefix="plasmite-watchers-") as directory:
        root = Path(directory)
        ready = root / "ready"
        pools = root / "pools"
        pools.mkdir()
        access = root / "access"
        access.mkdir()
        port = loopback_port() if https else 0
        origin = f"https://127.0.0.1:{port or 9743}"

        with running(binary, pools, ready, access, origin, port, extra_args) as (
            pid, local_url, executable, env
        ):
            if not https:
                yield local_url, pid, None, None, None
                return
            invite = subprocess.check_output(
                [*executable, "access", "invite", "--name", "watcher-benchmark", "--format", "json"],
                env=env,
            )
            key = json.loads(invite)["access_key"]
            keys = subprocess.check_output([*executable, "access", "keys", "--format", "json"], env=env)
            key_id = json.loads(keys)["keys"][0]["id"]

        state = pools / ".plasmite-serve"
        identity = json.loads((state / "identity.json").read_text())
        certificate = state / identity["cert_file"]
        token = secrets.token_hex(32)
        # Store the fixture grant in the throwaway directory. The running
        # server still validates the key and token on its production routes.
        descriptor = os.open(state / "oauth.json", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(descriptor, "w") as output:
            json.dump({
                "codes": [],
                "grants": [{
                    "id": "watcher-benchmark", "client_id": "watcher-benchmark",
                    "resource": origin + "/mcp", "key_id": key_id, "refresh_hash": "",
                }],
                "access_tokens": [{
                    "hash": hashlib.sha256(token.encode()).hexdigest(),
                    "grant_id": "watcher-benchmark", "expires_at": int(time.time()) + 1800,
                }],
            }, output)
        with running(binary, pools, ready, access, origin, port, extra_args) as (pid, _, _, _):
            yield origin, pid, certificate, key, token
