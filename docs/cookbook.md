# Plasmite Cookbook

## Contents

- [CI Gate](#ci-gate)
- [Live Event Stream](#live-event-stream)
- [Process Capture with tap](#process-capture-with-tap)
- [Duplex Chat](#duplex-chat)
- [System Log Ring Buffer](#system-log-ring-buffer)
- [Replay & Debug](#replay--debug)
- [Remote Pool Access](#remote-pool-access)
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
plasmite serve init --bind 0.0.0.0:9700 --output-dir ./.plasmite-serve

# Start secure server with generated artifacts
plasmite serve \
  --bind 0.0.0.0:9700 \
  --allow-non-loopback \
  --token-file ./.plasmite-serve/serve-token.txt \
  --tls-cert ./.plasmite-serve/serve-cert.pem \
  --tls-key ./.plasmite-serve/serve-key.pem
```

**On a client** (same CLI, plus auth/trust flags):

```bash
plasmite feed https://server:9700/events \
  --token-file ./.plasmite-serve/serve-token.txt \
  --tls-ca ./.plasmite-serve/serve-cert.pem \
  '{"sensor": "temp", "value": 23.5}'

plasmite follow https://server:9700/events \
  --token-file ./.plasmite-serve/serve-token.txt \
  --tls-ca ./.plasmite-serve/serve-cert.pem \
  --tail 20
```

Development-only shortcut when trust bootstrapping is unavailable:

```bash
plasmite follow https://server:9700/events --tail 20 --tls-skip-verify
```

curl remains useful for API debugging, but native `plasmite feed` / `plasmite follow` should be the first-line operator workflow.

A built-in web UI is available at `https://server:9700/ui`.

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
`just ci-fast`:

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
- **Pattern matching & filtering**: [spec/v0/SPEC.md § follow](../spec/v0/SPEC.md)
- **README**: [../README.md](../README.md)
