"""Exercise Plasmite's stateless stdio protocol using the published MCP SDK."""
import argparse
import asyncio
from pathlib import Path
from tempfile import TemporaryDirectory

from mcp import Client, StdioServerParameters


async def run(binary: Path) -> None:
    with TemporaryDirectory() as directory:
        server = StdioServerParameters(
            command=str(binary.resolve()), args=["--dir", directory, "mcp"]
        )
        for mode in ("auto", "2026-07-28"):
            async with Client(server, mode=mode, read_timeout_seconds=5) as client:
                assert client.session.protocol_version == "2026-07-28"
                tools = await client.list_tools()
                assert any(tool.name == "plasmite_feed" for tool in tools.tools)
                pool = f"sdk-{mode}"
                fed = await client.call_tool(
                    "plasmite_feed", {"pool": pool, "create": True, "data": {"proof": mode}}
                )
                assert fed.structured_content["message"]["seq"] == 1
                fetched = await client.call_tool("plasmite_fetch", {"pool": pool})
                assert fetched.structured_content["messages"][0]["data"]["proof"] == mode
                resource = await client.read_resource(f"plasmite:///pools/{pool}")
                assert resource.contents[0].mime_type == "application/json"
                print(f"{mode}: tools/list, feed, fetch and resources/read passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    asyncio.run(run(parser.parse_args().binary))
