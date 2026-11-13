"""Immutable Lite3 views; keep native protocols separate from legacy Messages."""

import base64
import ctypes
import json
import os
import platform
import sysconfig
from collections.abc import ItemsView, Mapping, Sequence, ValuesView
from datetime import datetime, timezone

from . import _LIB, _take_error, plsm_error_t


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


def _time_text(message):
    seconds, nanoseconds = divmod(message.timestamp_ns, 1_000_000_000)
    return (
        datetime.fromtimestamp(seconds, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
        + f".{nanoseconds:09d}Z"
    )


MessageView.time_rfc3339 = property(_time_text)
MessageView.time = property(
    lambda self: datetime.fromtimestamp(
        self.timestamp_ns // 1_000_000_000, timezone.utc
    ).replace(microsecond=self.timestamp_ns % 1_000_000_000 // 1000)
)


def to_native(value):
    """Build independent native containers for one view or subtree."""
    if isinstance(value, MessageView):
        return {
            "seq": value.seq,
            "time": value.time_rfc3339,
            "meta": value.meta._native(),
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
        return (
            '{"seq":'
            + str(value.seq)
            + ',"time":'
            + json.dumps(value.time_rfc3339)
            + ',"meta":'
            + value.meta._json()
            + ',"data":'
            + to_json(value.data)
            + "}"
        )
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
    return backend.get(ctypes.cast(pool._ptr, ctypes.c_void_p).value, seq)


def from_lite3(payload, seq=0, timestamp_ns=0):
    if not isinstance(payload, (bytes, bytearray, memoryview)):
        raise TypeError("payload must be bytes-like")
    if not all(isinstance(n, int) and 0 <= n < 1 << 64 for n in [seq, timestamp_ns]):
        raise ValueError("identity must fit unsigned 64-bit integers")
    return backend.from_bytes(bytes(payload), seq, timestamp_ns)
