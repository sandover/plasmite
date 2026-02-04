"""
Purpose: Execute conformance manifests against the Python binding.
Key Exports: None (script entry point).
Role: Reference runner for JSON conformance manifests in Python.
Invariants: Manifests are JSON-only; steps execute in order; fail-fast on errors.
Invariants: Workdir is isolated under the manifest directory.
Notes: Mirrors Rust/Go/Node conformance runner behavior.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

from plasmite import Client, Durability, PlasmiteError, parse_message


def main() -> None:
    if len(sys.argv) != 2:
        raise RuntimeError("usage: plasmite-conformance <path/to/manifest.json>")

    manifest_path = Path(sys.argv[1]).resolve()
    manifest_dir = manifest_path.parent
    repo_root = manifest_dir.parent

    content = manifest_path.read_text(encoding="utf-8")
    manifest = json.loads(content)

    if manifest.get("conformance_version") != 0:
        raise RuntimeError(f"unsupported conformance_version: {manifest.get('conformance_version')}")

    workdir = manifest.get("workdir") or "work"
    workdir_path = manifest_dir / workdir
    reset_workdir(workdir_path)

    client = Client(str(workdir_path))

    for index, step in enumerate(manifest.get("steps", [])):
        step_id = step.get("id")
        op = step.get("op")
        if not op:
            raise step_err(index, step_id, "missing op")
        if op == "create_pool":
            run_create_pool(client, step, index, step_id)
        elif op == "append":
            run_append(client, step, index, step_id)
        elif op == "get":
            run_get(client, step, index, step_id)
        elif op == "tail":
            run_tail(client, step, index, step_id)
        elif op == "spawn_poke":
            run_spawn_poke(repo_root, workdir_path, step, index, step_id)
        elif op == "corrupt_pool_header":
            run_corrupt_pool_header(workdir_path, step, index, step_id)
        elif op == "chmod_path":
            run_chmod_path(step, index, step_id)
        else:
            raise step_err(index, step_id, f"unknown op: {op}")


def reset_workdir(path: Path) -> None:
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True, exist_ok=True)


def run_create_pool(client: Client, step: dict[str, Any], index: int, step_id: str | None) -> None:
    pool = require_pool(step, index, step_id)
    size_bytes = step.get("input", {}).get("size_bytes", 1024 * 1024)
    err = try_call(lambda: client.create_pool(pool, int(size_bytes))).error
    validate_expect_error(step.get("expect"), err, index, step_id)


def run_append(client: Client, step: dict[str, Any], index: int, step_id: str | None) -> None:
    pool = require_pool(step, index, step_id)
    input_data = require_input(step, index, step_id)
    if "data" not in input_data:
        raise step_err(index, step_id, "missing input.data")
    descrips = input_data.get("descrips", [])

    pool_handle = try_call(lambda: client.open_pool(pool))
    if pool_handle.error:
        validate_expect_error(step.get("expect"), pool_handle.error, index, step_id)
        return

    payload = json.dumps(input_data["data"]).encode("utf-8")
    result = try_call(lambda: pool_handle.value.append_json(payload, descrips, Durability.FAST))
    pool_handle.value.close()
    if result.error:
        validate_expect_error(step.get("expect"), result.error, index, step_id)
        return
    validate_expect_error(step.get("expect"), None, index, step_id)

    if step.get("expect", {}).get("seq") is not None:
        message = parse_message(result.value)
        if message.get("seq") != step["expect"]["seq"]:
            raise step_err(index, step_id, f"expected seq {step['expect']['seq']}, got {message.get('seq')}")


def run_get(client: Client, step: dict[str, Any], index: int, step_id: str | None) -> None:
    pool = require_pool(step, index, step_id)
    input_data = require_input(step, index, step_id)
    if "seq" not in input_data:
        raise step_err(index, step_id, "missing input.seq")

    pool_handle = try_call(lambda: client.open_pool(pool))
    if pool_handle.error:
        validate_expect_error(step.get("expect"), pool_handle.error, index, step_id)
        return

    result = try_call(lambda: pool_handle.value.get_json(int(input_data["seq"])))
    pool_handle.value.close()
    if result.error:
        validate_expect_error(step.get("expect"), result.error, index, step_id)
        return
    validate_expect_error(step.get("expect"), None, index, step_id)

    message = parse_message(result.value)
    if step.get("expect", {}).get("data") is not None and step["expect"]["data"] != message.get("data"):
        raise step_err(index, step_id, "data mismatch")
    if step.get("expect", {}).get("descrips") is not None and step["expect"]["descrips"] != message.get("meta", {}).get("descrips"):
        raise step_err(index, step_id, "descrips mismatch")


def run_tail(client: Client, step: dict[str, Any], index: int, step_id: str | None) -> None:
    pool = require_pool(step, index, step_id)
    input_data = step.get("input", {})
    since_seq = input_data.get("since_seq")
    max_messages = input_data.get("max")

    pool_handle = try_call(lambda: client.open_pool(pool))
    if pool_handle.error:
        validate_expect_error(step.get("expect"), pool_handle.error, index, step_id)
        return

    stream = try_call(
        lambda: pool_handle.value.open_stream(
            int(since_seq) if since_seq is not None else None,
            int(max_messages) if max_messages is not None else None,
            500,
        )
    )
    if stream.error:
        pool_handle.value.close()
        validate_expect_error(step.get("expect"), stream.error, index, step_id)
        return

    messages = []
    while True:
        payload = stream.value.next_json()
        if payload is None:
            break
        messages.append(parse_message(payload))
        if max_messages is not None and len(messages) >= int(max_messages):
            break
    stream.value.close()
    pool_handle.value.close()

    validate_expect_error(step.get("expect"), None, index, step_id)

    expected = expected_messages(step.get("expect"), index, step_id)
    if len(messages) != len(expected["messages"]):
        raise step_err(index, step_id, f"expected {len(expected['messages'])} messages, got {len(messages)}")

    for idx in range(1, len(messages)):
        if messages[idx - 1]["seq"] >= messages[idx]["seq"]:
            raise step_err(index, step_id, "tail messages out of order")

    if expected["ordered"]:
        for idx, entry in enumerate(expected["messages"]):
            if entry["data"] != messages[idx]["data"]:
                raise step_err(index, step_id, "data mismatch")
            if "descrips" in entry and entry["descrips"] != messages[idx]["meta"]["descrips"]:
                raise step_err(index, step_id, "descrips mismatch")
    else:
        remaining = messages[:]
        for entry in expected["messages"]:
            matched = False
            for idx, actual in enumerate(remaining):
                if entry["data"] != actual["data"]:
                    continue
                if "descrips" in entry and entry["descrips"] != actual["meta"]["descrips"]:
                    continue
                remaining.pop(idx)
                matched = True
                break
            if not matched:
                raise step_err(index, step_id, "message mismatch")


def run_spawn_poke(repo_root: Path, workdir_path: Path, step: dict[str, Any], index: int, step_id: str | None) -> None:
    pool = require_pool(step, index, step_id)
    input_data = require_input(step, index, step_id)
    messages = input_data.get("messages")
    if not isinstance(messages, list):
        raise step_err(index, step_id, "input.messages must be array")

    plasmite_bin = resolve_plasmite_bin(repo_root)

    processes = []
    for message in messages:
        if "data" not in message:
            raise step_err(index, step_id, "message.data is required")
        payload = json.dumps(message["data"])
        descrips = message.get("descrips", [])
        if not isinstance(descrips, list):
            raise step_err(index, step_id, "message.descrips must be array")
        args = [plasmite_bin, "--dir", str(workdir_path), "poke", pool, payload]
        for descrip in descrips:
            args.extend(["--descrip", descrip])
        processes.append(subprocess.Popen(args))

    for proc in processes:
        code = proc.wait()
        if code != 0:
            raise step_err(index, step_id, "poke process failed")


def run_corrupt_pool_header(workdir_path: Path, step: dict[str, Any], index: int, step_id: str | None) -> None:
    pool = require_pool(step, index, step_id)
    path = resolve_pool_path(workdir_path, pool)
    Path(path).write_bytes(b"NOPE")


def run_chmod_path(step: dict[str, Any], index: int, step_id: str | None) -> None:
    if sys.platform == "win32":
        raise step_err(index, step_id, "chmod_path is not supported on this platform")
    input_data = require_input(step, index, step_id)
    path = input_data.get("path")
    mode = input_data.get("mode")
    if not path:
        raise step_err(index, step_id, "missing input.path")
    if not mode:
        raise step_err(index, step_id, "missing input.mode")
    os.chmod(path, int(mode, 8))


def expected_messages(expect: dict[str, Any] | None, index: int, step_id: str | None) -> dict[str, Any]:
    if not expect:
        raise step_err(index, step_id, "missing expect")
    if "messages" in expect and "messages_unordered" in expect:
        raise step_err(index, step_id, "expect.messages and expect.messages_unordered are mutually exclusive")
    if isinstance(expect.get("messages"), list):
        return {"ordered": True, "messages": expect["messages"]}
    if isinstance(expect.get("messages_unordered"), list):
        return {"ordered": False, "messages": expect["messages_unordered"]}
    raise step_err(index, step_id, "expect.messages or expect.messages_unordered is required")


def validate_expect_error(expect: dict[str, Any] | None, err: Exception | None, index: int, step_id: str | None) -> None:
    if not expect or "error" not in expect:
        if err is None:
            return
        raise step_err(index, step_id, f"unexpected error: {err}")
    if err is None:
        raise step_err(index, step_id, "expected error but operation succeeded")
    if not isinstance(err, PlasmiteError):
        raise step_err(index, step_id, f"unexpected error type: {err}")

    expect_err = expect["error"]
    if expect_err.get("kind") != error_kind_label(err.kind):
        raise step_err(index, step_id, f"expected error kind {expect_err.get('kind')}, got {error_kind_label(err.kind)}")
    if expect_err.get("message_contains") and expect_err["message_contains"] not in str(err):
        raise step_err(index, step_id, f"expected message to contain '{expect_err['message_contains']}', got '{err}'")
    if "has_path" in expect_err and expect_err["has_path"] != (err.path is not None):
        raise step_err(index, step_id, "path presence mismatch")
    if "has_seq" in expect_err and expect_err["has_seq"] != (err.seq is not None):
        raise step_err(index, step_id, "seq presence mismatch")
    if "has_offset" in expect_err and expect_err["has_offset"] != (err.offset is not None):
        raise step_err(index, step_id, "offset presence mismatch")


def resolve_plasmite_bin(repo_root: Path) -> str:
    env_bin = os.environ.get("PLASMITE_BIN")
    if env_bin:
        return env_bin
    candidate = repo_root / "target" / "debug" / "plasmite"
    if candidate.exists():
        return str(candidate)
    raise RuntimeError("plasmite binary not found; set PLASMITE_BIN or build target/debug/plasmite")


def error_kind_label(kind: Any) -> str:
    mapping = {
        "INTERNAL": "Internal",
        "USAGE": "Usage",
        "NOT_FOUND": "NotFound",
        "ALREADY_EXISTS": "AlreadyExists",
        "BUSY": "Busy",
        "PERMISSION": "Permission",
        "CORRUPT": "Corrupt",
        "IO": "Io",
    }
    if hasattr(kind, "name"):
        return mapping.get(kind.name, "Internal")
    return mapping.get(str(kind), "Internal")


def resolve_pool_path(workdir_path: Path, pool: str) -> str:
    if "/" in pool:
        return pool
    if pool.endswith(".plasmite"):
        return str(workdir_path / pool)
    return str(workdir_path / f"{pool}.plasmite")


def require_pool(step: dict[str, Any], index: int, step_id: str | None) -> str:
    pool = step.get("pool")
    if not pool:
        raise step_err(index, step_id, "missing pool")
    return pool


def require_input(step: dict[str, Any], index: int, step_id: str | None) -> dict[str, Any]:
    input_data = step.get("input")
    if not input_data:
        raise step_err(index, step_id, "missing input")
    return input_data


def step_err(index: int, step_id: str | None, message: str) -> RuntimeError:
    out = f"step {index}"
    if step_id:
        out = f"{out} ({step_id})"
    return RuntimeError(f"{out}: {message}")


class CallResult:
    def __init__(self, value: Any = None, error: Exception | None = None) -> None:
        self.value = value
        self.error = error


def try_call(fn) -> CallResult:
    try:
        return CallResult(value=fn())
    except Exception as err:
        return CallResult(error=err)


if __name__ == "__main__":
    try:
        main()
    except Exception as err:
        print(err, file=sys.stderr)
        sys.exit(1)
