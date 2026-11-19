"""
Purpose: Provide Python bindings for the libplasmite C ABI.
Key Exports: Client, Pool, Stream, Durability, ErrorKind, PlasmiteError.
Role: Minimal, ergonomic wrapper around include/plasmite.h for Python users.
Invariants: JSON bytes in/out; native operations and close serialize per handle.
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
    cast,
    sizeof,
    string_at,
)
from datetime import datetime, timedelta, timezone
from enum import IntEnum
from functools import wraps
from threading import Event, Lock, RLock
import json
import os
import re
import sys
from pathlib import Path
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
    RETENTION_GAP = 9


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


class RetentionGapError(PlasmiteError):
    def __init__(self, *args, missing: int | None = None, next_seq: int | None = None):
        super().__init__(*args)
        self.missing = missing
        self.next_seq = next_seq


@dataclass(frozen=True)
class MessageMeta:
    tags: list[str]


@dataclass(frozen=True)
class Message:
    seq: int
    time: int
    data: Any
    meta: MessageMeta
    raw: bytes

    @property
    def tags(self) -> list[str]:
        return self.meta.tags


@dataclass(frozen=True)
class Lite3Frame:
    seq: int
    time: int
    flags: int
    payload: bytes


class plsm_client_t(Structure):
    pass


class plsm_pool_t(Structure):
    pass


class plsm_stream_t(Structure):
    pass


class plsm_lite3_stream_t(Structure):
    pass


class plsm_udp_client_t(Structure):
    pass


class plsm_udp_pool_t(Structure):
    pass


class plsm_udp_follow_t(Structure):
    pass


class plsm_buf_t(Structure):
    _fields_ = [
        ("data", c_void_p),
        ("len", c_size_t),
    ]


class plsm_udp_event_t(Structure):
    _fields_ = [
        ("kind", c_uint32),
        ("reason", c_uint32),
        ("seq", c_uint64),
        ("time", c_uint64),
        ("first", c_uint64),
        ("last", c_uint64),
        ("start_seq", c_uint64),
        ("next_seq", c_uint64),
        ("end_seq", c_uint64),
        ("has_end", c_uint8),
        ("terminal", c_uint8),
        ("reserved", c_uint8),
        ("payload", plsm_buf_t),
    ]


class plsm_feed_receipt_t(Structure):
    _fields_ = [("seq", c_uint64), ("time", c_uint64)]


class plsm_lite3_frame_t(Structure):
    _fields_ = [
        ("seq", c_uint64),
        ("time", c_uint64),
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
        ("missing", c_uint64),
        ("next_seq", c_uint64),
    ]


class plsm_text_t(Structure):
    _fields_ = [("data", c_char_p), ("len", c_size_t)]


class plsm_read_options_t(Structure):
    _fields_ = [
        ("struct_size", c_uint32),
        ("start_kind", c_uint32),
        ("start_value", c_uint64),
        ("tags", POINTER(plsm_text_t)),
        ("tags_len", c_size_t),
    ]


class plsm_feed_options_t(Structure):
    _fields_ = [
        ("struct_size", c_uint32),
        ("durability", c_uint32),
        ("tags", POINTER(plsm_text_t)),
        ("tags_len", c_size_t),
    ]


def _load_lib() -> CDLL:
    if sys.platform == "win32":
        versioned = "plasmite2.dll"
    elif sys.platform == "darwin":
        versioned = "libplasmite.2.dylib"
    else:
        versioned = "libplasmite.so.2"
    env_dir = os.environ.get("PLASMITE_LIB_DIR")
    if env_dir:
        candidate = Path(env_dir).resolve() / versioned
        if candidate.is_file():
            return _check_abi(CDLL(str(candidate)))
        raise OSError(f"Plasmite 2 library not found in {env_dir}")
    candidate = Path(__file__).resolve().parent / "_native" / versioned
    if candidate.is_file():
        return _check_abi(CDLL(str(candidate)))
    return _check_abi(CDLL(versioned))


def _check_abi(lib: CDLL) -> CDLL:
    try:
        major = lib.plsm_abi_major
    except AttributeError as exc:
        raise ImportError("Plasmite requires ABI major 2; library does not report its ABI") from exc
    major.argtypes = []
    major.restype = c_uint32
    actual = major()
    if actual != 2:
        raise ImportError(f"Plasmite requires ABI major 2; found {actual}")
    return lib


_LIB = _load_lib()

DEFAULT_POOL_SIZE_BYTES = 1024 * 1024
_UINT64_MAX = (1 << 64) - 1


def default_pool_dir() -> str:
    if os.name == "nt":
        home = os.environ.get("USERPROFILE", "")
        if not os.path.isabs(home):
            import ctypes
            import uuid
            profile_id = ctypes.create_string_buffer(uuid.UUID("5e6c858f-0e22-4760-9afe-ea3317b67173").bytes_le)
            directory = ctypes.c_void_p()
            shell = ctypes.WinDLL("shell32")
            shell.SHGetKnownFolderPath.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)]
            shell.SHGetKnownFolderPath.restype = ctypes.c_long
            status = shell.SHGetKnownFolderPath(profile_id, 0, None, ctypes.byref(directory))
            home = ""
            if status >= 0 and directory.value:
                try:
                    home = ctypes.wstring_at(directory)
                finally:
                    ole = ctypes.WinDLL("ole32")
                    ole.CoTaskMemFree.argtypes = [ctypes.c_void_p]
                    ole.CoTaskMemFree(directory)
    else:
        home = os.environ.get("HOME", "")
        if not os.path.isabs(home):
            import pwd
            try:
                home = pwd.getpwuid(os.getuid()).pw_dir
            except (KeyError, OSError):
                home = ""
    if not home or not os.path.isabs(home):
        raise ValueError("cannot determine the user home directory; supply an explicit pool directory")
    return os.path.join(home, ".plasmite", "pools")


try:
    DEFAULT_POOL_DIR = default_pool_dir()
except ValueError:
    DEFAULT_POOL_DIR = None

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
    POINTER(plsm_client_t), c_char_p, c_uint32,
    POINTER(POINTER(plsm_pool_t)), POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_pool_open.restype = c_int
_LIB.plsm_pool_feed_json.argtypes = [
    POINTER(plsm_pool_t), c_char_p, c_size_t, POINTER(plsm_feed_options_t),
    POINTER(plsm_buf_t), POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_pool_feed_json.restype = c_int
for _open_reader in (
    _LIB.plsm_pool_follow, _LIB.plsm_pool_fetch,
    _LIB.plsm_pool_follow, _LIB.plsm_pool_fetch,
):
    _open_reader.argtypes = [
        POINTER(plsm_pool_t), POINTER(plsm_read_options_t),
        POINTER(POINTER(plsm_stream_t)), POINTER(POINTER(plsm_error_t)),
    ]
    _open_reader.restype = c_int

_LIB.plsm_pool_info.argtypes = [POINTER(plsm_pool_t), POINTER(plsm_buf_t), POINTER(POINTER(plsm_error_t))]
_LIB.plsm_pool_info.restype = c_int
_LIB.plsm_pool_list.argtypes = [POINTER(plsm_client_t), POINTER(plsm_buf_t), POINTER(POINTER(plsm_error_t))]
_LIB.plsm_pool_list.restype = c_int
_LIB.plsm_pool_delete.argtypes = [POINTER(plsm_client_t), c_char_p, POINTER(POINTER(plsm_error_t))]
_LIB.plsm_pool_delete.restype = c_int

_LIB.plsm_pool_free.argtypes = [POINTER(plsm_pool_t)]
_LIB.plsm_pool_free.restype = None


_LIB.plsm_pool_feed_lite3.argtypes = _LIB.plsm_pool_feed_json.argtypes
_LIB.plsm_pool_feed_lite3.restype = c_int

for _name in ("plsm_pool_feed_json_receipt", "plsm_pool_feed_lite3_receipt"):
    _function = getattr(_LIB, _name)
    _function.argtypes = [
        POINTER(plsm_pool_t), c_char_p, c_size_t, POINTER(plsm_feed_options_t),
        POINTER(plsm_feed_receipt_t), POINTER(POINTER(plsm_error_t)),
    ]
    _function.restype = c_int



for _name in ("plsm_pool_follow_lite3", "plsm_pool_fetch_lite3"):
    _function = getattr(_LIB, _name)
    _function.argtypes = [
        POINTER(plsm_pool_t), POINTER(plsm_read_options_t),
        POINTER(POINTER(plsm_lite3_stream_t)), POINTER(POINTER(plsm_error_t)),
    ]
    _function.restype = c_int


_LIB.plsm_stream_next_wait.argtypes = [POINTER(plsm_stream_t), c_uint64, POINTER(plsm_buf_t), POINTER(POINTER(plsm_error_t))]
_LIB.plsm_stream_next_wait.restype = c_int
_LIB.plsm_stream_cancel.argtypes = [POINTER(plsm_stream_t)]
_LIB.plsm_stream_cancel.restype = None
_LIB.plsm_lite3_stream_next_wait.argtypes = [POINTER(plsm_lite3_stream_t), c_uint64, POINTER(plsm_lite3_frame_t), POINTER(POINTER(plsm_error_t))]
_LIB.plsm_lite3_stream_next_wait.restype = c_int
_LIB.plsm_lite3_stream_cancel.argtypes = [POINTER(plsm_lite3_stream_t)]
_LIB.plsm_lite3_stream_cancel.restype = None



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

_LIB.plsm_udp_client_new.argtypes = [
    c_char_p,
    POINTER(POINTER(plsm_udp_client_t)),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_udp_client_new.restype = c_int
_LIB.plsm_udp_client_free.argtypes = [POINTER(plsm_udp_client_t)]
_LIB.plsm_udp_client_free.restype = None
_LIB.plsm_udp_client_open_pool.argtypes = [
    POINTER(plsm_udp_client_t),
    c_char_p,
    POINTER(POINTER(plsm_udp_pool_t)),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_udp_client_open_pool.restype = c_int
_LIB.plsm_udp_pool_free.argtypes = [POINTER(plsm_udp_pool_t)]
_LIB.plsm_udp_pool_free.restype = None
_LIB.plsm_udp_pool_feed_lite3.argtypes = [
    POINTER(plsm_udp_pool_t),
    c_char_p,
    c_size_t,
    POINTER(plsm_feed_options_t),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_udp_pool_feed_lite3.restype = c_int
_LIB.plsm_udp_pool_read.argtypes = [
    POINTER(plsm_udp_pool_t), POINTER(plsm_read_options_t), c_uint32, c_uint32,
    POINTER(POINTER(plsm_udp_follow_t)), POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_udp_pool_read.restype = c_int
_LIB.plsm_udp_pool_feed_json.argtypes = [
    POINTER(plsm_udp_pool_t), c_char_p, c_size_t, POINTER(plsm_feed_options_t), POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_udp_pool_feed_json.restype = c_int
_LIB.plsm_udp_follow_next_view.argtypes = [
    POINTER(plsm_udp_follow_t), c_uint64, POINTER(plsm_udp_event_t),
    POINTER(c_void_p), POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_udp_follow_next_view.restype = c_int
_LIB.plsm_udp_follow_next.argtypes = [
    POINTER(plsm_udp_follow_t),
    c_uint64,
    POINTER(plsm_udp_event_t),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_udp_follow_next.restype = c_int
_LIB.plsm_udp_follow_cancel.argtypes = [POINTER(plsm_udp_follow_t)]
_LIB.plsm_udp_follow_cancel.restype = None
_LIB.plsm_udp_follow_close.argtypes = [POINTER(plsm_udp_follow_t)]
_LIB.plsm_udp_follow_close.restype = None
_LIB.plsm_udp_event_free.argtypes = [POINTER(plsm_udp_event_t)]
_LIB.plsm_udp_event_free.restype = None
_LIB.plsm_udp_event_message.argtypes = [
    POINTER(plsm_udp_event_t),
    POINTER(plsm_buf_t),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_udp_event_message.restype = c_int

_ERROR_KIND_TO_CLASS: dict[ErrorKind, type[PlasmiteError]] = {
    ErrorKind.INTERNAL: InternalError,
    ErrorKind.USAGE: UsageError,
    ErrorKind.NOT_FOUND: NotFoundError,
    ErrorKind.ALREADY_EXISTS: AlreadyExistsError,
    ErrorKind.BUSY: BusyError,
    ErrorKind.PERMISSION: PermissionDeniedError,
    ErrorKind.CORRUPT: CorruptError,
    ErrorKind.IO: IoError,
    ErrorKind.RETENTION_GAP: RetentionGapError,
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
    missing = int(err.missing) or None
    next_seq = int(err.next_seq) or None
    _LIB.plsm_error_free(err_ptr)
    try:
        kind = ErrorKind(raw_kind)
    except ValueError:
        kind = ErrorKind.INTERNAL
    if not message:
        message = _default_error_message(kind)
    if kind == ErrorKind.RETENTION_GAP:
        return RetentionGapError(kind, message, path, seq, offset, missing=missing, next_seq=next_seq)
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
        ErrorKind.RETENTION_GAP: "retention gap",
    }
    return mapping.get(kind, "error")


def _buf_to_bytes(buf: plsm_buf_t) -> bytes:
    if not buf.data or buf.len == 0:
        _LIB.plsm_buf_free(byref(buf))
        return b""
    out = string_at(buf.data, buf.len)
    _LIB.plsm_buf_free(byref(buf))
    return out


def _pool_info(inspect, handle):
    buf = plsm_buf_t()
    error = POINTER(plsm_error_t)()
    if inspect(handle, byref(buf), byref(error)) != 0:
        raise _take_error(error)
    return json.loads(_buf_to_bytes(buf))


def _frame_to_py(frame: plsm_lite3_frame_t) -> Lite3Frame:
    seq = int(frame.seq)
    timestamp_ns = int(frame.time)
    flags = int(frame.flags)
    payload = b""
    if frame.payload.data and frame.payload.len:
        payload = string_at(frame.payload.data, frame.payload.len)
    _LIB.plsm_lite3_frame_free(byref(frame))
    return Lite3Frame(seq=seq, time=timestamp_ns, flags=flags, payload=payload)


def _tag_array(values: Iterable[str]):
    encoded = [_ensure_bytes(value.encode("utf-8"), "tag") for value in values]
    if not encoded:
        return POINTER(plsm_text_t)(), encoded
    array = (plsm_text_t * len(encoded))(*(plsm_text_t(value, len(value)) for value in encoded))
    return array, encoded


def _closed_error(target: str) -> PlasmiteError:
    return UsageError(ErrorKind.USAGE, f"plasmite: {target} is closed")


def _require_open(ptr: object, target: str) -> None:
    if not ptr:
        raise _closed_error(target)


def _serialized_handle(method):
    """Keep handle checks, native calls, and owned-result cleanup together."""
    @wraps(method)
    def locked(self, *args, **kwargs):
        with self._lock:
            return method(self, *args, **kwargs)

    return locked


def _ensure_bytes(payload: bytes | bytearray | memoryview, name: str) -> bytes:
    if isinstance(payload, bytes):
        return payload if type(payload) is bytes else bytes.__getitem__(payload, slice(None))
    if isinstance(payload, (bytearray, memoryview)):
        encoded = bytes(payload)
        return encoded if type(encoded) is bytes else bytes.__getitem__(encoded, slice(None))
    raise TypeError(f"{name} must be bytes-like")


def _encode_c_string(value: str, name: str) -> bytes:
    encoded = _ensure_bytes(value.encode("utf-8"), name)
    if b"\0" in encoded:
        raise ValueError(f"{name} must not contain NUL")
    return encoded


def _ensure_non_negative_int(value: int, name: str) -> int:
    if not isinstance(value, int):
        raise TypeError(f"{name} must be an int")
    if value < 0:
        raise ValueError(f"{name} must be >= 0")
    if value > _UINT64_MAX:
        raise ValueError(f"{name} must fit an unsigned 64-bit integer")
    return value


def _optional_non_negative_int(value: Optional[int], name: str) -> Optional[int]:
    if value is None:
        return None
    return _ensure_non_negative_int(value, name)


@dataclass(frozen=True)
class _Instant:
    nanoseconds: int


def at(nanoseconds: int) -> _Instant:
    """Select a Unix nanosecond instant instead of a sequence."""
    return _Instant(_ensure_non_negative_int(nanoseconds, "time"))


_DURATION_UNITS = dict(ns=1, us=1000, ms=1000000, s=10**9, m=60*10**9,
                       h=3600*10**9, d=86400*10**9)


def _timedelta_ns(value: timedelta) -> int:
    return ((value.days * 86400 + value.seconds) * 10**6 + value.microseconds) * 1000


def _wait_deadline(wait):
    from math import isfinite
    from time import monotonic_ns

    if wait is None:
        return None
    if isinstance(wait, timedelta):
        nanos = _timedelta_ns(wait)
    elif type(wait) is int or type(wait) is float and isfinite(wait * 10**9):
        nanos = int(wait * 10**9)
    else:
        raise UsageError(ErrorKind.USAGE, "wait must be a duration in seconds or a timedelta")
    if nanos < 0:
        raise UsageError(ErrorKind.USAGE, "wait must be non-negative")
    return monotonic_ns() + nanos


def _wait_ms(deadline, cancel):
    from time import monotonic_ns

    limit = 50 if cancel is not None else _UINT64_MAX
    if deadline is None:
        return limit
    return min(limit, max(0, (deadline - monotonic_ns() + 999_999) // 1_000_000))


def _parse_time_ns(text: str) -> int:
    pattern = r"(\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2})(?:\.(\d+))?([Zz]|[+-]\d{2}:\d{2})"
    parsed = re.fullmatch(pattern, text.strip(), re.ASCII)
    if parsed is None:
        raise ValueError("expected an RFC 3339 timestamp")
    seconds, fraction, zone = parsed.groups()
    whole = datetime.fromisoformat(seconds + ("+00:00" if zone.upper() == "Z" else zone))
    epoch = datetime(1970, 1, 1, tzinfo=timezone.utc)
    return _timedelta_ns(whole - epoch) + int((fraction or "")[:9].ljust(9, "0"))


def _read_start(since, last) -> tuple[int, int]:
    try:
        if last is not None:
            if since is not None:
                raise ValueError("since and last are mutually exclusive")
            return 5, _ensure_non_negative_int(last, "last")
        if since is None:
            return 0, 0
        if isinstance(since, _Instant):
            return 3, since.nanoseconds
        if isinstance(since, timedelta):
            return 4, _ensure_non_negative_int(_timedelta_ns(since), "since")
        if isinstance(since, int):
            return 2, _ensure_non_negative_int(since, "since")
        if isinstance(since, str):
            if since == "oldest":
                return 1, 0
            if duration := re.fullmatch(r"([0-9]+)(ns|us|ms|s|m|h|d)", since.strip(), re.I):
                value = int(duration[1]) * _DURATION_UNITS[duration[2].lower()]
                return 4, min(_UINT64_MAX, value)
            return 3, max(0, min(_UINT64_MAX, _parse_time_ns(since)))
        raise ValueError("since must be oldest, a sequence, an instant or a duration")
    except ValueError as exc:
        raise UsageError(ErrorKind.USAGE, str(exc)) from exc


class Client:
    def __init__(self, pool_dir: str | None = None) -> None:
        self._lock = RLock()
        self._ptr = None
        if pool_dir is None:
            pool_dir = default_pool_dir()
        if not pool_dir:
            raise ValueError("pool_dir is required")
        out_client = POINTER(plsm_client_t)()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_client_new(_encode_c_string(pool_dir, "pool_dir"), byref(out_client), byref(out_err))
        if rc != 0:
            raise _take_error(out_err)
        self._ptr = out_client

    @_serialized_handle
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
        encoded_ref = _encode_c_string(pool_ref, "pool_ref")
        native_size = c_uint64(size_bytes)
        _require_open(self._ptr, "client")
        rc = _LIB.plsm_pool_create(
            self._ptr,
            encoded_ref,
            native_size,
            byref(out_pool),
            byref(out_err),
        )
        if rc != 0:
            raise _take_error(out_err)
        return Pool(out_pool)

    @_serialized_handle
    def open_pool(self, pool_ref: str, *, create: bool = False) -> Pool:
        _require_open(self._ptr, "client")
        if not pool_ref:
            raise ValueError("pool_ref is required")
        out_pool = POINTER(plsm_pool_t)()
        out_err = POINTER(plsm_error_t)()
        encoded_ref = _encode_c_string(pool_ref, "pool_ref")
        create_flag = int(create)
        _require_open(self._ptr, "client")
        rc = _LIB.plsm_pool_open(
            self._ptr,
            encoded_ref,
            create_flag,
            byref(out_pool),
            byref(out_err),
        )
        if rc != 0:
            raise _take_error(out_err)
        return Pool(out_pool)

    @_serialized_handle
    def list_pools(self) -> list[dict]:
        _require_open(self._ptr, "client")
        return _pool_info(_LIB.plsm_pool_list, self._ptr)

    @_serialized_handle
    def delete_pool(self, pool_ref: str) -> None:
        _require_open(self._ptr, "client")
        error = POINTER(plsm_error_t)()
        encoded = _encode_c_string(pool_ref, "pool_ref")
        _require_open(self._ptr, "client")
        if _LIB.plsm_pool_delete(self._ptr, encoded, byref(error)) != 0:
            raise _take_error(error)

    @_serialized_handle
    def close(self) -> None:
        if getattr(self, "_ptr", None):
            _LIB.plsm_client_free(self._ptr)
            self._ptr = None

    def __enter__(self) -> Client:
        with self._lock:
            _require_open(self._ptr, "client")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


@dataclass(frozen=True)
class FeedReceipt:
    """The committed sequence and Unix nanosecond time."""
    seq: int
    time: int


class Pool:
    def __init__(self, ptr: POINTER(plsm_pool_t)) -> None:
        self._lock = RLock()
        self._ptr = ptr

    @_serialized_handle
    def info(self) -> dict:
        _require_open(self._ptr, "pool")
        return _pool_info(_LIB.plsm_pool_info, self._ptr)

    def feed(
        self, value: dict | Message, tags: Iterable[str] | None = None,
        durability: Durability = Durability.FAST,
    ) -> Message:
        """Store an object or forward a message with a new sequence and time."""
        from ._views import MessageView
        if isinstance(value, MessageView):
            if tags is not None:
                raise UsageError(ErrorKind.USAGE, "cannot supply tags when forwarding a message")
            return self.feed_lite3(memoryview(value), durability)
        if isinstance(value, Message):
            if tags is not None:
                raise UsageError(ErrorKind.USAGE, "cannot supply tags when forwarding a message")
            tags = value.tags
            value = value.data
        payload = json.dumps(value).encode("utf-8")
        return parse_message(self.feed_json(payload, tags, durability))

    def feed_json(
        self, payload: bytes, tags: Iterable[str] | None = None,
        durability: Durability = Durability.FAST,
    ) -> bytes:
        buf = self._feed_encoded(_LIB.plsm_pool_feed_json, payload, tags, durability, plsm_buf_t())
        return _buf_to_bytes(buf)

    @_serialized_handle
    def _feed_encoded(self, store, payload, tags, durability, out):
        _require_open(self._ptr, "pool")
        payload = _ensure_bytes(payload, "payload")
        tag_array, encoded_tags = _tag_array([] if tags is None else tags)
        options = plsm_feed_options_t(sizeof(plsm_feed_options_t), int(durability),
                                      tag_array, len(encoded_tags))
        error = POINTER(plsm_error_t)()
        _require_open(self._ptr, "pool")
        if store(self._ptr, payload, len(payload), byref(options), byref(out), byref(error)) != 0:
            raise _take_error(error)
        return out

    def feed_lite3(self, payload: bytes, durability: Durability = Durability.FAST) -> Message:
        buf = self._feed_encoded(_LIB.plsm_pool_feed_lite3, payload, None, durability, plsm_buf_t())
        return parse_message(_buf_to_bytes(buf))

    def feed_receipt(
        self, value, tags: Iterable[str] | None = None,
        durability: Durability = Durability.FAST,
    ) -> FeedReceipt:
        from ._views import MessageView
        if isinstance(value, MessageView):
            if tags is not None:
                raise UsageError(ErrorKind.USAGE, "cannot supply tags when forwarding a message")
            return self.feed_lite3_receipt(memoryview(value), durability)
        if isinstance(value, Message):
            if tags is not None:
                raise UsageError(ErrorKind.USAGE, "cannot supply tags when forwarding a message")
            tags, value = value.tags, value.data
        return self.feed_json_receipt(json.dumps(value).encode("utf-8"), tags, durability)

    def feed_json_receipt(
        self, payload: bytes, tags: Iterable[str] | None = None,
        durability: Durability = Durability.FAST,
    ) -> FeedReceipt:
        receipt = self._feed_encoded(_LIB.plsm_pool_feed_json_receipt, payload, tags,
                                     durability, plsm_feed_receipt_t())
        return FeedReceipt(int(receipt.seq), int(receipt.time))

    def feed_lite3_receipt(self, payload: bytes, durability: Durability = Durability.FAST) -> FeedReceipt:
        receipt = self._feed_encoded(_LIB.plsm_pool_feed_lite3_receipt, payload, None,
                                     durability, plsm_feed_receipt_t())
        return FeedReceipt(int(receipt.seq), int(receipt.time))

    @_serialized_handle
    def _get_json(self, seq: int) -> bytes:
        stream = self._open_stream(_LIB.plsm_pool_fetch, seq, None, None)
        try:
            return _required_record(stream.next_json(), seq)
        finally:
            stream.close()

    @_serialized_handle
    def feed_view(self, value, tags=None, durability=Durability.FAST):
        """Store an object and return an immutable view of the committed message."""
        _require_open(self._ptr, "pool")
        from ._views import MessageView, append_view

        if isinstance(value, Message):
            if tags is not None:
                raise UsageError(ErrorKind.USAGE, "cannot supply tags when forwarding a message")
            tags, value = value.tags, value.data
        elif isinstance(value, MessageView) and tags is not None:
            raise UsageError(ErrorKind.USAGE, "cannot supply tags when forwarding a message")
        return append_view(self, value, tags, durability)

    def _get_view(self, seq: int):
        with self.fetch_view(since=seq) as stream:
            return _required_record(next(stream, None), seq)

    def _get_lite3(self, seq: int) -> Lite3Frame:
        with self.fetch_lite3(since=seq) as reader:
            return _required_record(reader.next(), seq)

    @_serialized_handle
    def _open_stream(self, open_native, since, last, tags, *, lite3=False):
        _require_open(self._ptr, "pool")
        kind, value = _read_start(since, last)
        tag_array, encoded_tags = _tag_array([] if tags is None else tags)
        options = plsm_read_options_t(sizeof(plsm_read_options_t), kind, value,
                                      tag_array, len(encoded_tags))
        handle, stream = (plsm_lite3_stream_t, Lite3Stream) if lite3 else (plsm_stream_t, Stream)
        native = POINTER(handle)()
        error = POINTER(plsm_error_t)()
        _require_open(self._ptr, "pool")
        if open_native(self._ptr, byref(options), byref(native), byref(error)) != 0:
            raise _take_error(error)
        return stream(native)

    def fetch(self, *, since=None, last=None, tags=None, cancel=None) -> Reader:
        """Read up to the last sequence present when this call opens the reader."""
        return Reader(self._open_stream(_LIB.plsm_pool_fetch, since, last, tags), cancel)

    def fetch_lite3(self, *, since=None, last=None, tags=None, cancel=None) -> Reader:
        return Reader(self._open_stream(_LIB.plsm_pool_fetch_lite3, since, last, tags, lite3=True), cancel)

    def follow_lite3(self, *, since=None, last=None, tags=None, cancel=None) -> Reader:
        return Reader(self._open_stream(_LIB.plsm_pool_follow_lite3, since, last, tags, lite3=True), cancel)

    def fetch_view(self, *, since=None, last=None, tags=None, cancel=None):
        from ._views import ViewStream
        return ViewStream(self._open_stream(_LIB.plsm_pool_fetch, since, last, tags), cancel)

    def follow_view(self, *, since=None, last=None, tags=None, cancel=None):
        from ._views import ViewStream
        return ViewStream(self._open_stream(_LIB.plsm_pool_follow, since, last, tags), cancel)

    def follow(self, *, since=None, last=None, tags=None, cancel=None) -> Reader:
        """Open a live reader now. With no selection, read new messages."""
        return Reader(self._open_stream(_LIB.plsm_pool_follow, since, last, tags), cancel)

    @_serialized_handle
    def close(self) -> None:
        if getattr(self, "_ptr", None):
            _LIB.plsm_pool_free(self._ptr)
            self._ptr = None

    def __enter__(self) -> Pool:
        with self._lock:
            _require_open(self._ptr, "pool")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


def _required_record(value, seq):
    if value is None:
        raise NotFoundError(ErrorKind.NOT_FOUND, "message not found", seq=seq)
    return value


class Stream:
    def __init__(self, ptr: POINTER(plsm_stream_t)) -> None:
        self._lock = RLock()
        self._state_lock = RLock()
        self._ptr = ptr

    @_serialized_handle
    def next_json(self) -> Optional[bytes]:
        _require_open(self._ptr, "stream")
        buf = plsm_buf_t()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_stream_next_wait(self._ptr, (1 << 64) - 1, byref(buf), byref(out_err))
        if rc == 1:
            return _buf_to_bytes(buf)
        if rc in (2, 3):
            return None
        raise _take_error(out_err)

    @_serialized_handle
    def next_wait(self, timeout_ms: int) -> tuple[int, bytes | None]:
        """Return (status, value): 0=wait expired, 1=value, 2=finished, 3=cancelled."""
        _require_open(self._ptr, "stream")
        timeout_ms = c_uint64(_ensure_non_negative_int(timeout_ms, "timeout_ms"))
        _require_open(self._ptr, "stream")
        value = plsm_buf_t()
        out_err = POINTER(plsm_error_t)()
        status = _LIB.plsm_stream_next_wait(self._ptr, timeout_ms, byref(value), byref(out_err))
        if status < 0:
            raise _take_error(out_err)
        return status, _buf_to_bytes(value) if status == 1 else None

    def cancel(self) -> None:
        """Permanently cancel a read, including one running on another thread."""
        with self._state_lock:
            if self._ptr:
                _LIB.plsm_stream_cancel(self._ptr)

    def __iter__(self) -> Stream:
        return self

    def __next__(self) -> bytes:
        message = self.next_json()
        if message is None:
            raise StopIteration
        return message

    def close(self) -> None:
        if not hasattr(self, "_state_lock"):
            return
        self.cancel()
        with self._lock:
            with self._state_lock:
                if self._ptr:
                    _LIB.plsm_stream_free(self._ptr)
                    self._ptr = None

    def __enter__(self) -> Stream:
        with self._lock:
            _require_open(self._ptr, "stream")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


class Reader:
    """A reader opened by fetch or follow. Close it to stop work from another thread."""

    def __init__(self, stream: Stream | Lite3Stream, cancel: Event | None) -> None:
        self._stream = stream
        self._cancel = cancel
        self._decode = parse_message if isinstance(stream, Stream) else lambda frame: frame
        self._closed = False

    def __iter__(self) -> Reader:
        return self

    def __next__(self) -> Message | Lite3Frame:
        return self.next()

    def next(self, wait: timedelta | float | None = None) -> Message | Lite3Frame | None:
        """Return a message, or None if the wait in seconds or timedelta expires."""
        from time import monotonic_ns

        deadline = _wait_deadline(wait)
        while not self._closed and (self._cancel is None or not self._cancel.is_set()):
            try:
                status, payload = self._stream.next_wait(_wait_ms(deadline, self._cancel))
            except UsageError:
                if self._closed:
                    raise StopIteration from None
                raise
            if status == 1:
                return self._decode(payload)
            if status in (2, 3):
                break
            if deadline is not None and monotonic_ns() >= deadline:
                return None
        self.close()
        raise StopIteration

    def close(self) -> None:
        self._closed = True
        self._stream.close()

    def __enter__(self) -> Reader:
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()


class Lite3Stream:
    def __init__(self, ptr: POINTER(plsm_lite3_stream_t)) -> None:
        self._lock = RLock()
        self._state_lock = RLock()
        self._ptr = ptr

    @_serialized_handle
    def next(self) -> Optional[Lite3Frame]:
        _require_open(self._ptr, "stream")
        frame = plsm_lite3_frame_t()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_lite3_stream_next_wait(self._ptr, (1 << 64) - 1, byref(frame), byref(out_err))
        if rc == 1:
            return _frame_to_py(frame)
        if rc in (2, 3):
            return None
        raise _take_error(out_err)

    @_serialized_handle
    def next_wait(self, timeout_ms: int) -> tuple[int, Lite3Frame | None]:
        """Return (status, value): 0=wait expired, 1=value, 2=finished, 3=cancelled."""
        _require_open(self._ptr, "stream")
        timeout_ms = c_uint64(_ensure_non_negative_int(timeout_ms, "timeout_ms"))
        _require_open(self._ptr, "stream")
        value = plsm_lite3_frame_t()
        out_err = POINTER(plsm_error_t)()
        status = _LIB.plsm_lite3_stream_next_wait(self._ptr, timeout_ms, byref(value), byref(out_err))
        if status < 0:
            raise _take_error(out_err)
        return status, _frame_to_py(value) if status == 1 else None

    def cancel(self) -> None:
        """Permanently cancel a read, including one running on another thread."""
        with self._state_lock:
            if self._ptr:
                _LIB.plsm_lite3_stream_cancel(self._ptr)

    def __iter__(self) -> Lite3Stream:
        return self

    def __next__(self) -> Lite3Frame:
        frame = self.next()
        if frame is None:
            raise StopIteration
        return frame

    def close(self) -> None:
        if not hasattr(self, "_state_lock"):
            return
        self.cancel()
        with self._lock:
            with self._state_lock:
                if self._ptr:
                    _LIB.plsm_lite3_stream_free(self._ptr)
                    self._ptr = None

    def __enter__(self) -> Lite3Stream:
        with self._lock:
            _require_open(self._ptr, "stream")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


@dataclass(frozen=True)
class UdpFrame:
    seq: int
    time: int
    payload: bytes

    def message(self) -> Message:
        payload = _ensure_bytes(self.payload, "payload")
        event = plsm_udp_event_t()
        event.kind = 1
        event.seq = self.seq
        event.time = self.time
        # Keep the immutable snapshot alive through the GIL-releasing CDLL call.
        event.payload.data = cast(c_char_p(payload), c_void_p) if payload else None
        event.payload.len = len(payload)
        out_message = plsm_buf_t()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_udp_event_message(byref(event), byref(out_message), byref(out_err))
        if rc != 0:
            raise _take_error(out_err)
        return parse_message(_buf_to_bytes(out_message))


@dataclass(frozen=True)
class UdpSkip:
    """A range the server explicitly skipped."""

    first: int
    last: int
    reason: str


@dataclass(frozen=True)
class UdpProgress:
    start_seq: int
    next_seq: int
    end_seq: int | None
    terminal: bool


UdpEvent = UdpFrame | UdpSkip | UdpProgress


class UdpClient:
    """Client for one explicit UDP endpoint."""

    def __init__(self, endpoint: str) -> None:
        self._lock = RLock()
        self._ptr = None
        if not endpoint:
            raise ValueError("endpoint is required")
        out_client = POINTER(plsm_udp_client_t)()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_udp_client_new(_encode_c_string(endpoint, "endpoint"), byref(out_client), byref(out_err))
        if rc != 0:
            raise _take_error(out_err)
        self._ptr = out_client

    @_serialized_handle
    def open_pool(self, pool_name: str) -> UdpPool:
        _require_open(self._ptr, "UDP client")
        if not pool_name:
            raise ValueError("pool_name is required")
        out_pool = POINTER(plsm_udp_pool_t)()
        out_err = POINTER(plsm_error_t)()
        encoded_name = _encode_c_string(pool_name, "pool_name")
        _require_open(self._ptr, "UDP client")
        rc = _LIB.plsm_udp_client_open_pool(
            self._ptr,
            encoded_name,
            byref(out_pool),
            byref(out_err),
        )
        if rc != 0:
            raise _take_error(out_err)
        return UdpPool(out_pool)

    @_serialized_handle
    def close(self) -> None:
        if self._ptr:
            _LIB.plsm_udp_client_free(self._ptr)
            self._ptr = None

    def __enter__(self) -> UdpClient:
        with self._lock:
            _require_open(self._ptr, "UDP client")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


class UdpPool:
    """A named pool on one UDP endpoint."""

    def __init__(self, ptr: POINTER(plsm_udp_pool_t)) -> None:
        self._lock = RLock()
        self._ptr = ptr

    @_serialized_handle
    def feed_lite3(self, payload: bytes | bytearray | memoryview, durability=Durability.FAST) -> None:
        _require_open(self._ptr, "UDP pool")
        payload = _ensure_bytes(payload, "payload")
        if not payload:
            raise ValueError("payload is required")
        out_err = POINTER(plsm_error_t)()
        options = plsm_feed_options_t(sizeof(plsm_feed_options_t), int(durability))
        _require_open(self._ptr, "UDP pool")
        rc = _LIB.plsm_udp_pool_feed_lite3(
            self._ptr,
            payload,
            c_size_t(len(payload)),
            byref(options),
            byref(out_err),
        )
        if rc != 0:
            raise _take_error(out_err)

    @_serialized_handle
    def feed(self, value, tags=None, durability=Durability.FAST) -> None:
        _require_open(self._ptr, "UDP pool")
        from ._views import MessageView
        if isinstance(value, MessageView):
            if tags is not None:
                raise UsageError(ErrorKind.USAGE, "cannot supply tags when forwarding a message")
            return self.feed_lite3(memoryview(value), durability)
        if isinstance(value, Message):
            if tags is not None:
                raise UsageError(ErrorKind.USAGE, "cannot supply tags when forwarding a message")
            tags, value = value.tags, value.data
        payload = json.dumps(value).encode("utf-8")
        encoded, values = _tag_array([] if tags is None else tags)
        options = plsm_feed_options_t(sizeof(plsm_feed_options_t), int(durability), encoded, len(values))
        error = POINTER(plsm_error_t)()
        _require_open(self._ptr, "UDP pool")
        if _LIB.plsm_udp_pool_feed_json(self._ptr, payload, len(payload), byref(options), byref(error)) != 0:
            raise _take_error(error)

    def follow(self, *, since=None, last=None, tags=None, cancel=None) -> UdpFollow:
        return self._read(True, False, since, last, tags, cancel)

    def fetch(self, *, since=None, last=None, tags=None, cancel=None) -> UdpFollow:
        return self._read(False, False, since, last, tags, cancel)

    def follow_view(self, *, since=None, last=None, tags=None, cancel=None) -> UdpFollow:
        return self._read(True, True, since, last, tags, cancel)

    def fetch_view(self, *, since=None, last=None, tags=None, cancel=None) -> UdpFollow:
        return self._read(False, True, since, last, tags, cancel)

    @_serialized_handle
    def _read(self, live, views, since, last, tags, cancel) -> UdpFollow:
        _require_open(self._ptr, "UDP pool")
        kind, value = _read_start(since, last)
        encoded, values = _tag_array([] if tags is None else tags)
        options = plsm_read_options_t(sizeof(plsm_read_options_t), kind, value, encoded, len(values))
        native = POINTER(plsm_udp_follow_t)()
        error = POINTER(plsm_error_t)()
        _require_open(self._ptr, "UDP pool")
        if _LIB.plsm_udp_pool_read(self._ptr, byref(options), live, views, byref(native), byref(error)) != 0:
            raise _take_error(error)
        return UdpFollow(native, views=views, cancel=cancel)

    @_serialized_handle
    def close(self) -> None:
        if self._ptr:
            _LIB.plsm_udp_pool_free(self._ptr)
            self._ptr = None

    def __enter__(self) -> UdpPool:
        with self._lock:
            _require_open(self._ptr, "UDP pool")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


class UdpFollow:
    """Best-effort UDP event stream. Close cancels a blocked wait."""

    def __init__(self, ptr: POINTER(plsm_udp_follow_t), *, views=False, cancel=None) -> None:
        self._state_lock = RLock()
        self._read_lock = Lock()
        self._ptr = ptr
        self._closing = False
        self._cancelled = False
        self._views = views
        self._cancel = cancel
        self._finished = False

    def next_event(self, wait: timedelta | float | None = None) -> UdpEvent | MessageView | None:
        from time import monotonic_ns

        deadline = _wait_deadline(wait)
        if self._views:
            from ._views import backend
            adopt_owner = backend.adopt_owner
        with self._read_lock:
            with self._state_lock:
                if not self._ptr or self._closing or self._cancelled or self._finished:
                    return None
                ptr = self._ptr
            while True:
                if self._cancel is not None and self._cancel.is_set():
                    self.cancel()
                    return None
                event = plsm_udp_event_t()
                out_err = POINTER(plsm_error_t)()
                wait_ms = _wait_ms(deadline, self._cancel)
                owner = c_void_p()
                if self._views:
                    rc = _LIB.plsm_udp_follow_next_view(
                        ptr, c_uint64(wait_ms), byref(event), byref(owner), byref(out_err)
                    )
                else:
                    rc = _LIB.plsm_udp_follow_next(
                        ptr, c_uint64(wait_ms), byref(event), byref(out_err)
                    )
                if rc == 1:
                    if self._views and event.kind == 1:
                        try:
                            return adopt_owner(owner.value)
                        finally:
                            _LIB.plsm_udp_event_free(byref(event))
                    return _udp_event_to_python(event)
                if rc == 2 or rc == 3:
                    with self._state_lock:
                        self._finished = True
                    return None
                if rc < 0:
                    raise _take_error(out_err)
                if deadline is not None and monotonic_ns() >= deadline:
                    return None

    def next(self, wait: timedelta | float | None = None) -> UdpEvent | MessageView | None:
        event = self.next_event(wait)
        if event is None and (self._finished or self._closing or self._cancelled or not self._ptr):
            raise StopIteration
        return event

    def events(self) -> Generator[UdpEvent | MessageView, None, None]:
        while True:
            event = self.next_event()
            if event is None:
                return
            yield event

    def __iter__(self) -> UdpFollow:
        return self

    def __next__(self) -> UdpEvent | MessageView:
        return self.next()

    def cancel(self) -> None:
        with self._state_lock:
            if self._ptr and not self._cancelled:
                _LIB.plsm_udp_follow_cancel(self._ptr)
                self._cancelled = True

    def close(self) -> None:
        with self._state_lock:
            ptr = self._ptr
            if not ptr:
                return
            self._closing = True
            _LIB.plsm_udp_follow_cancel(ptr)
        with self._read_lock:
            with self._state_lock:
                if self._ptr:
                    _LIB.plsm_udp_follow_close(self._ptr)
                    self._ptr = None
                    self._cancelled = True

    def __enter__(self) -> UdpFollow:
        with self._state_lock:
            _require_open(self._ptr, "UDP follow")
        return self

    def __exit__(self, _exc_type, _exc, _tb) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


def _udp_event_to_python(event: plsm_udp_event_t) -> UdpEvent:
    try:
        if event.kind == 1:
            payload = b""
            if event.payload.data and event.payload.len:
                payload = string_at(event.payload.data, event.payload.len)
            return UdpFrame(int(event.seq), int(event.time), payload)
        if event.kind == 2:
            reasons = {0: "unknown", 1: "oversized", 2: "retention", 3: "pressure"}
            return UdpSkip(int(event.first), int(event.last), reasons.get(int(event.reason), "unknown"))
        if event.kind == 3:
            return UdpProgress(
                int(event.start_seq),
                int(event.next_seq),
                int(event.end_seq) if event.has_end else None,
                bool(event.terminal),
            )
        raise PlasmiteError(ErrorKind.INTERNAL, f"plasmite: unknown UDP event kind {event.kind}")
    finally:
        _LIB.plsm_udp_event_free(byref(event))


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
        time=_parse_time_ns(raw_time),
        data=parsed.get("data"),
        meta=MessageMeta(tags=tag_list),
        raw=raw,
    )


def create_pool(pool_ref: str, size_bytes: int = DEFAULT_POOL_SIZE_BYTES) -> Pool:
    with Client() as client:
        return client.create_pool(pool_ref, size_bytes)


def open_pool(pool_ref: str, *, create: bool = False) -> Pool:
    with Client() as client:
        return client.open_pool(pool_ref, create=create)


def list_pools() -> list[dict]:
    with Client() as client:
        return client.list_pools()


def delete_pool(pool_ref: str) -> None:
    with Client() as client:
        client.delete_pool(pool_ref)


__all__ = [
    "FeedReceipt",
    "Client",
    "Pool",
    "Message",
    "MessageMeta",
    "Stream",
    "Reader",
    "at",
    "create_pool",
    "open_pool",
    "list_pools",
    "delete_pool",
    "Lite3Frame",
    "Lite3Stream",
    "UdpClient",
    "UdpPool",
    "UdpFollow",
    "UdpFrame",
    "UdpSkip",
    "UdpProgress",
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
    "RetentionGapError",
    "DEFAULT_POOL_DIR",
    "DEFAULT_POOL_SIZE_BYTES",
    "default_pool_dir",
    "parse_message",
    "MessageView",
    "ObjectView",
    "ArrayView",
    "to_native",
    "to_json",
    "view_from_lite3",
]


# Keep extension loading lazy so legacy clients work with older system SDKs.
def to_native(value):
    from ._views import to_native as convert

    return convert(value)


def to_json(value):
    from ._views import to_json as convert

    return convert(value)


def view_from_lite3(payload, seq=0, time=0):
    from ._views import from_lite3

    return from_lite3(payload, seq, time)


def __getattr__(name):
    if name in {"MessageView", "ObjectView", "ArrayView"}:
        from . import _views

        value = getattr(_views, name)
        globals()[name] = value
        return value
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
