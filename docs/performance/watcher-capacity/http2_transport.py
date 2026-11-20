"""An HTTP/2 connection with one socket reader for concurrent benchmark requests."""

import asyncio
from dataclasses import dataclass

from h2.config import H2Configuration
from h2.connection import H2Connection
from h2.events import (
    ConnectionTerminated,
    DataReceived,
    RemoteSettingsChanged,
    ResponseReceived,
    StreamEnded,
    StreamReset,
    WindowUpdated,
)
from h2.exceptions import ProtocolError, StreamClosedError
import httpx


@dataclass
class Reply:
    headers: asyncio.Future
    body: asyncio.Queue

    def fail(self, error):
        if self.headers.done():
            self.body.put_nowait(error)
        else:
            self.headers.set_exception(error)


class Body(httpx.AsyncByteStream):
    def __init__(self, connection, stream_id, queue, timeout):
        self.connection = connection
        self.stream_id = stream_id
        self.queue = queue
        self.timeout = timeout

    async def __aiter__(self):
        while True:
            try:
                item = await asyncio.wait_for(self.queue.get(), timeout=self.timeout)
            except TimeoutError as error:
                raise httpx.ReadTimeout("HTTP/2 response stalled") from error
            if item is None:
                return
            if isinstance(item, Exception):
                raise item
            yield item

    async def aclose(self):
        self.connection.close_stream(self.stream_id)


class Http2Transport(httpx.AsyncBaseTransport):
    def __init__(self, context):
        self.context = context
        self.h2 = H2Connection(config=H2Configuration(header_encoding="utf-8"))
        self.replies = {}
        self.connecting = asyncio.Lock()
        self.writable = asyncio.Event()
        self.pump = self.writer = self.failed = None

    async def connect(self, url):
        async with self.connecting:
            if self.writer is not None:
                return
            reader, writer = await asyncio.wait_for(
                asyncio.open_connection(url.host, url.port or 443, ssl=self.context),
                timeout=15,
            )
            tls = writer.get_extra_info("ssl_object")
            if tls.selected_alpn_protocol() != "h2":
                writer.close()
                await writer.wait_closed()
                raise httpx.ConnectError("server did not negotiate HTTP/2")
            self.writer = writer
            self.h2.initiate_connection()
            self.flush()
            self.pump = asyncio.create_task(self.read_frames(reader))

    def flush(self):
        if data := self.h2.data_to_send():
            self.writer.write(data)

    async def handle_async_request(self, request):
        try:
            await self.connect(request.url)
        except (OSError, TimeoutError) as error:
            raise httpx.ConnectError(str(error)) from error
        if self.failed:
            raise self.failed
        data = await request.aread()
        stream_id = self.h2.get_next_available_stream_id()
        reply = Reply(asyncio.get_running_loop().create_future(), asyncio.Queue())
        self.replies[stream_id] = reply
        try:
            headers = [
                (":method", request.method),
                (":scheme", request.url.scheme),
                (":authority", request.headers["host"]),
                (":path", request.url.raw_path.decode("ascii")),
            ]
            headers.extend(
                (key, value) for key, value in request.headers.multi_items()
                if key not in ("host", "connection", "transfer-encoding")
            )
            self.h2.send_headers(stream_id, headers, end_stream=not data)
            self.flush()
            if data:
                await self.send_body(stream_id, data)
            response_headers = await asyncio.wait_for(reply.headers, timeout=15)
        except TimeoutError as error:
            self.close_stream(stream_id)
            raise httpx.ReadTimeout("HTTP/2 response did not open") from error
        except BaseException:
            self.close_stream(stream_id)
            raise
        status = int(next(value for key, value in response_headers if key == ":status"))
        timeout = request.extensions.get("timeout", {}).get("read")
        return httpx.Response(
            status,
            headers=[(key, value) for key, value in response_headers if not key.startswith(":")],
            stream=Body(self, stream_id, reply.body, timeout),
            extensions={"http_version": b"HTTP/2"},
        )

    async def send_body(self, stream_id, data):
        sent = 0
        while sent < len(data):
            if self.failed:
                raise self.failed
            try:
                window = self.h2.local_flow_control_window(stream_id)
            except StreamClosedError as error:
                raise httpx.RemoteProtocolError("server closed request stream") from error
            size = min(window, self.h2.max_outbound_frame_size, len(data) - sent)
            if size <= 0:
                self.writable.clear()
                await asyncio.wait_for(self.writable.wait(), timeout=15)
                continue
            end = sent + size
            self.h2.send_data(stream_id, data[sent:end], end_stream=end == len(data))
            sent = end
            self.flush()
            await self.writer.drain()

    async def read_frames(self, reader):
        try:
            while data := await reader.read(65536):
                for event in self.h2.receive_data(data):
                    if isinstance(event, ConnectionTerminated):
                        raise httpx.RemoteProtocolError(str(event))
                    if isinstance(event, (WindowUpdated, RemoteSettingsChanged)):
                        self.writable.set()
                    elif isinstance(event, DataReceived):
                        self.h2.acknowledge_received_data(event.flow_controlled_length, event.stream_id)
                        if reply := self.replies.get(event.stream_id):
                            reply.body.put_nowait(event.data)
                    elif isinstance(event, (ResponseReceived, StreamEnded, StreamReset)):
                        reply = self.replies.get(event.stream_id)
                        if reply is None:
                            continue
                        if isinstance(event, ResponseReceived):
                            reply.headers.set_result(event.headers)
                        elif isinstance(event, StreamEnded):
                            reply.body.put_nowait(None)
                        else:
                            reply.fail(httpx.RemoteProtocolError(str(event)))
                            self.writable.set()
                self.flush()
            raise httpx.RemoteProtocolError("HTTP/2 connection closed")
        except (OSError, ProtocolError, httpx.RemoteProtocolError) as error:
            self.failed = httpx.RemoteProtocolError(str(error))
            for reply in self.replies.values():
                reply.fail(self.failed)
            self.writable.set()

    def close_stream(self, stream_id):
        self.replies.pop(stream_id, None)
        if self.failed:
            return
        try:
            self.h2.reset_stream(stream_id)
        except StreamClosedError:
            return
        self.flush()

    async def aclose(self):
        if self.pump:
            self.pump.cancel()
            await asyncio.gather(self.pump, return_exceptions=True)
        if self.writer:
            self.writer.close()
            try:
                await self.writer.wait_closed()
            except OSError:
                pass
