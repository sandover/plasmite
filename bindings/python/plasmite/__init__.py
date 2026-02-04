"""
Purpose: Provide Python bindings for the libplasmite C ABI (v0).
Key Exports: Client, Pool, Stream, Durability, ErrorKind, PlasmiteError.
Role: Minimal, ergonomic wrapper around include/plasmite.h for Python users.
Invariants: JSON bytes in/out; explicit close/free for native handles.
Invariants: Errors preserve stable kinds and context fields.
Notes: Uses ctypes and links to libplasmite resolved at runtime.
"""

from __future__ import annotations

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
from enum import IntEnum
import json
import os
import sys
from typing import Iterable, Optional


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


class plsm_client_t(Structure):
    pass


class plsm_pool_t(Structure):
    pass


class plsm_stream_t(Structure):
    pass


class plsm_buf_t(Structure):
    _fields_ = [
        ("data", c_void_p),
        ("len", c_size_t),
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
            for name in ("libplasmite.dylib", "libplasmite.so")
        ]
        for candidate in candidates:
            if os.path.exists(candidate):
                return CDLL(candidate)
    for name in ("plasmite", "libplasmite"):
        try:
            return CDLL(name)
        except OSError:
            continue
    raise OSError(
        "libplasmite not found; set PLASMITE_LIB_DIR or build target/debug/libplasmite.*"
    )


_LIB = _load_lib()

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

_LIB.plsm_pool_get_json.argtypes = [
    POINTER(plsm_pool_t),
    c_uint64,
    POINTER(plsm_buf_t),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_pool_get_json.restype = c_int

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

_LIB.plsm_stream_next.argtypes = [
    POINTER(plsm_stream_t),
    POINTER(plsm_buf_t),
    POINTER(POINTER(plsm_error_t)),
]
_LIB.plsm_stream_next.restype = c_int

_LIB.plsm_stream_free.argtypes = [POINTER(plsm_stream_t)]
_LIB.plsm_stream_free.restype = None

_LIB.plsm_buf_free.argtypes = [POINTER(plsm_buf_t)]
_LIB.plsm_buf_free.restype = None

_LIB.plsm_error_free.argtypes = [POINTER(plsm_error_t)]
_LIB.plsm_error_free.restype = None


def _take_error(err_ptr: POINTER(plsm_error_t) | None) -> PlasmiteError:
    if not err_ptr:
        return PlasmiteError(ErrorKind.INTERNAL, "plasmite: unknown error")
    err = err_ptr.contents
    message = err.message.decode("utf-8") if err.message else ""
    path = err.path.decode("utf-8") if err.path else None
    seq = int(err.seq) if err.has_seq else None
    offset = int(err.offset) if err.has_offset else None
    _LIB.plsm_error_free(err_ptr)
    return PlasmiteError(ErrorKind(err.kind), message or "plasmite: error", path, seq, offset)


def _buf_to_bytes(buf: plsm_buf_t) -> bytes:
    if not buf.data or buf.len == 0:
        _LIB.plsm_buf_free(byref(buf))
        return b""
    data = (c_uint8 * buf.len).from_address(buf.data)
    out = bytes(data)
    _LIB.plsm_buf_free(byref(buf))
    return out


def _descrip_array(values: Iterable[str]) -> tuple[POINTER(c_char_p), list[bytes]]:
    encoded = [value.encode("utf-8") for value in values]
    if not encoded:
        return POINTER(c_char_p)(), encoded
    arr = (c_char_p * len(encoded))()
    for idx, val in enumerate(encoded):
        arr[idx] = val
    return arr, encoded


class Client:
    def __init__(self, pool_dir: str) -> None:
        if not pool_dir:
            raise ValueError("pool_dir is required")
        out_client = POINTER(plsm_client_t)()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_client_new(pool_dir.encode("utf-8"), byref(out_client), byref(out_err))
        if rc != 0:
            raise _take_error(out_err)
        self._ptr = out_client

    def create_pool(self, pool_ref: str, size_bytes: int) -> Pool:
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

    def close(self) -> None:
        if getattr(self, "_ptr", None):
            _LIB.plsm_client_free(self._ptr)
            self._ptr = None

    def __del__(self) -> None:
        self.close()


class Pool:
    def __init__(self, ptr: POINTER(plsm_pool_t)) -> None:
        self._ptr = ptr

    def append_json(self, payload: bytes, descrips: Iterable[str], durability: Durability) -> bytes:
        buf = plsm_buf_t()
        out_err = POINTER(plsm_error_t)()
        arr, _keep = _descrip_array(descrips)
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

    def get_json(self, seq: int) -> bytes:
        buf = plsm_buf_t()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_pool_get_json(self._ptr, c_uint64(seq), byref(buf), byref(out_err))
        if rc != 0:
            raise _take_error(out_err)
        return _buf_to_bytes(buf)

    def open_stream(
        self,
        since_seq: Optional[int] = None,
        max_messages: Optional[int] = None,
        timeout_ms: Optional[int] = None,
    ) -> Stream:
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

    def close(self) -> None:
        if getattr(self, "_ptr", None):
            _LIB.plsm_pool_free(self._ptr)
            self._ptr = None

    def __del__(self) -> None:
        self.close()


class Stream:
    def __init__(self, ptr: POINTER(plsm_stream_t)) -> None:
        self._ptr = ptr

    def next_json(self) -> Optional[bytes]:
        buf = plsm_buf_t()
        out_err = POINTER(plsm_error_t)()
        rc = _LIB.plsm_stream_next(self._ptr, byref(buf), byref(out_err))
        if rc == 1:
            return _buf_to_bytes(buf)
        if rc == 0:
            return None
        raise _take_error(out_err)

    def close(self) -> None:
        if getattr(self, "_ptr", None):
            _LIB.plsm_stream_free(self._ptr)
            self._ptr = None

    def __del__(self) -> None:
        self.close()


def parse_message(payload: bytes) -> dict:
    return json.loads(payload.decode("utf-8"))


__all__ = [
    "Client",
    "Pool",
    "Stream",
    "Durability",
    "ErrorKind",
    "PlasmiteError",
    "parse_message",
]
