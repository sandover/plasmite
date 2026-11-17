"""Functional Lite3 fallback; native wheels avoid these ctypes field crossings.

The bytes subclass supplies a safe readonly buffer on Python 3.10 too. It keeps
an encoded copy for that buffer; it never constructs an eager native data tree.
"""

import ctypes as c
import operator
from collections.abc import Mapping, Sequence
from threading import Lock

from . import _LIB as lib
from . import _take_error, _tag_array, plsm_buf_t, plsm_error_t, plsm_feed_options_t, plsm_pool_t, plsm_stream_t


class Ref(c.Structure):
    _fields_ = [("owner", c.c_void_p), ("offset", c.c_size_t)]


class Info(c.Structure):
    _fields_ = [
        ("kind", c.c_uint32),
        ("integer", c.c_int64),
        ("real", c.c_double),
        ("bytes", c.c_void_p),
        ("len", c.c_size_t),
    ]


class Text(c.Structure):
    _fields_ = [("data", c.c_void_p), ("len", c.c_size_t)]


class Entry(c.Structure):
    _fields_ = [("key", c.c_void_p), ("key_len", c.c_size_t), ("value", Ref)]


_ready = False
_configure_lock = Lock()


def _configure():
    # Freeze ctypes signatures before concurrent free-threaded calls can use them.
    with _configure_lock:
        _configure_unlocked()


def _configure_unlocked():
    global _ready
    if _ready:
        return
    err = c.POINTER(c.POINTER(plsm_error_t))
    owner = c.POINTER(c.c_void_p)
    signatures = {
        "plsm_internal_pool_get_view": (
            [c.POINTER(plsm_pool_t), c.c_uint64, owner, err],
            c.c_int,
        ),
        "plsm_view_from_lite3": (
            [c.c_char_p, c.c_size_t, c.c_uint64, c.c_uint64, owner, err],
            c.c_int,
        ),
        "plsm_view_stream_next": (
            [
                c.POINTER(plsm_stream_t),
                c.c_uint64,
                c.c_size_t,
                c.c_size_t,
                c.POINTER(c.c_void_p),
                c.POINTER(c.c_size_t),
                err,
            ],
            c.c_int,
        ),
        "plsm_pool_feed_view": (
            [c.POINTER(plsm_pool_t), c.c_void_p, c.c_uint32, owner, err],
            c.c_int,
        ),
        "plsm_builder_copy_value": (
            [c.c_void_p, c.c_size_t, c.c_char_p, c.c_size_t, Ref, err],
            c.c_int,
        ),
        "plsm_view_release": ([c.c_void_p], None),
        "plsm_view_seq": ([c.c_void_p], c.c_uint64),
        "plsm_view_time": ([c.c_void_p], c.c_uint64),
        "plsm_view_root": ([c.c_void_p], Ref),
        "plsm_view_data": ([c.c_void_p], Ref),
        "plsm_view_bytes": ([c.c_void_p, c.POINTER(c.c_size_t)], c.c_void_p),
        "plsm_value_read": ([Ref, c.POINTER(Info), err], c.c_int),
        "plsm_value_get": ([Ref, c.c_char_p, c.c_size_t, c.POINTER(Ref), err], c.c_int),
        "plsm_value_index": ([Ref, c.c_uint32, c.POINTER(Ref), err], c.c_int),
        "plsm_value_iter": ([Ref, owner, err], c.c_int),
        "plsm_cursor_next": (
            [c.c_void_p, c.POINTER(Entry), c.c_size_t, c.POINTER(c.c_size_t), err],
            c.c_int,
        ),
        "plsm_cursor_free": ([c.c_void_p], None),
        "plsm_value_to_json": ([Ref, c.POINTER(plsm_buf_t), err], c.c_int),
        "plsm_view_to_json": ([c.c_void_p, c.POINTER(plsm_buf_t), err], c.c_int),
    }
    signatures.update(
        {
            "plsm_pool_feed_json_view": (
                [
                    c.POINTER(plsm_pool_t),
                    c.c_char_p,
                    c.c_size_t,
                    c.POINTER(plsm_feed_options_t),
                    owner,
                    err,
                ],
                c.c_int,
            ),
            "plsm_builder_new": (
                [c.POINTER(Text), c.c_size_t, owner, c.POINTER(c.c_size_t), err],
                c.c_int,
            ),
            "plsm_builder_put": (
                [
                    c.c_void_p,
                    c.c_size_t,
                    c.c_char_p,
                    c.c_size_t,
                    c.POINTER(Info),
                    c.POINTER(c.c_size_t),
                    err,
                ],
                c.c_int,
            ),
            "plsm_pool_feed_builder_view": (
                [c.POINTER(plsm_pool_t), c.c_void_p, c.c_uint32, owner, err],
                c.c_int,
            ),
            "plsm_builder_free": ([c.c_void_p], None),
        }
    )
    try:
        for name, (arguments, result) in signatures.items():
            function = getattr(lib, name)
            function.argtypes, function.restype = arguments, result
    except AttributeError as exc:
        raise NotImplementedError(
            "view APIs require a libplasmite SDK with owned-view support"
        ) from exc
    _ready = True


def _call(function, *args):
    error = c.POINTER(plsm_error_t)()
    if function(*args, c.byref(error)):
        raise _take_error(error)


def _info(ref):
    out = Info()
    _call(lib.plsm_value_read, ref, c.byref(out))
    return out


def _wrap(snapshot, ref):
    # A ctypes structure field can borrow a reusable iterator buffer. Copy the
    # two logical reference fields before retaining a child beyond this chunk.
    ref = Ref(ref.owner, ref.offset)
    info = _info(ref)
    if info.kind == 1:
        return None
    if info.kind == 2:
        return bool(info.integer)
    if info.kind == 3:
        return info.integer
    if info.kind == 4:
        return info.real
    if info.kind == 5:
        return c.string_at(info.bytes, info.len).decode("utf-8")
    if info.kind == 6:
        return memoryview(c.string_at(info.bytes, info.len))
    if info.kind == 7:
        return ObjectView(snapshot, ref, info.len)
    if info.kind == 8:
        return ArrayView(snapshot, ref, range(info.len))
    raise TypeError("missing value has no representation")


def _entries(snapshot, ref):
    cursor = c.c_void_p()
    _call(lib.plsm_value_iter, ref, c.byref(cursor))
    try:
        slots = (Entry * 16)()
        while True:
            count = c.c_size_t()
            _call(lib.plsm_cursor_next, cursor, slots, 16, c.byref(count))
            if not count.value:
                break
            for i in range(count.value):
                entry = slots[i]
                key = (
                    c.string_at(entry.key, entry.key_len).decode("utf-8")
                    if entry.key
                    else None
                )
                yield key, _wrap(snapshot, entry.value)
    finally:
        lib.plsm_cursor_free(cursor)


class MessageView(bytes):
    def __new__(cls, ptr):
        try:
            size = c.c_size_t()
            address = lib.plsm_view_bytes(ptr, c.byref(size))
            value = super().__new__(cls, c.string_at(address, size.value))
            value._ptr = ptr
            return value
        except BaseException:
            lib.plsm_view_release(ptr)
            raise

    def __del__(self):
        ptr = getattr(self, "_ptr", None)
        if ptr:
            self._ptr = None
            lib.plsm_view_release(ptr)

    @property
    def seq(self):
        return lib.plsm_view_seq(self._ptr)

    @property
    def time(self):
        return lib.plsm_view_time(self._ptr)

    @property
    def data(self):
        return _wrap(self, lib.plsm_view_data(self._ptr))

    @property
    def _root(self):
        return _wrap(self, lib.plsm_view_root(self._ptr))

    def _json(self):
        output = plsm_buf_t()
        _call(lib.plsm_view_to_json, self._ptr, c.byref(output))
        try:
            return c.string_at(output.data, output.len).decode("utf-8")
        finally:
            lib.plsm_buf_free(c.byref(output))

    def __repr__(self):
        return f"<plasmite.MessageView seq={self.seq}>"

    def __eq__(self, other):
        return self is other

    def __ne__(self, other):
        return self is not other

    __hash__ = object.__hash__


class ObjectView(Mapping):
    __slots__ = ("_snapshot", "_ref", "_length")

    def __init__(self, snapshot, ref, length):
        self._snapshot, self._ref, self._length = snapshot, ref, length

    def __len__(self):
        return self._length

    def __getitem__(self, key):
        if not isinstance(key, str):
            hash(key)
            raise KeyError(key)
        try:
            text = key.encode("utf-8")
        except UnicodeEncodeError:
            raise KeyError(key) from None
        out = Ref()
        _call(lib.plsm_value_get, self._ref, text, len(text), c.byref(out))
        if not out.owner:
            raise KeyError(key)
        return _wrap(self._snapshot, out)

    def __contains__(self, key):
        if not isinstance(key, str):
            hash(key)
            return False
        try:
            text = key.encode("utf-8")
        except UnicodeEncodeError:
            return False
        out = Ref()
        _call(lib.plsm_value_get, self._ref, text, len(text), c.byref(out))
        return bool(out.owner)

    def __iter__(self):
        return (key for key, _ in _entries(self._snapshot, self._ref))

    def _items(self):
        return _entries(self._snapshot, self._ref)

    def _values(self):
        return (value for _, value in self._items())

    def _native(self):
        from ._views import to_native

        return {key: to_native(value) for key, value in self._items()}

    def _json(self):
        return _json(self._ref)

    def __repr__(self):
        return f"<plasmite.ObjectView len={len(self)}>"


class ArrayView(Sequence):
    __slots__ = ("_snapshot", "_ref", "_indices", "_sliced")

    def __init__(self, snapshot, ref, indices, sliced=False):
        self._snapshot, self._ref = snapshot, ref
        self._indices, self._sliced = indices, sliced

    def __len__(self):
        return len(self._indices)

    def __getitem__(self, key):
        if isinstance(key, slice):
            return ArrayView(self._snapshot, self._ref, self._indices[key], True)
        index = self._indices[operator.index(key)]
        out = Ref()
        _call(lib.plsm_value_index, self._ref, index, c.byref(out))
        return _wrap(self._snapshot, out)

    def __iter__(self):
        if self._sliced:
            return (self[i] for i in range(len(self)))
        return (value for _, value in _entries(self._snapshot, self._ref))

    def _native(self):
        from ._views import to_native

        return [to_native(value) for value in self]

    def _json(self):
        if self._sliced:
            raise TypeError("slice has no standalone encoded subtree")
        return _json(self._ref)

    def __repr__(self):
        return f"<plasmite.ArrayView len={len(self)}>"


def _json(ref):
    output = plsm_buf_t()
    _call(lib.plsm_value_to_json, ref, c.byref(output))
    try:
        return c.string_at(output.data, output.len).decode("utf-8")
    finally:
        lib.plsm_buf_free(c.byref(output))


def get(address, seq):
    _configure()
    out = c.c_void_p()
    _call(
        lib.plsm_internal_pool_get_view,
        c.cast(address, c.POINTER(plsm_pool_t)),
        seq,
        c.byref(out),
    )
    return MessageView(out)


def from_bytes(payload, seq, timestamp_ns):
    _configure()
    out = c.c_void_p()
    _call(
        lib.plsm_view_from_lite3, payload, len(payload), seq, timestamp_ns, c.byref(out)
    )
    return MessageView(out)


def _snapshot_input(value, parents, binary):
    kind = type(value)
    if kind in (dict, list, tuple):
        if any(value is parent for parent in parents):
            raise ValueError("circular Python input")
        parents = (*parents, value)
        if kind is dict:
            out = {}
            for key, child in value.items():
                if type(key) is not str:
                    raise TypeError("append_view object keys must be exact str values")
                out[key] = _snapshot_input(child, parents, binary)
            return out
        return [_snapshot_input(child, parents, binary) for child in value]
    if kind in (ObjectView, ArrayView):
        binary[0] = True
        return value
    if kind in (bytes, bytearray, memoryview):
        binary[0] = True
        return bytes(value)
    if kind is int and not -(1 << 63) <= value < 1 << 63:
        raise OverflowError("Python int too large to convert to C long long")
    if value is None or kind in (bool, int, float, str):
        return value
    raise TypeError(
        "append_view supports exact builtins and byte buffers; use append for subclasses"
    )


def append(address, data, tags, durability):
    # ctypes releases the GIL. Freeze mutable inputs before its first crossing.
    _configure()
    pool = c.cast(address, c.POINTER(plsm_pool_t))
    out = c.c_void_p()
    if type(data) is MessageView and tags is None:
        _call(lib.plsm_pool_feed_view, pool, data._ptr, durability, c.byref(out))
        return MessageView(out)
    if type(data) is MessageView:
        data = data.data
    if type(data) not in (dict, ObjectView):
        raise TypeError(
            "append_view data must be an exact dict, ObjectView or MessageView"
        )
    tags = [] if tags is None else tags
    if any(type(tag) is not str for tag in tags):
        raise TypeError("append_view tags must be exact str values")
    binary = [False]
    data = _snapshot_input(data, (), binary)
    encoded_tags = [tag.encode("utf-8") for tag in tags]
    _configure()
    pool = c.cast(address, c.POINTER(plsm_pool_t))
    out = c.c_void_p()
    if not binary[0]:
        # Bulk JSON avoids one ctypes call per field on compatibility runtimes.
        import json

        payload = json.dumps(data, ensure_ascii=False, separators=(",", ":")).encode(
            "utf-8"
        )
        tag_array, encoded = _tag_array(tags)
        options = plsm_feed_options_t(c.sizeof(plsm_feed_options_t), durability, tag_array, len(encoded))
        _call(
            lib.plsm_pool_feed_json_view,
            pool,
            payload,
            len(payload),
            c.byref(options),
            c.byref(out),
        )
        return MessageView(out)
    spans = (Text * len(tags))(
        *[Text(c.cast(c.c_char_p(tag), c.c_void_p), len(tag)) for tag in encoded_tags]
    )
    builder, root = c.c_void_p(), c.c_size_t()
    _call(lib.plsm_builder_new, spans, len(tags), c.byref(builder), c.byref(root))
    try:
        if type(data) is ObjectView:
            _call(
                lib.plsm_builder_copy_value, builder, c.c_size_t(-1), None, 0, data._ref
            )
        else:
            _write_container(builder, root.value, data)
        _call(lib.plsm_pool_feed_builder_view, pool, builder, durability, c.byref(out))
        return MessageView(out)
    finally:
        lib.plsm_builder_free(builder)


def _write_container(builder, node, value):
    items = value.items() if type(value) is dict else ((None, child) for child in value)
    for key, child in items:
        key_bytes = key.encode("utf-8") if key is not None else None
        if type(child) is ObjectView or (
            type(child) is ArrayView and not child._sliced
        ):
            _call(
                lib.plsm_builder_copy_value,
                builder,
                node,
                key_bytes,
                len(key_bytes) if key_bytes is not None else 0,
                child._ref,
            )
            continue
        if type(child) is ArrayView:
            info, next_node = Info(), c.c_size_t()
            info.kind = 8
            _call(
                lib.plsm_builder_put,
                builder,
                node,
                key_bytes,
                len(key_bytes) if key_bytes is not None else 0,
                c.byref(info),
                c.byref(next_node),
            )
            for index in child._indices:
                ref = Ref()
                _call(lib.plsm_value_index, child._ref, index, c.byref(ref))
                _call(
                    lib.plsm_builder_copy_value, builder, next_node.value, None, 0, ref
                )
            continue
        info = Info()
        encoded = None
        if child is None:
            info.kind = 1
        elif type(child) is bool:
            info.kind, info.integer = 2, child
        elif type(child) is int:
            info.kind, info.integer = 3, child
        elif type(child) is float:
            info.kind, info.real = 4, child
        elif type(child) in (str, bytes):
            info.kind = 5 if type(child) is str else 6
            encoded = child.encode("utf-8") if type(child) is str else child
            info.bytes, info.len = c.cast(c.c_char_p(encoded), c.c_void_p), len(encoded)
        else:
            info.kind = 7 if type(child) is dict else 8
        key_bytes = key.encode("utf-8") if key is not None else None
        next_node = c.c_size_t()
        _call(
            lib.plsm_builder_put,
            builder,
            node,
            key_bytes,
            len(key_bytes) if key_bytes is not None else 0,
            c.byref(info),
            c.byref(next_node),
        )
        if info.kind in (7, 8):
            _write_container(builder, next_node.value, child)


def adopt_owner(address):
    try:
        _configure()
    except BaseException:
        lib.plsm_view_release(c.c_void_p(address))
        raise
    return MessageView(address)


def next_batch(address, max_count, max_bytes, timeout_ms):
    _configure()
    owners = (c.c_void_p * max_count)()
    count = c.c_size_t()
    error = c.POINTER(plsm_error_t)()
    status = lib.plsm_view_stream_next(
        c.cast(address, c.POINTER(plsm_stream_t)),
        timeout_ms,
        max_count,
        max_bytes,
        owners,
        c.byref(count),
        c.byref(error),
    )
    items = []
    try:
        if status < 0:
            raise _take_error(error)
        for index in range(count.value):
            ptr = owners[index]
            owners[index] = None
            items.append(MessageView(ptr))
        return status, items
    finally:
        for index in range(count.value):
            if owners[index]:
                lib.plsm_view_release(owners[index])
