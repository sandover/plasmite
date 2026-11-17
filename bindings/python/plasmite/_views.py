"""Immutable Lite3 views; keep native protocols separate from legacy Messages."""

import base64
import ctypes
import json
import os
import operator
import platform
import sysconfig
import sys
import time
from collections.abc import ItemsView, Mapping, Sequence, ValuesView

from . import (
    _LIB,
    _require_open,
    _take_error,
    plsm_error_t,
    _optional_non_negative_int,
    _ensure_bytes,
)


def _native_error(address):
    return _take_error(ctypes.cast(address, ctypes.POINTER(plsm_error_t)))


_native_available = (
    platform.python_implementation() == "CPython"
    and not sysconfig.get_config_var("Py_GIL_DISABLED")
    and os.environ.get("PLASMITE_PYTHON_VIEWS", "1") != "0"
)
if _native_available:
    try:
        from . import _views_native as backend

        backend._configure(_LIB._handle, _native_error, _LIB)
    except ImportError:
        _native_available = False
if not _native_available:
    from . import _views_fallback as backend

MessageView = backend.MessageView
ObjectView = backend.ObjectView
ArrayView = backend.ArrayView
VIEW_BACKEND = "native" if _native_available else "python"


class _Items(ItemsView):
    def __iter__(self):
        return self._mapping._items()


class _Values(ValuesView):
    def __iter__(self):
        return self._mapping._values()


def _object_equal(self, other):
    if not isinstance(other, Mapping):
        return NotImplemented
    if len(self) != len(other):
        return False
    for key, value in self._items():
        try:
            if value != other[key]:
                return False
        except KeyError:
            return False
    return True


def _array_equal(self, other):
    if not isinstance(other, (ArrayView, list)):
        return NotImplemented
    return len(self) == len(other) and all(a == b for a, b in zip(self, other))


Mapping.register(ObjectView)
Sequence.register(ArrayView)
ObjectView.get = Mapping.get
ObjectView.keys = Mapping.keys
ObjectView.items = lambda self: _Items(self)
ObjectView.values = lambda self: _Values(self)
ObjectView.__eq__ = _object_equal
ObjectView.__hash__ = None
ArrayView.__eq__ = _array_equal
ArrayView.__hash__ = None
ArrayView.index = Sequence.index
ArrayView.count = Sequence.count
ArrayView.__contains__ = Sequence.__contains__
MessageView.meta = property(lambda self: self._root["meta"])
MessageView.tags = property(lambda self: self.meta["tags"])


def to_native(value):
    """Build independent native containers for one view or subtree."""
    if isinstance(value, MessageView):
        return {
            "seq": value.seq,
            "time": value.time,
            "meta": {"tags": to_native(value.tags)},
            "data": to_native(value.data),
        }
    if isinstance(value, (ObjectView, ArrayView)):
        return value._native()
    if isinstance(value, memoryview):
        return value.tobytes()
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    raise TypeError("to_native expects a Plasmite view or scalar")


def to_json(value):
    """Format a requested view directly, without a native container tree."""
    if isinstance(value, MessageView):
        return value._json()
    if isinstance(value, (ObjectView, ArrayView)):
        try:
            return value._json()
        except TypeError:
            if isinstance(value, ArrayView):
                # A slice describes logical indices rather than encoded bytes.
                return "[" + ",".join(to_json(item) for item in value) + "]"
            raise
    if isinstance(value, memoryview):
        return json.dumps(base64.b64encode(value).decode("ascii"))
    if value is None or isinstance(value, (bool, int, float, str)):
        return json.dumps(value, allow_nan=False)
    raise TypeError("to_json expects a Plasmite view or scalar")


def get_view(pool, seq):
    if not isinstance(seq, int) or not 0 <= seq < 1 << 64:
        raise ValueError("seq must fit an unsigned 64-bit integer")
    # Match the integer conversion before borrowing a pool pointer. An object
    # can report __class__=int while its __index__ still runs Python callbacks.
    seq = operator.index(seq)
    _require_open(pool._ptr, "pool")
    return backend.get(ctypes.cast(pool._ptr, ctypes.c_void_p).value, seq)


def from_lite3(payload, seq=0, timestamp_ns=0):
    if not isinstance(payload, (bytes, bytearray, memoryview)):
        raise TypeError("payload must be bytes-like")
    if not all(isinstance(n, int) and 0 <= n < 1 << 64 for n in [seq, timestamp_ns]):
        raise ValueError("identity must fit unsigned 64-bit integers")
    encoded = bytes(payload)
    # __bytes__ may return a subtype; its __len__ is not the native buffer size.
    if type(encoded) is not bytes:
        encoded = bytes.__getitem__(encoded, slice(None))
    return backend.from_bytes(encoded, seq, timestamp_ns)


def append_view(pool, data, tags, durability):
    # Callbacks can close this handle reentrantly through its RLock.
    normalized_tags = None if tags is None else list(tags)
    normalized_durability = int(durability)
    _require_open(pool._ptr, "pool")
    address = ctypes.cast(pool._ptr, ctypes.c_void_p).value
    return backend.append(address, data, normalized_tags, normalized_durability)


class ViewStream:
    """An explicit, bounded stream of independently retained message views."""

    def __init__(self, stream, cancel=None):
        self._stream = stream
        self._cancel = cancel

    def __iter__(self):
        return self

    def __next__(self):
        return self._read(1, sys.maxsize, None)[0]

    def next_batch(self, max_count, max_bytes, timeout_ms=None):
        # Normalize callbacks before borrowing a pointer or calculating deadlines.
        if not isinstance(max_count, int) or not isinstance(max_bytes, int):
            raise TypeError("batch bounds must be integers")
        max_count, max_bytes = int(max_count), int(max_bytes)
        if not 0 < max_count <= sys.maxsize or not 0 < max_bytes <= sys.maxsize:
            raise ValueError("batch bounds must be positive and fit address space")
        timeout_ms = _optional_non_negative_int(timeout_ms, "timeout_ms")
        timeout_ms = None if timeout_ms is None else int(timeout_ms)
        return self._read(max_count, max_bytes, timeout_ms)

    def _read(self, max_count, max_bytes, timeout_ms):
        deadline = None if timeout_ms is None else time.monotonic() + timeout_ms / 1000
        stream = self._stream
        while True:
            if self._cancel is not None and self._cancel.is_set():
                stream.cancel()
                raise StopIteration
            wait = (
                50
                if deadline is None
                else min(50, max(0, int((deadline - time.monotonic()) * 1000)))
            )
            with stream._lock:
                _require_open(stream._ptr, "stream")
                address = ctypes.cast(stream._ptr, ctypes.c_void_p).value
                status, items = backend.next_batch(address, max_count, max_bytes, wait)
            if status == 1:
                if self._cancel is not None and self._cancel.is_set():
                    stream.cancel()
                    raise StopIteration
                return items
            if status in (2, 3):
                raise StopIteration
            if deadline is not None and time.monotonic() >= deadline:
                return []

    def cancel(self):
        self._stream.cancel()

    def close(self):
        self._stream.close()

    def __enter__(self):
        self._stream.__enter__()
        return self

    def __exit__(self, *args):
        self.close()
