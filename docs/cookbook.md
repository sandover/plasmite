# Plasmite Cookbook

## Contents

- [CI Gate](#ci-gate)
- [Live Event Stream](#live-event-stream)
- [Process Capture with tap](#process-capture-with-tap)
- [Duplex Chat](#duplex-chat)
- [System Log Ring Buffer](#system-log-ring-buffer)
- [Replay & Debug](#replay--debug)
- [Remote Pool Access](#remote-pool-access)
- [Detect Retention Gaps](#detect-retention-gaps)
- [MCP Agent Access](#mcp-agent-access)
- [When Plasmite Isn't the Right Fit](#when-plasmite-isnt-the-right-fit)
- [Next Steps](#next-steps)

---

## CI Gate

A deploy script needs to wait until the test runner says "green". No polling loops, no lock files, no shared database.

```bash
# deploy.sh — blocks until tests pass
pls follow ci --where '.data.status == "green"' --one > /dev/null
echo "Tests passed — deploying..."
./deploy-to-staging.sh

# test-runner.sh — signals when done (--create makes the pool if needed)
./run-tests.sh
pls feed ci --create '{"status": "green", "commit": "a1b2c3d", "suite": "unit"}'
```

`--one` exits as soon as a matching message arrives. The deploy script blocks with zero CPU until that happens.

<details>
<summary><strong>Python · Node · Go</strong></summary>

**Python — wait for green**

```python
from plasmite import Client

with Client() as c, c.open_pool("ci") as pool:
    for msg in pool.tail(timeout_ms=30000):
        if msg.data.get("status") == "green":
            print(f"commit {msg.data['commit']} passed — deploying")
            break
```

**Python — signal green**

```python
from plasmite import Client

with Client() as c, c.pool("ci") as pool:
    pool.append({"status": "green", "commit": "a1b2c3d", "suite": "unit"})
```

**Node — wait for green**

```js
const { Client } = require("plasmite");
(async () => {
  const c = new Client();
  let pool;
  try {
    pool = c.openPool("ci");
    for await (const msg of pool.tail({ timeoutMs: 30000 })) {
      if (msg.data.status === "green") {
        console.log(`commit ${msg.data.commit} passed — deploying`);
        break;
      }
    }
  } finally {
    if (pool) pool.close();
    c.close();
  }
})();
```

**Go — wait for green**

```go
c, _ := plasmite.NewDefaultClient()
p, _ := c.OpenPool(plasmite.PoolRefName("ci"))
out, errs := p.Tail(ctx, plasmite.TailOptions{Timeout: 30 * time.Second})
for msg := range out {
    var d map[string]any
    json.Unmarshal(msg.Data, &d)
    if d["status"] == "green" {
        fmt.Printf("commit %s passed — deploying\n", d["commit"])
        break
    }
}
if err := <-errs; err != nil { log.Fatal(err) }
p.Close(); c.Close()
```

</details>

---

## Live Event Stream

A Raspberry Pi pushes sensor readings every second. A deploy bot and an alerting cron job also write to the same pool. Tags separate the concerns; readers pick what they care about.

### Sensor readings

```bash
# on the Pi — feed readings every second
while true; do
  pls feed telemetry --create \
    --tag sensor \
    "{\"sensor\": \"temp\", \"value\": $(cat /sys/class/thermal/thermal_zone0/temp), \"ts\": \"$(date -Iseconds)\"}"
  sleep 1
done

# on a laptop — alert when the CPU thermal zone exceeds 80°C
pls follow telemetry --tag sensor --where '.data.value > 80000'

# replay the last hour of readings to see the trend
pls follow telemetry --tag sensor --since 1h --replay 0 \
  | jq '.data | [.ts, .value]'
```

### Multi-writer event bus

Several processes write to the same pool with different tags. An on-call engineer tails only what matters.

```bash
pls feed telemetry --tag deploy '{"service": "api", "sha": "f4e5d6c"}'
pls feed telemetry --tag alert  '{"service": "api", "msg": "latency spike"}'
pls feed telemetry --tag metric '{"service": "web", "rps": 1420}'

# on-call — show only api alerts
pls follow telemetry --tag alert --where '.data.service == "api"'

# postmortem — what happened in the 10 minutes before the alert?
pls follow telemetry --since 10m --replay 0 --jsonl > tmp/timeline.jsonl
```

### Ingest an external API stream

A streaming API is fire-and-forget: if nothing is listening, the data is lost. Pipe it into a pool and it sticks around. The ring buffer keeps disk usage bounded.

```bash
# capture Stripe's event stream into a pool
curl -N https://api.stripe.com/v1/events \
  -H "Authorization: Bearer $STRIPE_KEY" \
  | pls feed stripe-events --create

# in another terminal — filter for completed payments as they arrive
pls follow stripe-events --where '.data.type == "payment_intent.succeeded"'

# something went wrong 20 minutes ago — replay and investigate
pls follow stripe-events --since 20m --replay 1

# export the last 500 events for offline analysis
pls follow stripe-events --tail 500 --jsonl > tmp/stripe-dump.jsonl
```

### Build progress

A CI build prints to stdout, but stdout is gone when the terminal closes. Write structured progress to a pool instead and it's available to any process, anytime.

```bash
pls feed build --create '{"step": "compile", "pct": 0}'
sleep 1
pls feed build '{"step": "compile", "pct": 100}'
pls feed build '{"step": "test", "pct": 0}'
sleep 2
pls feed build '{"step": "test", "pct": 100}'
pls feed build --tag done '{"step": "finished", "ok": true}'

# another terminal — watch the build live
pls follow build

# a deploy script — block until done, then ship it
pls follow build --tag done --one > /dev/null && ./deploy.sh

# next morning — what happened overnight?
pls follow build --since 12h --replay 0
```

<details>
<summary><strong>Python · Node · Go</strong></summary>

**Python — produce tagged events**

```python
from plasmite import Client, Durability

with Client() as c, c.pool("telemetry") as pool:
    pool.append({"sensor": "temp", "value": 42100}, ["sensor"], Durability.FAST)
    pool.append({"service": "api", "sha": "f4e5d6c"}, ["deploy"], Durability.FAST)
    pool.append({"service": "api", "msg": "latency spike"}, ["alert"], Durability.FAST)
```

**Python — filter by tag**

```python
from plasmite import Client

with Client() as c, c.open_pool("telemetry") as pool:
    for msg in pool.tail(timeout_ms=5000, tags=["alert"]):
        print(msg.seq, msg.tags, msg.data)
```

**Node — produce tagged events**

```js
const { Client } = require("plasmite");
const c = new Client();
let pool;
try {
  pool = c.pool("telemetry");
  pool.append({ sensor: "temp", value: 42100 }, ["sensor"]);
  pool.append({ service: "api", sha: "f4e5d6c" }, ["deploy"]);
  pool.append({ service: "api", msg: "latency spike" }, ["alert"]);
} finally {
  if (pool) pool.close();
  c.close();
}
```

**Go — produce tagged events**

```go
c, _ := plasmite.NewDefaultClient()
p, _ := c.Pool(plasmite.PoolRefName("telemetry"), 0)
p.Append(map[string]any{"sensor": "temp", "value": 42100}, []string{"sensor"}, plasmite.WithDurability(plasmite.DurabilityFast))
p.Append(map[string]any{"service": "api", "sha": "f4e5d6c"}, []string{"deploy"}, plasmite.WithDurability(plasmite.DurabilityFast))
p.Append(map[string]any{"service": "api", "msg": "latency spike"}, []string{"alert"}, plasmite.WithDurability(plasmite.DurabilityFast))
p.Close(); c.Close()
```

**Go — filter by tag**

```go
c, _ := plasmite.NewDefaultClient()
p, _ := c.OpenPool(plasmite.PoolRefName("telemetry"))
out, errs := p.Tail(ctx, plasmite.TailOptions{Tags: []string{"alert"}, Timeout: 5 * time.Second})
for msg := range out { fmt.Println(msg.Seq, msg.Tags(), string(msg.Data)) }
if err := <-errs; err != nil { log.Fatal(err) }
p.Close(); c.Close()
```

</details>

---

## Process Capture with tap

Use `tap` to wrap an existing command and persist its stdout/stderr as pool messages without changing the wrapped program.

```bash
# capture command output into a pool
pls tap build --create -- cargo build

# in another terminal, watch output live
pls follow build

# replay recent output
pls follow build --since 2h

# filter only stderr lines
pls follow build --where '.data.stream == "stderr"'

# tag captured lines for downstream filters
pls tap deploy --tag prod -- ./deploy.sh
```

For long-running or high-volume commands, choose an explicit pool size so the ring does not overwrite data too quickly:

```bash
pls tap api --create --create-size 64M -- ./server
```

---

## Duplex Chat

`duplex` runs send and follow in one process. Type a line and it's appended; messages from the other side print as they arrive.

### Two-party chat

**Terminal 1** — Alice creates the pool and starts chatting:

```bash
pls duplex chat --create --me alice
```

**Terminal 2** — Bob joins and catches up on the last 20 messages:

```bash
pls duplex chat --me bob --tail 20
```

Each non-empty line typed becomes `{"from": "alice", "msg": "..."}`. By default, the sender's own messages are hidden. Add `--echo-self` to see everything:

```bash
pls duplex chat --me alice --echo-self
```

### Remote duplex

If a server is running (`pls serve`), duplex works over the network too. Same syntax, just pass a URL:

```bash
pls duplex http://server:9700/chat --me alice --tail 10
```

Note: `--create` and `--since` are not supported for remote pools. Use `--tail` to catch up on history.

### Scripted duplex (non-TTY)

When stdin is not a TTY, duplex ingests a JSON stream (like `feed`). The session ends when stdin reaches EOF.

```bash
printf '{"from":"alice","msg":"boot complete"}\n{"from":"alice","msg":"ready"}' \
  | pls duplex chat --me alice
```

Use `--timeout` to bound how long the follow side waits for new messages:

```bash
printf '{"ping": true}' | pls duplex chat --me healthcheck --timeout 5s
```

---

## System Log Ring Buffer

Pipe system logs into a pool. The ring buffer caps disk usage, and anything in the window can be replayed or searched.

```bash
# Linux — journald
journalctl -o json-seq -f | pls feed syslog --create

# macOS — unified log
/usr/bin/log stream --style ndjson | pls feed syslog --create
```

Default pool size is 1 MB. For busier systems, make a bigger buffer:

```bash
pls pool create syslog --size 8M
journalctl -o json-seq -f | pls feed syslog
```

Then, when something crashes:

```bash
# replay the last 30 minutes
pls follow syslog --since 30m --replay 1

# find kernel panics
pls follow syslog --since 1h --where '.data.MESSAGE | test("panic")'

# pipe to jq for further analysis
pls follow syslog --since 10m --replay 0 | jq '.data | {SYSLOG_IDENTIFIER, MESSAGE}'
```

---

## Replay & Debug

Every message in a pool has a sequence number and a nanosecond timestamp, so replaying a time window is a one-liner.

An incident pool has been accumulating events. Something went wrong in the last hour:

```bash
# replay the last hour at 10× real-time speed — watch the incident unfold
pls follow incidents --since 1h --replay 10

# replay at original speed (1×) to see exact timing
pls follow incidents --since 1h --replay 1

# narrow down: only sev1 events with a 503 code
pls follow incidents --since 2h --tag sev1 --where '.data.code == 503'

# show just the last 20 messages
pls follow incidents --tail 20

# export the evidence for a postmortem
mkdir -p tmp
pls follow incidents --tag error --tail 100 --jsonl > tmp/errors.jsonl
```

---

## Remote Pool Access

A machine exposes its local pools over HTTP. Clients on other machines use the same CLI; just pass a URL.

**On the server (secure default):**

```bash
# Generate token + TLS artifacts and keep the printed fingerprint for out-of-band verification
plasmite serve init --bind 0.0.0.0:9700 --host pools.example.com --output-dir ./.plasmite-serve

# Start secure server with generated artifacts
plasmite serve \
  --bind 0.0.0.0:9700 \
  --allow-non-loopback \
  --token-file ./.plasmite-serve/plasmite-auth-token.txt \
  --tls-cert ./.plasmite-serve/plasmite-tls-cert.pem \
  --tls-key ./.plasmite-serve/plasmite-tls-key.pem
```

**On a client** (same CLI, plus auth/trust flags):

```bash
plasmite feed https://server:9700/events \
  --token-file ./.plasmite-serve/plasmite-auth-token.txt \
  --tls-ca ./.plasmite-serve/plasmite-tls-cert.pem \
  '{"sensor": "temp", "value": 23.5}'

plasmite follow https://server:9700/events \
  --token-file ./.plasmite-serve/plasmite-auth-token.txt \
  --tls-ca ./.plasmite-serve/plasmite-tls-cert.pem \
  --tail 20
```

Development-only shortcut when trust bootstrapping is unavailable:

```bash
plasmite follow https://server:9700/events --tail 20 --tls-skip-verify
```

curl remains useful for API debugging, but native `plasmite feed` / `plasmite follow` should be the first-line operator workflow.

A built-in web UI is available at `https://server:9700/ui`.

---

## Detect Retention Gaps

A pool is a fixed-size ring buffer. If writers fill it faster than a consumer
advances, old messages disappear. Normal SDK tails continue from the next
retained message so existing best-effort consumers keep moving.

Consumers that require a complete sequence can opt into a fail-closed check:

```python
from plasmite import RetentionGapError

try:
    for message in pool.tail(
        since_seq=checkpoint,
        error_on_gap=True,
        timeout_ms=30_000,
    ):
        process(message)
        checkpoint = message.seq + 1
except RetentionGapError as error:
    print(f"first missing sequence: {error.seq}")
    rebuild_state_before_restarting()
```

The error occurs before the first message after the gap, including when tag
filters would discard that message. The consumer remains responsible for
choosing a new checkpoint or rebuilding state. This check detects loss; it
does not add acknowledgements or prevent the ring buffer from overwriting old
data.

Local decoded and Lite3 tails support the option. Remote JSON tails support it;
remote Lite3 tails do not because their wire format has no terminal error
frame.

## MCP Agent Access

Plasmite can also be used as an MCP server for agent harnesses. A remote MCP
client does not need the Plasmite CLI installed.

At initialization, the server explains the core model:

- a pool is a named, persistent, fixed-size stream of JSON messages;
- feeding appends a message; it never updates an existing message;
- pools are ring buffers, so old messages are overwritten as capacity fills;
- messages contain `data`, optional `tags`, time, and an automatic sequence
  number that most users can ignore;
- `plasmite_pool_list` is the normal first tool when the target pool is
  unknown;
- `plasmite_read` inspects recent or historical messages, while
  `plasmite_wait` waits once for future messages.

Tool discovery reflects the server's access mode, so a read-only or write-only
server exposes only the operations the agent can actually call. Existing pools
also appear as MCP resources; reading one returns up to the latest 20 messages.
Tag filters require every specified tag. MCP does not currently expose jq
`where` filtering.

`plasmite_feed` is an append, not an upsert. If its transport fails after an
ambiguous response, retrying can append a duplicate; use an application-level
stable identifier when retries must be safe.

Creating a pool that already exists returns `AlreadyExists` without changing
the pool or its messages. Use the existing pool as-is unless the task explicitly
requires discarding it.

### Local MCP server (stdio)

```json
{
  "mcpServers": {
    "plasmite": {
      "command": "plasmite",
      "args": ["mcp", "--dir", "/path/to/pools"]
    }
  }
}
```

### Remote MCP server (`/mcp`)

```json
{
  "mcpServers": {
    "plasmite-remote": {
      "type": "streamable-http",
      "url": "https://server:9700/mcp"
    }
  }
}
```

Remote MCP uses the same auth/TLS posture as `plasmite serve`:
- if server auth is enabled, clients send the same bearer token;
- if TLS is enabled, clients trust the same certificate/CA material.

### Mac host to Windows VM: a tiny MCP bridge

VMware's host-only network is useful for agent coordination: the guest can
reach the host, but the listener does not need to be exposed to the wider
network. Run a second Plasmite server on the Mac's host-only address; keep the
ordinary loopback server separate.

**On the Mac host:**

```bash
# Use the Mac address visible to the VM, not 0.0.0.0.
HOST_VMNET_IP=192.168.175.1
TOKEN_DIR="$HOME/.config/plasmite/codex-bridge"

plasmite serve init --token-only --output-dir "$TOKEN_DIR"

plasmite serve \
  --bind "$HOST_VMNET_IP:9700" \
  --allow-non-loopback \
  --insecure-no-tls \
  --token-file "$TOKEN_DIR/plasmite-auth-token.txt"
```

The token is still required: the private network limits where traffic can go,
while the token limits who can use Plasmite. Plaintext is acceptable here only
because the listener is bound to the host-only interface. Do not use this
recipe with `0.0.0.0` or a public interface.

Copy the token to the Windows user through an already trusted host/guest path.
Store it as a user environment variable without placing the value in the Codex
configuration:

```powershell
$token = (Get-Content .\plasmite-auth-token.txt -Raw).Trim()
[Environment]::SetEnvironmentVariable("PLASMITE_MCP_TOKEN", $token, "User")
Remove-Item .\plasmite-auth-token.txt
```

Start a new Windows shell, then register the HTTP MCP endpoint once:

```powershell
Invoke-RestMethod http://192.168.175.1:9700/healthz

codex mcp add plasmite-host `
  --url http://192.168.175.1:9700/mcp `
  --bearer-token-env-var PLASMITE_MCP_TOKEN

codex mcp list
```

Future Codex sessions for that Windows user inherit the MCP server. Agents can
list, read, wait, and write pools through the normal Plasmite MCP tools; no
per-pool setup is required.

Run the host command under a service manager for persistence. To rotate access,
replace the host token, update `PLASMITE_MCP_TOKEN` in Windows, and restart the
server and Codex. To remove access:

```powershell
codex mcp remove plasmite-host
[Environment]::SetEnvironmentVariable("PLASMITE_MCP_TOKEN", $null, "User")
```

### Waiting and polling

MCP tools are request/response, so each wait returns one batch or times out.

For tail-like use, call `plasmite_wait` with a pool and no `after_seq`. It
snapshots the pool's current end and waits only for messages appended
afterward. This cursor-free form is for starting a wait. In a repeated wait
loop, pass each result's `next_after_seq` into the next call as `after_seq`;
otherwise messages appended between calls can be skipped.

Sequence numbers are automatic metadata, not a normal prerequisite. For
advanced resumable polling:

1. Call `plasmite_read` with `pool` and optional filters.
2. Save `next_after_seq` from the result.
3. Call `plasmite_wait` with that `after_seq` cursor for an idle, bounded wait,
   or call `plasmite_read` again for an immediate non-blocking check.
4. Save the returned cursor and repeat. A timed-out wait returns an empty
   message batch with `timed_out: true`.

Supplying `after_seq` lets a reader catch up on messages appended while it was
offline. Omitting it intentionally starts at the live edge.

`plasmite_read` details in v1:
- default `count` is 20, maximum is 200;
- without `after_seq`, it returns the last `count` matching messages (ascending);
- with `after_seq`, it returns messages where `seq > after_seq` (ascending);
- if both `since` and `after_seq` are set, both filters apply (intersection).
- `next_after_seq` is the highest sequence examined, not merely the last
  matching message, so filtered polling does not repeatedly rescan messages;
- `last_returned_seq` is the last matching message, or `null` for an empty
  batch;
- `oldest_available_seq` and `newest_available_seq` report current retention
  bounds;
- `fell_behind: true` means retention overtook the supplied cursor, so the
  returned batch begins at the oldest message still available.

v1 intentionally does not implement MCP resource subscriptions or POST-SSE
mode. `plasmite_wait` is a bounded request/response tool, not a live stream.

### Coordination conventions (experimental, recommended)

MCP tools are generic. For multi-agent coordination, provide explicit conventions in your agent instructions.

Recommended `claims` pattern:

1. Use one shared pool (for example `claims`).
2. Prefer tags over jq for routing:
   - `claim`
   - `agent:<agent-id>`
   - `file:<path>` for each claimed file
3. Claim by feeding an event with current files:
   - `data`: `{"agent":"amp-1","files":["src/auth.rs"]}`
4. Release by feeding an empty files list:
   - `data`: `{"agent":"amp-1","files":[]}`
5. Readers treat claims as leases by time window (for example `since: "10m"`), then reconstruct latest claim per agent client-side.

Important v1 limits:
- No server TTL/auto-expiry for stale claims.
- No `latest_by` read primitive yet; state reconstruction is client-side.
- No atomic claim operation; read-then-write races are possible, so claims are advisory.

### CI split pattern (writer outside MCP, reader via MCP)

For CI status, a simple split works well:
- CI pipeline writes with CLI/API (`plasmite feed` or HTTP `/v0/pools/.../messages`).
- Agents read with MCP (`plasmite_read`, `count: 1`, optional `after_seq` polling).

### Browser page served separately (CORS)

If a browser app is hosted on another origin (for example `https://demo.wratify.ai`), configure `pls serve` with an explicit allowlist:

```bash
pls serve \
  --bind 0.0.0.0:9700 \
  --allow-non-loopback \
  --access read-only \
  --cors-origin https://demo.wratify.ai
```

Then the page can:
- List pools with `GET /v0/ui/pools`
- Stream one pool with `GET /v0/ui/pools/<pool>/events`

## Cookbook Golden Checks

The following sections are covered by `scripts/cookbook_smoke.sh` and enforced in
`just integration`:

- CI Gate
- Live Event Stream (build progress, multi-writer event bus)
- Replay & Debug
- Remote Pool Access

Non-gated sections in this pass:

- Duplex Chat
- System Log Ring Buffer
- When Plasmite Isn't the Right Fit
- Next Steps

Operational notes:
- For an HTTPS page, use HTTPS on the pool endpoint too (browser mixed-content rules).
- `--cors-origin` is exact-match only and repeatable for multiple origins.
- If bearer auth is required, avoid putting long-lived tokens in public frontend code.
- See `docs/record/serving.md` for complete deployment and troubleshooting guidance.

---

## When Plasmite Isn't the Right Fit

Plasmite is great for local and small-team IPC, but it's not the answer to everything.

| If you need… | Consider instead |
|---|---|
| **Multi-host cluster replication** | Kafka, NATS JetStream, or Redpanda. Plasmite pools live on one filesystem. |
| **Schema registry / contract enforcement** | Confluent Schema Registry, Buf. Plasmite is schema-free by design. |
| **Server-side workflow orchestration** | Temporal, Inngest. Plasmite has no built-in retries, sagas, or state machines. |
| **Lowest-latency in-process channels** | OS pipes, `crossbeam`, Go channels. Plasmite's disk persistence adds overhead not needed for thread-to-thread comms. |
| **Durable storage for large blobs** | S3, MinIO. Pool messages are meant to be small JSON; the ring buffer is bounded. |

---

## Next Steps

- **Rust API spec**: [spec/api/v0/SPEC.md](../spec/api/v0/SPEC.md)
- **Go bindings**: [bindings/go/README.md](../bindings/go/README.md)
- **Node bindings**: [../bindings/node/README.md](../bindings/node/README.md)
- **CLI spec**: [../spec/v0/SPEC.md](../spec/v0/SPEC.md)
- **Pattern matching & filtering**: [Live Event Stream](#live-event-stream)
- **README**: [../README.md](../README.md)
