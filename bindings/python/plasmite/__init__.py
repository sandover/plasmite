"""
Purpose: Provide Python bindings for the libplasmite C ABI (v0).
Key Exports: Client, Pool, Stream, Durability, ErrorKind, PlasmiteError.
Role: Minimal, ergonomic wrapper around include/plasmite.h for Python users.
Invariants: JSON bytes in/out; explicit close/free for native handles.
Invariants: Errors preserve stable kinds and context fields.
Notes: Uses ctypes and links to libplasmite resolved at runtime.
"""

from __future__ import annotations

from dataclasses import dataclass
from ctypes import (
    CDLL,
    POINTER,
    Structure,
    byref,
    c_char_p,
    c_int,
    c_size_t,
    c_uint32,
    c_uint64,
    c_uint8,
    c_void_p,
)
from datetime import datetime, timezone
from enum import IntEnum
import json
import os
from pathlib import Path
import sys
import time as _time
from typing import Any, Generator, Iterable, Optional


class ErrorKind(IntEnum):
    INTERNAL = 1
    USAGE = 2
    NOT_FOUND = 3
    ALREADY_EXISTS = 4
    BUSY = 5
    PERMISSION = 6
    CORRUPT = 7
    IO = 8


class Durability(IntEnum):
    FAST = 0
    FLUSH = 1


class PlasmiteError(RuntimeError):
    def __init__(
        self,
        kind: ErrorKind,
        message: str,
        path: str | None = None,
        seq: int | None = None,
        offset: int | None = None,
    ) -> None:
        super().__init__(message)
        self.kind = kind
        self.path = path
        self.seq = seq
        self.offset = offset


class NotFoundError(PlasmiteError):
    pass


class AlreadyExistsError(PlasmiteError):
    pass


class BusyError(PlasmiteError):
    pass


class PermissionDeniedError(PlasmiteError):
    pass


class CorruptError(PlasmiteError):
    pass


class IoError(PlasmiteError):
    pass


class UsageError(PlasmiteError):
    pass


class InternalError(PlasmiteError):
    pass


@dataclass(frozen=True, slots=True)
class MessageMeta:
    tags: list[str]


@dataclass(frozen=True, slots=True)
class Message:
    seq: int
    time: datetime
    time_rfc3339: str
    data: Any
    meta: MessageMeta
    raw: bytes

    @property
    def tags(self) -> list[str]:
        return self.meta.tags


class plsm_client_t(Structure):
    pass


class plsm_pool_t(Structure):
    pass


class plsm_stream_t(Structure):
    pass


class plsm_lite3_stream_t(Structure):
    pass


class plsm_buf_t(Structure):
    _fields_ = [
        ("data", c_void_p),
        ("len", c_size_t),
    ]


class plsm_lite3_frame_t(Structure):
    _fields_ = [
        ("seq", c_uint64),
        ("timestamp_ns", c_uint64),
        ("flags", c_uint32),
        ("payload", plsm_buf_t),
    ]


class plsm_error_t(Structure):
    _fields_ = [
        ("kind", c_int),
        ("message", c_char_p),
        ("path", c_char_p),
        ("seq", c_uint64),
        ("offset", c_uint64),
        ("has_seq", c_uint8),
        ("has_offset", c_uint8),
    ]


def _load_lib() -> CDLL:
    env_dir = os.environ.get("PLASMITE_LIB_DIR")
    if env_dir:
        candidates = [
            os.path.join(env_dir, name)
            for name in ("plasmite.dll", "libplasmite.dylib", "libplasmite.so")
        ]
        for candidate in candidates:
            if os.path.exists(candidate):
                return CDLL(candidate)
    native_dir = Path(__file__).resolve().parent / "_native"
    for candidate in (
        native_dir / "plasmite.dll",
        native_dir / "libplasmite.dylib",
        native_dir / "libplasmite.so",
    ):
        if candidate.exists():
            return CDLL(str(candidate))
    for name in ("plasmite.dll", "plasmite", "libplasmite"):
        try:
            return CDLL(name)
        except OSError:
            continue
    raise OSError(
        "libplasmite not found; set PLASMITE_LIB_DIR or build target/debug/libplasmite.*"
    )


_LIB = _load_lib()

DEFAULT_POOL_SIZE_BYTES = 1024 * 1024
DEFAULT_POOL_SIZE = DEFAULT_POOL_SIZE_BYTES


def default_pool_dir() -> str:
    # Compute lazily so tests/tools can override HOME in-process without re-importing.
    # `expanduser("~")` consults HOME at call time on Unix.
    return os.path.join(os.path.expanduser("~"), ".plasmite", "pools")


DEFAULT_POOL_DIR = default_pool_dir()

_LIB.plsm_client_new.argtypes = [c_char_p, POINTER(POINTER(plsm_client_t)), POINTER(POINTER(plsm_error_t))]
_LIB.plsm_client_new.restype = c_int
_LIB.plsm_client_free.argtypes = [POINTER(plsm_client_t)]
_LIB.plsm_client_free.restype = None

_LIB.plsm_pool_create.argtypes = [
    POINTER(plsm_client_t),
    c_char_p,
    c_uint64,
    POINTER(POINTER(plsm_pool_t)),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_pool_create.restype = c_int
_LIB.plsm_pool_open.argtypes = [
    POINTER(plsm_client_t),
    c_char_p,
    POINTER(POINTER(plsm_pool_t)),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_pool_open.restype = c_int
_LIB.plsm_pool_free.argtypes = [POINTER(plsm_pool_t)]
_LIB.plsm_pool_free.restype = None

_LIB.plsm_pool_append_json.argtypes = [
    POINTER(plsm_pool_t),
    POINTER(c_uint8),
    c_size_t,
    POINTER(c_char_p),
    c_size_t,
    c_uint32,
    POINTER(plsm_buf_t),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_pool_append_json.restype = c_int

_LIB.plsm_pool_append_lite3.argtypes = [
    POINTER(plsm_pool_t),
    POINTER(c_uint8),
    c_size_t,
    c_uint32,
    POINTER(c_uint64),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_pool_append_lite3.restype = c_int

_LIB.plsm_pool_get_json.argtypes = [
    POINTER(plsm_pool_t),
    c_uint64,
    POINTER(plsm_buf_t),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_pool_get_json.restype = c_int

_LIB.plsm_pool_get_lite3.argtypes = [
    POINTER(plsm_pool_t),
    c_uint64,
    POINTER(plsm_lite3_frame_t),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_pool_get_lite3.restype = c_int

_LIB.plsm_stream_open.argtypes = [
    POINTER(plsm_pool_t),
    c_uint64,
    c_uint32,
    c_uint64,
    c_uint32,
    c_uint64,
    c_uint32,
    POINTER(POINTER(plsm_stream_t)),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_stream_open.restype = c_int

_LIB.plsm_lite3_stream_open.argtypes = [
    POINTER(plsm_pool_t),
    c_uint64,
    c_uint32,
    c_uint64,
    c_uint32,
    c_uint64,
    c_uint32,
    POINTER(POINTER(plsm_lite3_stream_t)),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_lite3_stream_open.restype = c_int

_LIB.plsm_stream_next.argtypes = [
    POINTER(plsm_stream_t),
    POINTER(plsm_buf_t),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_stream_next.restype = c_int

_LIB.plsm_lite3_stream_next.argtypes = [
    POINTER(plsm_lite3_stream_t),
    POINTER(plsm_lite3_frame_t),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_lite3_stream_next.restype = c_int

_LIB.plsm_stream_free.argtypes = [POINTER(plsm_stream_t)]
_LIB.plsm_stream_free.restype = None

_LIB.plsm_lite3_stream_free.argtypes = [POINTER(plsm_lite3_stream_t)]
_LIB.plsm_lite3_stream_free.restype = None

_LIB.plsm_buf_free.argtypes = [POINTER(plsm_buf_t)]
_LIB.plsm_buf_free.restype = None

_LIB.plsm_lite3_frame_free.argtypes = [POINTER(plsm_lite3_frame_t)]
_LIB.plsm_lite3_frame_free.restype = None

_LIB.plsm_error_free.argtypes = [POINTER(plsm_error_t)]
_LIB.plsm_error_free.restype = None

_ERROR_KIND_TO_CLASS: dict[ErrorKind, type[PlasmiteError]] = {
    ErrorKind.INTERNAL: InternalError,
    ErrorKind.USAGE: UsageError,
    ErrorKind.NOT_FOUND: NotFoundError,
    ErrorKind.ALREADY_EXISTS: AlreadyExistsError,
    ErrorKind.BUSY: BusyError,
    ErrorKind.PERMISSION: PermissionDeniedError,
    ErrorKind.CORRUPT: CorruptError,
    ErrorKind.IO: IoError,
}


def _take_error(err_ptr: POINTER(plsm_error_t) | None) -> PlasmiteError:
    if not err_ptr:
        return PlasmiteError(ErrorKind.INTERNAL, "plasmite: unknown error")
    err = err_ptr.contents
    raw_kind = int(err.kind)
    message = err.message.decode("utf-8") if err.message else ""
    path = err.path.decode("utf-8") if err.path else None
    seq = int(err.seq) if err.has_seq else None
    offset = int(err.offset) if err.has_offset else None
    _LIB.plsm_error_free(err_ptr)
    try:
        kind = ErrorKind(raw_kind)
    except ValueError:
        kind = ErrorKind.INTERNAL
    if not message:
        message = _default_error_message(kind)
    error_cls = _ERROR_KIND_TO_CLASS.get(kind, PlasmiteError)
    return error_cls(kind, message, path, seq, offset)


def _default_error_message(kind: ErrorKind) -> str:
    mapping = {
        ErrorKind.INTERNAL: "internal error",
        ErrorKind.USAGE: "usage error",
        ErrorKind.NOT_FOUND: "not found",
        ErrorKind.ALREADY_EXISTS: "already exists",
        ErrorKind.BUSY: "busy",
        ErrorKind.PERMISSION: "permission denied",
        ErrorKind.CORRUPT: "corrupt",
        ErrorKind.IO: "io error",
    }
    return mapping.get(kind, "error")


def _buf_to_bytes(buf: plsm_buf_t) -> bytes:
    if not buf.data or buf.len == 0:
        _LIB.plsm_buf_free(byref(buf))
        return b""
    data = (c_uint8 * buf.len).from_address(buf.data)
    out = bytes(data)
    _LIB.plsm_buf_free(byref(buf))
    return out


def _frame_to_py(frame: plsm_lite3_frame_t) -> Lite3Frame:
    seq = int(frame.seq)
    timestamp_ns = int(frame.timestamp_ns)
    flags = int(frame.flags)
    payload = b""
    if frame.payload.data and frame.payload.len:
        data = (c_uint8 * frame.payload.len).from_address(frame.payload.data)
        payload = bytes(data)
    _LIB.plsm_lite3_frame_free(byref(frame))
    return Lite3Frame(seq=seq, timestamp_ns=timestamp_ns, flags=flags, payload=payload)


def _tag_array(values: Iterable[str]) -> tuple[POINTER(c_char_p), list[bytes]]:
    encoded = [value.encode("utf-8") for value in values]
    if not encoded:
        return POINTER(c_char_p)(), encoded
    arr = (c_char_p * len(encoded))()
    for idx, val in enumerate(encoded):
        arr[idx] = val
    return arr, encoded


def _closed_error(target: str) -> PlasmiteError:
    return PlasmiteError(ErrorKind.USAGE, f"plasmite: {target} is closed")


def _require_open(ptr: object, target: str) -> None:
    if not ptr:
        raise _closed_error(target)


def _ensure_bytes(payload: bytes | bytearray | memoryview, name: str) -> bytes:
    if isinstance(payload, bytes):
        return payload
    if isinstance(payload, (bytearray, memoryview)):
        return bytes(payload)
    raise TypeError(f"{name} must be bytes-like")


def _ensure_non_negative_int(value: int, name: str) -> int:
    if not isinstance(value, int):
        raise TypeError(f"{name} must be an int")
    if value < 0:
        raise ValueError(f"{name} must be >= 0")
    return value


def _optional_non_negative_int(value: Optional[int], name: str) -> Optional[int]:
    if value is None:
        return None
    return _ensure_non_negative_int(value, name)


class Client:
    def __init__(self, pool_dir: str | None = None) -> None:
        if pool_dir is None:
            pool_dir = default_pool_dir()
        if not pool_dir:
            raise ValueError("pool_dir is required")
        out_client = POINTER(plsm_client_t)()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_client_new(pool_dir.encode("utf-8"), byref(out_client), byref(out_err))
        if rc != 0:
            raise _take_error(out_err)
        self._ptr = out_client

    def create_pool(
        self,
        pool_ref: str,
        size_bytes: int = DEFAULT_POOL_SIZE_BYTES,
    ) -> Pool:
        _require_open(self._ptr, "client")
        if not pool_ref:
            raise ValueError("pool_ref is required")
        size_bytes = _ensure_non_negative_int(size_bytes, "size_bytes")
        out_pool = POINTER(plsm_pool_t)()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_pool_create(
            self._ptr,
            pool_ref.encode("utf-8"),
            c_uint64(size_bytes),
            byref(out_pool),
            byref(out_err),
        )
        if rc != 0:
            raise _take_error(out_err)
        return Pool(out_pool)

    def open_pool(self, pool_ref: str) -> Pool:
        _require_open(self._ptr, "client")
        if not pool_ref:
            raise ValueError("pool_ref is required")
        out_pool = POINTER(plsm_pool_t)()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_pool_open(
            self._ptr,
            pool_ref.encode("utf-8"),
            byref(out_pool),
            byref(out_err),
        )
        if rc != 0:
            raise _take_error(out_err)
        return Pool(out_pool)

    def pool(
        self,
        pool_ref: str,
        size_bytes: int = DEFAULT_POOL_SIZE_BYTES,
    ) -> Pool:
        try:
            return self.open_pool(pool_ref)
        except NotFoundError:
            return self.create_pool(pool_ref, size_bytes)

    def close(self) -> None:
        if getattr(self, "_ptr", None):
            _LIB.plsm_client_free(self._ptr)
            self._ptr = None

    def __enter__(self) -> Client:
        _require_open(self._ptr, "client")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


@dataclass(frozen=True, slots=True)
class Lite3Frame:
    seq: int
    timestamp_ns: int
    flags: int
    payload: bytes

    @property
    def time(self) -> datetime:
        return datetime.fromtimestamp(self.timestamp_ns / 1_000_000_000, tz=timezone.utc)


class Pool:
    def __init__(self, ptr: POINTER(plsm_pool_t)) -> None:
        self._ptr = ptr

    def append_json(self, payload: bytes, tags: Iterable[str], durability: Durability) -> bytes:
        _require_open(self._ptr, "pool")
        payload = _ensure_bytes(payload, "payload")
        if not payload:
            raise ValueError("payload is required")
        buf = plsm_buf_t()
        out_err = POINTER(plsm_error_t)()
        arr, _keep = _tag_array(tags)
        payload_buf = (c_uint8 * len(payload)).from_buffer_copy(payload)
        rc = _LIB.plsm_pool_append_json(
            self._ptr,
            payload_buf,
            c_size_t(len(payload)),
            arr,
            c_size_t(len(_keep)),
            c_uint32(int(durability)),
            byref(buf),
            byref(out_err),
        )
        if rc != 0:
            raise _take_error(out_err)
        return _buf_to_bytes(buf)

    def append(
        self,
        value,
        tags: Optional[Iterable[str]] = None,
        durability: Durability = Durability.FAST,
    ) -> Message:
        payload = self.append_json(
            json.dumps(value).encode("utf-8"),
            [] if tags is None else tags,
            durability,
        )
        return parse_message(payload)

    def append_lite3(self, payload: bytes, durability: Durability) -> int:
        _require_open(self._ptr, "pool")
        payload = _ensure_bytes(payload, "payload")
        if not payload:
            raise ValueError("payload is required")
        out_seq = c_uint64()
        out_err = POINTER(plsm_error_t)()
        payload_buf = (c_uint8 * len(payload)).from_buffer_copy(payload)
        rc = _LIB.plsm_pool_append_lite3(
            self._ptr,
            payload_buf,
            c_size_t(len(payload)),
            c_uint32(int(durability)),
            byref(out_seq),
            byref(out_err),
        )
        if rc != 0:
            raise _take_error(out_err)
        return int(out_seq.value)

    def get_json(self, seq: int) -> bytes:
        _require_open(self._ptr, "pool")
        seq = _ensure_non_negative_int(seq, "seq")
        buf = plsm_buf_t()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_pool_get_json(self._ptr, c_uint64(seq), byref(buf), byref(out_err))
        if rc != 0:
            raise _take_error(out_err)
        return _buf_to_bytes(buf)

    def get(self, seq: int) -> Message:
        return parse_message(self.get_json(seq))

    def get_lite3(self, seq: int) -> Lite3Frame:
        _require_open(self._ptr, "pool")
        seq = _ensure_non_negative_int(seq, "seq")
        frame = plsm_lite3_frame_t()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_pool_get_lite3(self._ptr, c_uint64(seq), byref(frame), byref(out_err))
        if rc != 0:
            raise _take_error(out_err)
        return _frame_to_py(frame)

    def open_stream(
        self,
        since_seq: Optional[int] = None,
        max_messages: Optional[int] = None,
        timeout_ms: Optional[int] = None,
    ) -> Stream:
        _require_open(self._ptr, "pool")
        since_seq = _optional_non_negative_int(since_seq, "since_seq")
        max_messages = _optional_non_negative_int(max_messages, "max_messages")
        timeout_ms = _optional_non_negative_int(timeout_ms, "timeout_ms")
        out_stream = POINTER(plsm_stream_t)()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_stream_open(
            self._ptr,
            c_uint64(since_seq or 0),
            c_uint32(1 if since_seq is not None else 0),
            c_uint64(max_messages or 0),
            c_uint32(1 if max_messages is not None else 0),
            c_uint64(timeout_ms or 0),
            c_uint32(1 if timeout_ms is not None else 0),
            byref(out_stream),
            byref(out_err),
        )
        if rc != 0:
            raise _take_error(out_err)
        return Stream(out_stream)

    def open_lite3_stream(
        self,
        since_seq: Optional[int] = None,
        max_messages: Optional[int] = None,
        timeout_ms: Optional[int] = None,
    ) -> Lite3Stream:
        _require_open(self._ptr, "pool")
        since_seq = _optional_non_negative_int(since_seq, "since_seq")
        max_messages = _optional_non_negative_int(max_messages, "max_messages")
        timeout_ms = _optional_non_negative_int(timeout_ms, "timeout_ms")
        out_stream = POINTER(plsm_lite3_stream_t)()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_lite3_stream_open(
            self._ptr,
            c_uint64(since_seq or 0),
            c_uint32(1 if since_seq is not None else 0),
            c_uint64(max_messages or 0),
            c_uint32(1 if max_messages is not None else 0),
            c_uint64(timeout_ms or 0),
            c_uint32(1 if timeout_ms is not None else 0),
            byref(out_stream),
            byref(out_err),
        )
        if rc != 0:
            raise _take_error(out_err)
        return Lite3Stream(out_stream)

    def replay(
        self,
        speed: float = 1.0,
        since_seq: Optional[int] = None,
        max_messages: Optional[int] = None,
        timeout_ms: Optional[int] = None,
        tags: Optional[Iterable[str]] = None,
    ) -> Generator[Message, None, None]:
        """Replay messages with original timing scaled by speed.

        Yields Message objects with inter-message delays derived from
        each message's ``time``. The first message is yielded
        immediately; subsequent messages are delayed by
        ``(current_time - prev_time) / speed``.
        """
        if speed <= 0:
            raise ValueError("speed must be positive")

        required_tags = list(tags or [])
        stream_max_messages = (
            None if (max_messages is not None and required_tags) else max_messages
        )

        stream = self.open_stream(
            since_seq=since_seq,
            max_messages=stream_max_messages,
            timeout_ms=timeout_ms,
        )
        try:
            prev_dt: Optional[datetime] = None
            delivered = 0
            filter_by_tags = bool(required_tags)
            while True:
                msg = stream.next_json()
                if msg is None:
                    break
                message = parse_message(msg)
                if filter_by_tags and not _message_has_tags(message, required_tags):
                    continue
                cur_dt = message.time
                if prev_dt is not None and cur_dt is not None:
                    delta = (cur_dt - prev_dt).total_seconds() / speed
                    if delta > 0:
                        _time.sleep(delta)
                if cur_dt is not None:
                    prev_dt = cur_dt
                delivered += 1
                yield message
                if max_messages is not None and delivered >= max_messages:
                    break
        finally:
            stream.close()

    def tail(
        self,
        since_seq: Optional[int] = None,
        max_messages: Optional[int] = None,
        timeout_ms: Optional[int] = None,
        tags: Optional[Iterable[str]] = None,
    ) -> Generator[Message, None, None]:
        """Tail JSON messages and optionally filter by exact tags."""
        required_tags = list(tags or [])
        stream_max_messages = (
            None if (max_messages is not None and required_tags) else max_messages
        )
        stream = self.open_stream(
            since_seq=since_seq,
            max_messages=stream_max_messages,
            timeout_ms=timeout_ms,
        )
        try:
            delivered = 0
            filter_by_tags = bool(required_tags)
            while True:
                msg = stream.next_json()
                if msg is None:
                    break
                message = parse_message(msg)
                if filter_by_tags:
                    if not _message_has_tags(message, required_tags):
                        continue
                delivered += 1
                yield message
                if max_messages is not None and delivered >= max_messages:
                    break
        finally:
            stream.close()

    def close(self) -> None:
        if getattr(self, "_ptr", None):
            _LIB.plsm_pool_free(self._ptr)
            self._ptr = None

    def __enter__(self) -> Pool:
        _require_open(self._ptr, "pool")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


class Stream:
    def __init__(self, ptr: POINTER(plsm_stream_t)) -> None:
        self._ptr = ptr

    def next_json(self) -> Optional[bytes]:
        _require_open(self._ptr, "stream")
        buf = plsm_buf_t()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_stream_next(self._ptr, byref(buf), byref(out_err))
        if rc == 1:
            return _buf_to_bytes(buf)
        if rc == 0:
            return None
        raise _take_error(out_err)

    def __iter__(self) -> Stream:
        return self

    def __next__(self) -> bytes:
        message = self.next_json()
        if message is None:
            raise StopIteration
        return message

    def close(self) -> None:
        if getattr(self, "_ptr", None):
            _LIB.plsm_stream_free(self._ptr)
            self._ptr = None

    def __enter__(self) -> Stream:
        _require_open(self._ptr, "stream")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


class Lite3Stream:
    def __init__(self, ptr: POINTER(plsm_lite3_stream_t)) -> None:
        self._ptr = ptr

    def next(self) -> Optional[Lite3Frame]:
        _require_open(self._ptr, "stream")
        frame = plsm_lite3_frame_t()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_lite3_stream_next(self._ptr, byref(frame), byref(out_err))
        if rc == 1:
            return _frame_to_py(frame)
        if rc == 0:
            return None
        raise _take_error(out_err)

    def __iter__(self) -> Lite3Stream:
        return self

    def __next__(self) -> Lite3Frame:
        frame = self.next()
        if frame is None:
            raise StopIteration
        return frame

    def close(self) -> None:
        if getattr(self, "_ptr", None):
            _LIB.plsm_lite3_stream_free(self._ptr)
            self._ptr = None

    def __enter__(self) -> Lite3Stream:
        _require_open(self._ptr, "stream")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


def _parse_rfc3339_utc(raw_time: str) -> datetime:
    parsed = datetime.fromisoformat(raw_time.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("message time must include timezone")
    return parsed.astimezone(timezone.utc)


def parse_message(payload: bytes) -> Message:
    raw = _ensure_bytes(payload, "payload")
    parsed = json.loads(raw.decode("utf-8"))
    if not isinstance(parsed, dict):
        raise ValueError("message payload must decode to an object")
    raw_time = parsed.get("time")
    if not isinstance(raw_time, str):
        raise ValueError("message time must be an RFC3339 string")
    meta = parsed.get("meta", {})
    tags = meta.get("tags") if isinstance(meta, dict) else None
    if not isinstance(tags, list):
        tags = []
    tag_list = [str(tag) for tag in tags]
    seq = parsed.get("seq")
    if not isinstance(seq, int):
        raise ValueError("message seq must be an int")
    return Message(
        seq=seq,
        time=_parse_rfc3339_utc(raw_time),
        time_rfc3339=raw_time,
        data=parsed.get("data"),
        meta=MessageMeta(tags=tag_list),
        raw=raw,
    )


def _message_has_tags(message: Message, required_tags: list[str]) -> bool:
    if not required_tags:
        return True
    return all(tag in message.tags for tag in required_tags)


__all__ = [
    "Client",
    "Pool",
    "Message",
    "MessageMeta",
    "Stream",
    "Lite3Frame",
    "Lite3Stream",
    "Durability",
    "ErrorKind",
    "PlasmiteError",
    "NotFoundError",
    "AlreadyExistsError",
    "BusyError",
    "PermissionDeniedError",
    "CorruptError",
    "IoError",
    "UsageError",
    "InternalError",
    "DEFAULT_POOL_DIR",
    "DEFAULT_POOL_SIZE",
    "DEFAULT_POOL_SIZE_BYTES",
    "default_pool_dir",
    "parse_message",
]
