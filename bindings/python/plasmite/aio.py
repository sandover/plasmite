"""Asyncio access to Plasmite pools."""

from __future__ import annotations

import asyncio
import ctypes as c
import itertools
import json
import os
import weakref
from concurrent.futures import Future, ThreadPoolExecutor
from functools import partial
from threading import Lock
from time import monotonic_ns

from . import (
    Client as _SyncClient,
    DEFAULT_POOL_SIZE_BYTES,
    Durability,
    ErrorKind,
    Message,
    NotFoundError,
    Reader as _SyncReader,
    UdpClient as _SyncUdpClient,
    UsageError,
    _LIB,
    _feed_input,
    _read_start,
    _require_open,
    _Stream,
    _tag_array,
    _take_error,
    _wait_deadline,
    at,
    plsm_error_t,
    plsm_feed_options_t,
    plsm_pool_t,
    plsm_read_options_t,
    plsm_stream_t,
    plsm_udp_pool_t,
)
from ._views import ViewStream, backend


_worker_count = min(8, max(2, os.cpu_count() or 2))
_workers = ThreadPoolExecutor(max_workers=_worker_count, thread_name_prefix="plasmite")
_open_workers = ThreadPoolExecutor(max_workers=_worker_count, thread_name_prefix="plasmite-open")


async def _work(call, *args, **kwargs):
    return await asyncio.get_running_loop().run_in_executor(
        _workers, partial(call, *args, **kwargs)
    )


class Client:
    """Open pools in a directory or by HTTPS URL."""

    def __init__(self, pool_dir=None):
        self._client = _SyncClient(pool_dir)

    def open_pool(self, pool_ref, *, create=False):
        return Pool(partial(self._client.open_pool, pool_ref, create=create))

    def create_pool(self, pool_ref, size_bytes=DEFAULT_POOL_SIZE_BYTES, **description):
        return Pool(partial(self._client.create_pool, pool_ref, size_bytes, **description))

    async def list_pools(self):
        return await _work(self._client.list_pools)

    async def delete_pool(self, pool_ref):
        await _work(self._client.delete_pool, pool_ref)

    async def aclose(self):
        await _work(self._client.close)

    async def __aenter__(self):
        self._client.__enter__()
        return self

    async def __aexit__(self, *_):
        await self.aclose()


class UdpClient:
    """Open pools on a UDP listener."""

    def __init__(self, endpoint):
        self._client = _SyncUdpClient(endpoint)

    def open_pool(self, pool_name):
        return Pool(partial(self._client.open_pool, pool_name), udp=True)

    async def aclose(self):
        await _work(self._client.close)

    async def __aenter__(self):
        self._client.__enter__()
        return self

    async def __aexit__(self, *_):
        await self.aclose()


class Pool:
    def __init__(self, open_pool, *, udp=False):
        self._open = open_pool
        self._native = None
        self._opening = None
        self._closed = False
        self._remote = False
        self._udp = udp

    async def _get_pool(self, deadline=None):
        if self._closed:
            raise UsageError(ErrorKind.USAGE, "pool is closed")
        if self._native is None:
            if self._opening is None:
                self._opening = asyncio.wrap_future(_open_workers.submit(self._open))
            native = await _wait(self._opening, deadline)
            if self._closed:
                raise UsageError(ErrorKind.USAGE, "pool is closed")
            self._native = native
            self._remote = self._udp or _LIB.plsm_internal_pool_is_remote(native._ptr)
        return self._native

    async def feed(self, value, tags=None, durability=Durability.FAST, *, format=None):
        if durability == "flush":
            durability = Durability.FLUSH
        elif durability == "fast":
            durability = Durability.FAST
        pool = await self._get_pool()
        feed = partial(pool.feed, value, tags, durability, format=format)
        if self._udp:
            return feed()
        if self._remote:
            return await _feed_http(pool, value, tags, durability, format)
        if int(durability) == Durability.FLUSH:
            return await _work(feed)
        return feed()

    async def info(self):
        pool = await self._get_pool()
        if self._udp:
            raise UsageError(ErrorKind.USAGE, "UDP pools do not provide info")
        return await _work(pool.info)

    def follow(self, *, since=None, last=None, tags=None):
        return Reader(self, True, since, last, tags)

    def fetch(self, *, since=None, last=None, tags=None):
        return Reader(self, False, since, last, tags)

    async def aclose(self):
        self._closed = True
        if self._opening is not None:
            native = await asyncio.shield(self._opening)
            await _work(native.close)

    async def __aenter__(self):
        await self._get_pool()
        return self

    async def __aexit__(self, *_):
        await self.aclose()


_WakeCallback = c.CFUNCTYPE(None, c.c_uint64, c.c_bool)
_next_token = itertools.count(1)
_listeners = {}


@_WakeCallback
def _wake_loop(token, removed):
    listener = _listeners.get(token)
    if listener is not None:
        listener.wake(removed)


class _LocalSignal:
    def __init__(self):
        self.loop = asyncio.get_running_loop()
        self.changed = asyncio.Event()
        self.token = next(_next_token)
        self.lock = Lock()
        self.scheduled = False
        self.removed = False
        _listeners[self.token] = self

    def prepare(self):
        self.changed.clear()
        with self.lock:
            self.scheduled = False

    def wake(self, removed):
        with self.lock:
            self.removed |= removed
            if self.scheduled:
                return
            self.scheduled = True
        try:
            self.loop.call_soon_threadsafe(self.changed.set)
        except RuntimeError:
            # The loop closed while a native write was in flight.
            pass

    def close(self):
        _listeners.pop(self.token, None)


def _open_reader(pool, live, since, last, tags, token):
    kind, value = _read_start(since, last)
    spans, keepalive = _tag_array([] if tags is None else tags)
    options = plsm_read_options_t(c.sizeof(plsm_read_options_t), kind, value, spans, len(keepalive))
    stream, error = c.POINTER(plsm_stream_t)(), c.POINTER(plsm_error_t)()
    with pool._lock:
        _require_open(pool._ptr, "pool")
        status = _LIB.plsm_internal_local_async_read(
            pool._ptr, c.byref(options), live, _wake_loop, token, c.byref(stream), c.byref(error)
        )
    if status:
        raise _take_error(error)
    return _SyncReader(ViewStream(_Stream(stream)))


_LIB.plsm_internal_pool_is_remote.argtypes = [c.POINTER(plsm_pool_t)]
_LIB.plsm_internal_pool_is_remote.restype = c.c_bool
_LIB.plsm_internal_local_async_read.argtypes = [
    c.POINTER(plsm_pool_t), c.POINTER(plsm_read_options_t), c.c_bool, _WakeCallback, c.c_uint64,
    c.POINTER(c.POINTER(plsm_stream_t)), c.POINTER(c.POINTER(plsm_error_t)),
]
_LIB.plsm_internal_local_async_read.restype = c.c_int


_NetReport = c.CFUNCTYPE(None, c.c_uint64, c.c_int, c.c_void_p, c.POINTER(plsm_error_t))
_network_jobs = {}


class _NetworkResult:
    """Own a native result until asyncio delivers or discards its callback."""

    def __init__(self, status, value, error):
        self.status = status
        self.value = value
        self.error = error

    def take(self):
        status, value, error = self.status, self.value, self.error
        self.status = 0
        return status, value, error

    def __del__(self):
        if self.status == 4:
            _LIB.plsm_internal_async_network_close(self.value)
        elif self.status == 1:
            _LIB.plsm_view_release(self.value)
        elif self.status < 0:
            _LIB.plsm_error_free(self.error)


@_NetReport
def _network_result(token, status, value, error):
    result = _NetworkResult(status, value, error)
    job = _network_jobs.pop(token, None)
    if job is not None:
        try:
            job.loop.call_soon_threadsafe(job.complete, result)
        except RuntimeError:
            pass


class _NetworkReader:
    def __init__(self, address):
        self.address = address

    def close(self):
        if self.address is not None:
            _LIB.plsm_internal_async_network_close(self.address)
            self.address = None

    def __del__(self):
        self.close()


class _NetworkJob:
    def __init__(self, call, *args):
        self.loop = asyncio.get_running_loop()
        self.future = self.loop.create_future()
        self.address = c.c_void_p()
        self.discard = False
        token = next(_next_token)
        _network_jobs[token] = self
        error = c.POINTER(plsm_error_t)()
        try:
            status = call(*args, token, _network_result, c.byref(self.address), c.byref(error))
            if status:
                raise _take_error(error)
        except BaseException:
            _network_jobs.pop(token, None)
            raise

    def close(self):
        self.discard = True
        self._release()
        return self.future

    def _release(self):
        if self.address:
            _LIB.plsm_internal_async_job_free(self.address)
            self.address = None

    def __del__(self):
        self._release()

    def complete(self, result):
        self._release()
        if self.discard:
            self.future.set_result(None)
            return
        status, value, error = result.take()
        if status < 0:
            self.future.set_exception(_take_error(error))
        elif status == 4:
            self.future.set_result(_NetworkReader(value))
        elif status == 1:
            try:
                message = Message(backend.adopt_owner(value))
            except Exception as exc:
                self.future.set_exception(exc)
            else:
                self.future.set_result(message)
        else:
            self.future.set_result(_END)


_job_args = [
    c.c_uint64, _NetReport, c.POINTER(c.c_void_p), c.POINTER(c.POINTER(plsm_error_t)),
]
_LIB.plsm_internal_async_network_read.argtypes = [
    c.POINTER(plsm_pool_t), c.POINTER(plsm_udp_pool_t), c.POINTER(plsm_read_options_t),
    c.c_bool, *_job_args,
]
_LIB.plsm_internal_async_network_next.argtypes = [c.c_void_p, *_job_args]
_LIB.plsm_internal_async_http_feed.argtypes = [
    c.POINTER(plsm_pool_t), c.c_uint32, c.c_char_p, c.c_size_t,
    c.POINTER(plsm_feed_options_t), *_job_args,
]
_LIB.plsm_internal_async_network_close.argtypes = [c.c_void_p]
_LIB.plsm_internal_async_network_close.restype = None
_LIB.plsm_internal_async_job_free.argtypes = [c.c_void_p]
_LIB.plsm_internal_async_job_free.restype = None
_LIB.plsm_view_release.argtypes = [c.c_void_p]
_LIB.plsm_view_release.restype = None


def _open_network(pool, udp, live, since, last, tags):
    kind, value = _read_start(since, last)
    spans, keepalive = _tag_array([] if tags is None else tags)
    options = plsm_read_options_t(c.sizeof(plsm_read_options_t), kind, value, spans, len(keepalive))
    with pool._lock:
        _require_open(pool._ptr, "pool")
        return _NetworkJob(
            _LIB.plsm_internal_async_network_read,
            None if udp else pool._ptr, pool._ptr if udp else None, c.byref(options), live,
        )


async def _feed_http(pool, value, tags, durability, format):
    value = _feed_input(value, format, tags)
    if isinstance(value, Message):
        payload, kind = bytes(value.raw), 2
    elif format == "lite3":
        payload, kind = value, 2
    elif format == "json":
        payload, kind = value, 1
    else:
        tags = None if tags is None else list(tags)
        durability = int(durability)
        try:
            payload = bytes(backend.append(None, value, tags, durability))
            kind, tags = 2, None
        except TypeError:
            # Match the synchronous feed for JSON-compatible subclasses.
            payload, kind = json.dumps(value).encode("utf-8"), 1
    spans, keepalive = _tag_array([] if tags is None else tags)
    options = plsm_feed_options_t(c.sizeof(plsm_feed_options_t), int(durability), spans, len(keepalive))
    with pool._lock:
        _require_open(pool._ptr, "pool")
        job = _NetworkJob(
            _LIB.plsm_internal_async_http_feed,
            pool._ptr, kind, payload, len(payload), c.byref(options),
        )
    try:
        return await asyncio.shield(job.future)
    finally:
        job.close()


def _poll(reader):
    try:
        return reader.next(0)
    except StopIteration:
        return _END


_END = object()


class _ReaderState:
    def __init__(self):
        self.signal = None
        self.opening = None
        self.closing = None
        self.closed = False
        self.network_open = None
        self.network_next = None
        self.network_reader = None

    def close(self):
        if not self.closed:
            self.closed = True
            if self.signal is not None:
                self.signal.close()
            if self.network_open is not None:
                self.network_open.close()
                if self.network_reader is None and self.network_open.future.done():
                    try:
                        self.network_reader = self.network_open.future.result()
                    except Exception:
                        pass
            if self.network_next is not None:
                self.network_next.close()
            if self.network_reader is not None:
                self.network_reader.close()
            if self.opening is not None:
                self.closing = Future()
                self.opening.add_done_callback(self.schedule_release)
        return self.closing

    def schedule_release(self, opening):
        try:
            reader = opening.result()
        except Exception:
            self.closing.set_result(None)
            return
        try:
            _workers.submit(self.release, reader)
        except RuntimeError:
            # The executor can shut down before interpreter finalizers run.
            self.closing.set_result(None)

    def release(self, reader):
        try:
            reader.close()
        except Exception as error:
            self.closing.set_exception(error)
        else:
            self.closing.set_result(None)


class _WaitExpired(Exception):
    pass


async def _wait(future, deadline):
    if deadline is None:
        return await asyncio.shield(future)
    remaining = (deadline - monotonic_ns()) / 1e9
    if future.done():
        return future.result()
    if remaining > 0:
        done, _ = await asyncio.wait((future,), timeout=remaining)
        if done:
            return future.result()
    raise _WaitExpired


class Reader:
    """Open on first use and allow one outstanding read."""

    def __init__(self, pool, live, since, last, tags):
        self._pool = pool
        self._live = live
        self._options = since, last, tags
        self._state = _ReaderState()
        self._opening = None
        self._pending = None
        self._reading = False
        self._finalize = weakref.finalize(self, self._state.close)

    async def _open(self, deadline):
        state = self._state
        if self._opening is None:
            pool = await self._pool._get_pool(deadline)
            if state.closed:
                return None
        if self._opening is None:
            if self._pool._remote:
                state.network_open = _open_network(
                    pool, self._pool._udp, self._live, *self._options
                )
                self._opening = state.network_open.future
            else:
                if self._live:
                    state.signal = _LocalSignal()
                token = 0 if state.signal is None else state.signal.token
                state.opening = _open_workers.submit(_open_reader, pool, self._live, *self._options, token)
                self._opening = asyncio.wrap_future(state.opening)
        reader = await _wait(self._opening, deadline)
        if self._pool._remote and not state.closed:
            state.network_reader = reader
        return reader

    async def next(self, wait=None):
        """Return a message, or None if opening or reading exceeds the wait."""
        if self._reading:
            raise UsageError(ErrorKind.USAGE, "a read is already in progress")
        self._reading = True
        try:
            return await self._next(_wait_deadline(wait))
        except _WaitExpired:
            return None
        finally:
            self._reading = False

    async def _next(self, deadline):
        state = self._state
        if state.closed:
            raise StopAsyncIteration
        reader = await self._open(deadline)
        if self._pool._remote:
            return await self._next_network(reader, deadline)
        while not state.closed:
            if state.signal is not None and state.signal.removed:
                raise NotFoundError(ErrorKind.NOT_FOUND, "pool was removed or replaced")
            if self._pending is None:
                if state.signal is not None:
                    state.signal.prepare()
                self._pending = asyncio.wrap_future(_workers.submit(_poll, reader))
            try:
                result = await _wait(self._pending, deadline)
            except (_WaitExpired, asyncio.CancelledError):
                raise
            except Exception:
                self._pending = None
                raise
            self._pending = None
            if state.closed:
                break
            if result is _END:
                state.close()
                break
            if result is not None:
                return result
            if deadline is None:
                await state.signal.changed.wait()
            else:
                try:
                    await asyncio.wait_for(
                        state.signal.changed.wait(), max(0, (deadline - monotonic_ns()) / 1e9)
                    )
                except asyncio.TimeoutError:
                    return None
        raise StopAsyncIteration

    async def _next_network(self, reader, deadline):
        state = self._state
        if state.closed:
            raise StopAsyncIteration
        if state.network_next is None:
            state.network_next = _NetworkJob(_LIB.plsm_internal_async_network_next, reader.address)
        job = state.network_next
        try:
            result = await _wait(job.future, deadline)
        except (_WaitExpired, asyncio.CancelledError):
            raise
        except Exception:
            state.network_next = None
            raise
        state.network_next = None
        if state.closed:
            raise StopAsyncIteration
        if result is _END:
            state.close()
            raise StopAsyncIteration
        return result

    def __aiter__(self):
        return self

    async def __anext__(self):
        return await self.next()

    async def __aenter__(self):
        if self._state.closed:
            raise UsageError(ErrorKind.USAGE, "reader is closed")
        await self._open(None)
        return self

    async def __aexit__(self, *_):
        await self.aclose()

    async def aclose(self):
        state = self._state
        closing = state.close()
        if state.signal is not None:
            state.signal.changed.set()
        if closing is not None:
            await asyncio.shield(asyncio.wrap_future(closing))
        for job in (state.network_open, state.network_next):
            if job is not None and not job.future.done():
                await asyncio.shield(job.future)


def open_pool(pool_ref, *, create=False):
    return Client().open_pool(pool_ref, create=create)


def create_pool(pool_ref, size_bytes=DEFAULT_POOL_SIZE_BYTES, **description):
    return Client().create_pool(pool_ref, size_bytes, **description)


async def list_pools():
    async with Client() as client:
        return await client.list_pools()


async def delete_pool(pool_ref):
    async with Client() as client:
        await client.delete_pool(pool_ref)


__all__ = [
    "Client", "UdpClient", "Pool", "Reader", "open_pool", "create_pool", "list_pools",
    "delete_pool", "at",
]
