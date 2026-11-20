import anyio
import os
from mcp import Client
import httpx2
from mcp.client.streamable_http import streamable_http_client
from mcp.client.subscriptions import ResourceUpdated
from mcp.types import TextResourceContents

URI = "plasmite:///pools/project"
URL = f"http://127.0.0.1:{os.getenv('IVQ245_PORT', '8000')}/mcp"

async def read(client):
    [result] = (await client.read_resource(URI)).contents
    assert isinstance(result, TextResourceContents)
    return result.text

async def main():
    async with Client(streamable_http_client(URL, http_client=httpx2.AsyncClient(trust_env=False))) as client, Client(streamable_http_client(URL, http_client=httpx2.AsyncClient(trust_env=False))) as sender:
        assert await read(client) == "synthetic seq=0"
        async with client.listen(resource_subscriptions=[URI]) as sub:
            await sender.call_tool("synthetic_tick", {"pool":"other"})
            with anyio.move_on_after(0.35) as decoy_wait:
                await anext(aiter(sub))
            assert decoy_wait.cancelled_caught, "Received an update for the unsubscribed pool"
            await sender.call_tool("synthetic_tick", {"pool":"project"})
            with anyio.fail_after(5):
                event = await anext(aiter(sub))
            assert isinstance(event, ResourceUpdated) and event.uri == URI, event
            assert await read(client) == "synthetic seq=1"
            print(f"first received event: {event!r}; read {await read(client)}")
        await sender.call_tool("synthetic_tick", {"pool":"project"})
        async with client.listen(resource_subscriptions=[URI]) as sub:
            assert await read(client) == "synthetic seq=2"
            print("closed-stream update recovered with fresh read: synthetic seq=2")
            with anyio.move_on_after(0.35) as scope:
                replay = await anext(aiter(sub))
            assert scope.cancelled_caught, replay
            print("no old event replayed in fresh subscription")

anyio.run(main)
