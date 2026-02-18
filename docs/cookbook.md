# Plasmite Cookbook

## Contents

- [Produce & Consume](#produce--consume)
- [CI Gate](#ci-gate)
- [Live Build Progress](#live-build-progress)
- [System Log Intake](#system-log-intake)
- [Polyglot Service Stitching](#polyglot-service-stitching)
- [Multi-Writer Event Bus](#multi-writer-event-bus)
- [Replay & Debug](#replay--debug)
- [Remote Pool Access](#remote-pool-access)
- [Ingest an API Event Stream](#ingest-an-api-event-stream)
- [When Plasmite Isn't the Right Fit](#when-plasmite-isnt-the-right-fit)
- [Next Steps](#next-steps)

---

## Produce & Consume

The core pattern — everything else in this cookbook is a variation.

**Terminal 1** — write a message:

```bash
pls feed work --create '{"task": "resize", "id": 1}'
```

**Terminal 2** — read it (and keep listening):

```bash
pls follow work
```

<details>
<summary><strong>Python · Node · Go</strong></summary>

**Python — produce**

```python
import json
from plasmite import Client, Durability

with Client() as c:
    pool = c.create_pool("work")
    pool.append({"task": "resize", "id": 1})
    pool.close()
```

**Python — consume**

```python
from plasmite import Client, parse_message

with Client() as c, c.open_pool("work") as pool:
    for raw in pool.tail(timeout_ms=5000):
        print(parse_message(raw))
```

**Node — produce**

```js
const { Client, Durability } = require("plasmite");
const c = new Client();
const pool = c.createPool("work");
pool.append({ task: "resize", id: 1 });
pool.close(); c.close();
```

**Node — consume**

```js
const { Client, parseMessage } = require("plasmite");
const c = new Client();
const pool = c.openPool("work");
const stream = pool.openStream(null, null, 5000);
for (const msg of stream) console.log(parseMessage(msg));
stream.close(); pool.close(); c.close();
```

**Go — produce**

```go
c, _ := plasmite.NewDefaultClient()
p, _ := c.CreatePool(plasmite.PoolRefName("work"), plasmite.DefaultPoolSize)
p.Append(map[string]any{"task": "resize", "id": 1}, nil, plasmite.DurabilityFast)
p.Close(); c.Close()
```

**Go — consume**

```go
c, _ := plasmite.NewDefaultClient()
p, _ := c.OpenPool(plasmite.PoolRefName("work"))
out, errs := p.Tail(ctx, plasmite.TailOptions{Timeout: 5 * time.Second})
for msg := range out { fmt.Println(string(msg)) }
if err := <-errs; err != nil { log.Fatal(err) }
p.Close(); c.Close()
```

</details>

---

## CI Gate

Block a deploy script until your test runner signals "green".

**The scenario:** Two scripts coordinate through a pool. The deploy script waits; the test runner feeds when it's done.

```bash
# deploy.sh — block until tests pass
pls follow ci --where '.data.status == "green"' --one > /dev/null
echo "Tests passed — deploying..."
./deploy-to-staging.sh

# test-runner.sh — signal when done (--create makes the pool if needed)
./run-tests.sh
pls feed ci --create '{"status": "green", "commit": "a1b2c3d", "suite": "unit"}'
```

No polling loops, no lock files, no shared database. `--one` exits as soon as a match arrives.

---

## Live Build Progress

Write structured progress from a build script; follow it in real time from another terminal.

**Terminal 1** — the build:

```bash
pls feed build --create '{"step": "compile", "pct": 0}'
sleep 1
pls feed build '{"step": "compile", "pct": 100}'
pls feed build '{"step": "test", "pct": 0}'
sleep 2
pls feed build '{"step": "test", "pct": 100}'
pls feed build --tag done '{"step": "finished", "ok": true}'
```

**Terminal 2** — following:

```bash
pls follow build
```

You'll see each message stream in as it's written. To wait for completion:

```bash
pls follow build --tag done --one
```

<details>
<summary><strong>Python · Node · Go</strong></summary>

**Python — writer**

```python
import json
from plasmite import Client, Durability

with Client() as c:
    pool = c.create_pool("build")
    for step, pct in [("compile", 0), ("compile", 100), ("test", 0), ("test", 100)]:
        pool.append({"step": step, "pct": pct})
    pool.append({"step": "finished", "ok": True}, ["done"], Durability.FAST)
    pool.close()
```

**Python — follower**

```python
from plasmite import Client, parse_message

with Client() as c, c.open_pool("build") as pool:
    for raw in pool.tail(timeout_ms=5000, tags=["done"]):
        print(parse_message(raw))
        break  # --one equivalent
```

**Node — writer**

```js
const { Client, Durability } = require("plasmite");
const c = new Client();
const pool = c.createPool("build");
for (const [step, pct] of [["compile",0],["compile",100],["test",0],["test",100]])
  pool.append({ step, pct });
pool.append({ step: "finished", ok: true }, ["done"]);
pool.close(); c.close();
```

**Go — writer**

```go
c, _ := plasmite.NewDefaultClient()
p, _ := c.CreatePool(plasmite.PoolRefName("build"), plasmite.DefaultPoolSize)
for _, s := range [][2]any{{"compile",0},{"compile",100},{"test",0},{"test",100}} {
    p.Append(map[string]any{"step": s[0], "pct": s[1]}, nil, plasmite.DurabilityFast)
}
p.Append(map[string]any{"step": "finished", "ok": true}, []string{"done"}, plasmite.DurabilityFast)
p.Close(); c.Close()
```

</details>

---

## System Log Intake

Pipe structured system logs into a bounded pool. Replay them later for debugging.

```bash
# Linux — journald
journalctl -o json-seq -f | pls feed syslog --create

# macOS — unified log
/usr/bin/log stream --style ndjson | pls feed syslog --create
```

The pool is a ring buffer (default 4 MB), so it won't fill your disk. To create a bigger buffer:

```bash
pls pool create syslog --size 8M
journalctl -o json-seq -f | pls feed syslog
```

Replay the last 30 minutes:

```bash
pls follow syslog --since 30m --replay 1
```

---

## Polyglot Service Stitching

A Python producer feeds work items into a pool. A Go consumer processes them.

**CLI version** — no bindings needed:

```bash
# producer.py
for i in $(seq 1 5); do
  pls feed jobs --create "{\"task\": \"resize-image\", \"id\": $i}"
done

# consumer (any language, any terminal)
pls follow jobs
```

**With native bindings:**

Python producer:

```python
from plasmite import Client, Durability

client = Client()
pool = client.create_pool("jobs")

for i in range(5):
    pool.append({"task": "resize-image", "id": i}, ["img"])
```

Node producer:

```js
const { Client, Durability } = require("plasmite");
const c = new Client();
const pool = c.openPool("jobs");
for (let i = 0; i < 5; i++)
  pool.append({ task: "resize-image", id: i }, ["img"]);
pool.close(); c.close();
```

Go consumer:

```go
client, _ := plasmite.NewDefaultClient()
defer client.Close()

pool, _ := client.OpenPool(plasmite.PoolRefName("jobs"))
defer pool.Close()

out, errs := pool.Tail(ctx, plasmite.TailOptions{Timeout: 5 * time.Second})
for msg := range out {
    fmt.Println(string(msg))
}
if err := <-errs; err != nil { log.Fatal(err) }
```

All processes hit the same on-disk pool — no serialization adapter, no broker.

---

## Multi-Writer Event Bus

Several scripts publish tagged events to one pool. A reader filters by tag.

**Writers** (run from different scripts or cron jobs):

```bash
pls feed events --create --tag deploy '{"service": "api", "sha": "f4e5d6c"}'
pls feed events --tag alert '{"service": "api", "msg": "latency spike"}'
pls feed events --tag metric '{"service": "web", "rps": 1420}'
```

**Reader** — show only alerts:

```bash
pls follow events --tag alert
```

Combine tags with `--where` for finer filtering:

```bash
pls follow events --tag alert --where '.data.service == "api"'
```

<details>
<summary><strong>Python · Node · Go</strong></summary>

**Python — write tagged events**

```python
import json
from plasmite import Client, Durability

with Client() as c:
    pool = c.create_pool("events")
    pool.append({"service": "api", "sha": "f4e5d6c"}, ["deploy"], Durability.FAST)
    pool.append({"service": "api", "msg": "latency spike"}, ["alert"], Durability.FAST)
    pool.close()
```

**Python — filter by tag**

```python
from plasmite import Client, parse_message

with Client() as c, c.open_pool("events") as pool:
    for raw in pool.tail(timeout_ms=5000, tags=["alert"]):
        print(parse_message(raw))
```

**Node — write tagged events**

```js
const { Client, Durability } = require("plasmite");
const c = new Client();
const pool = c.createPool("events");
pool.append({ service: "api", sha: "f4e5d6c" }, ["deploy"]);
pool.append({ service: "api", msg: "latency spike" }, ["alert"]);
pool.close(); c.close();
```

**Go — write tagged events**

```go
c, _ := plasmite.NewDefaultClient()
p, _ := c.CreatePool(plasmite.PoolRefName("events"), plasmite.DefaultPoolSize)
p.Append(map[string]any{"service": "api", "sha": "f4e5d6c"}, []string{"deploy"}, plasmite.DurabilityFast)
p.Append(map[string]any{"service": "api", "msg": "latency spike"}, []string{"alert"}, plasmite.DurabilityFast)
p.Close(); c.Close()
```

**Go — filter by tag**

```go
c, _ := plasmite.NewDefaultClient()
p, _ := c.OpenPool(plasmite.PoolRefName("events"))
out, errs := p.Tail(ctx, plasmite.TailOptions{Tags: []string{"alert"}, Timeout: 5 * time.Second})
for msg := range out { fmt.Println(string(msg)) }
if err := <-errs; err != nil { log.Fatal(err) }
p.Close(); c.Close()
```

</details>

---

## Replay & Debug

Replay recent messages at speed, filter by time range, and export to a file.

```bash
# Replay the last hour at 10× real-time speed
pls follow incidents --since 1h --replay 10

# Replay at original speed (1×)
pls follow incidents --since 1h --replay 1

# Show only the last 20 messages
pls follow incidents --tail 20

# Export recent errors to a file for sharing
pls follow incidents --tag error --tail 100 --jsonl > /tmp/errors.jsonl
```

Combine `--since`, `--tag`, and `--where` to narrow down exactly what you need:

```bash
pls follow incidents --since 2h --tag sev1 --where '.data.code == 503'
```

---

## Remote Pool Access

Expose your local pools over HTTP so another machine can read and write.

**On the server:**

```bash
# Local-only (loopback) — good for same-machine tools
pls serve

# LAN-accessible — bootstraps TLS + bearer token
pls serve init
```

**On the client** — same CLI, just pass a URL:

```bash
pls feed http://server:9700/events '{"sensor": "temp", "value": 23.5}'
pls follow http://server:9700/events --tail 20
```

A built-in web UI is available at `http://server:9700/ui`.

### Browser page served separately (CORS)

If your browser app is hosted on another origin (for example `https://demo.wratify.ai`), configure `pls serve` with an explicit allowlist:

```bash
pls serve \
  --bind 0.0.0.0:9100 \
  --allow-non-loopback \
  --access read-only \
  --cors-origin https://demo.wratify.ai
```

Then your page can:
- List pools with `GET /v0/ui/pools`
- Stream one pool with `GET /v0/ui/pools/<pool>/events`

## Cookbook Golden Checks

The following sections are covered by `scripts/cookbook_smoke.sh` and enforced in
`just ci-fast`:

- Produce & Consume
- CI Gate
- Live Build Progress
- Multi-Writer Event Bus
- Replay & Debug
- Remote Pool Access

Non-gated sections in this pass:

- System Log Intake
- Ingest an API Event Stream
- Polyglot Service Stitching
- When Plasmite Isn't the Right Fit
- Next Steps

Operational notes:
- For an HTTPS page, use HTTPS on the pool endpoint too (browser mixed-content rules).
- `--cors-origin` is exact-match only and repeatable for multiple origins.
- If you require bearer auth, avoid putting long-lived tokens in public frontend code.
- See `docs/record/serving.md` for complete deployment and troubleshooting guidance.

---

## Ingest an API Event Stream

Pipe a streaming HTTP response directly into a pool.

```bash
curl -N https://api.example.com/events | pls feed api-events --create
```

Then tail it from another terminal:

```bash
pls follow api-events
```

Or filter for specific events as they arrive:

```bash
pls follow api-events --where '.data.type == "payment.completed"'
```

---

## When Plasmite Isn't the Right Fit

Plasmite is great for local and small-team IPC, but it's not the answer to everything.

| If you need… | Consider instead |
|---|---|
| **Multi-host cluster replication** | Kafka, NATS JetStream, or Redpanda. Plasmite pools live on one filesystem. |
| **Schema registry / contract enforcement** | Confluent Schema Registry, Buf. Plasmite is schema-free by design. |
| **Server-side workflow orchestration** | Temporal, Inngest. Plasmite has no built-in retries, sagas, or state machines. |
| **Lowest-latency in-process channels** | OS pipes, `crossbeam`, Go channels. Plasmite's disk persistence adds overhead you don't need for thread-to-thread comms. |
| **Durable storage for large blobs** | S3, MinIO. Pool messages are meant to be small JSON; the ring buffer is bounded. |

---

## Next Steps

- **Rust API spec**: [spec/api/v0/SPEC.md](../spec/api/v0/SPEC.md)
- **Go bindings**: [bindings/go/README.md](../bindings/go/README.md)
- **Node bindings**: [../bindings/node/README.md](../bindings/node/README.md)
- **CLI spec**: [../spec/v0/SPEC.md](../spec/v0/SPEC.md)
- **Pattern matching & filtering**: [spec/v0/SPEC.md § follow](../spec/v0/SPEC.md)
- **README**: [../README.md](../README.md)
