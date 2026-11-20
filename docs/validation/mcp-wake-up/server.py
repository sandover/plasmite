import json
import os
import sys
from mcp.server.mcpserver import Context, MCPServer

async def trace(ctx, call_next):
    print(json.dumps({"method":ctx.method, "params":ctx.params}, default=str), file=sys.stderr, flush=True)
    return await call_next(ctx)

mcp = MCPServer("Disposable pool", middleware=[trace])
seq = {"project": 0, "other": 0}

@mcp.resource("plasmite:///pools/project")
def project() -> str:
    return f"synthetic seq={seq['project']}"

@mcp.resource("plasmite:///pools/other")
def other() -> str:
    return f"synthetic seq={seq['other']}"

@mcp.tool()
async def synthetic_tick(pool: str, ctx: Context) -> int:
    seq[pool] += 1
    await ctx.notify_resource_updated(f"plasmite:///pools/{pool}")
    return seq[pool]

async def traced_app(scope, receive, send):
    async def trace_send(msg):
        if msg["type"] == "http.response.body":
            body = msg.get("body", b"").decode(errors="replace")
            for line in body.splitlines():
                if line.startswith("data: ") and "notifications/" in line:
                    print("SSE " + line[6:], file=sys.stderr, flush=True)
        await send(msg)
    await app(scope, receive, trace_send)

app = mcp.streamable_http_app()

if __name__ == "__main__":
    import uvicorn
    uvicorn.run(traced_app, host="127.0.0.1", port=int(os.getenv("IVQ245_PORT", "8000")))
