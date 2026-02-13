# Plasmite Cookbook

## Contents

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

## CI Gate

Block a deploy script until your test runner signals "green".

**The scenario:** Two scripts coordinate through a pool. The deploy script waits; the test runner pokes when it's done.

```bash
# deploy.sh — block until tests pass
pls peek ci --where '.data.status == "green"' --one > /dev/null
echo "Tests passed — deploying..."
./deploy-to-staging.sh

# test-runner.sh — signal when done (--create makes the pool if needed)
./run-tests.sh
pls poke ci --create '{"status": "green", "commit": "a1b2c3d", "suite": "unit"}'
```

No polling loops, no lock files, no shared database. `--one` exits as soon as a match arrives.

---

## Live Build Progress

Write structured progress from a build script; watch it in real time from another terminal.

**Terminal 1** — the build:

```bash
pls poke build --create '{"step": "compile", "pct": 0}'
sleep 1
pls poke build '{"step": "compile", "pct": 100}'
pls poke build '{"step": "test", "pct": 0}'
sleep 2
pls poke build '{"step": "test", "pct": 100}'
pls poke build --tag done '{"step": "finished", "ok": true}'
```

**Terminal 2** — watching:

```bash
pls peek build
```

You'll see each message stream in as it's written. To wait for completion:

```bash
pls peek build --tag done --one
```

---

## System Log Intake

Pipe structured system logs into a bounded pool. Replay them later for debugging.

```bash
# Linux — journald
journalctl -o json-seq -f | pls poke syslog --create

# macOS — unified log
/usr/bin/log stream --style ndjson | pls poke syslog --create
```

The pool is a ring buffer (default 1 MB), so it won't fill your disk. To create a bigger buffer:

```bash
pls pool create syslog --size 8M
journalctl -o json-seq -f | pls poke syslog
```

Replay the last 30 minutes:

```bash
pls peek syslog --since 30m --replay 1
```

---

## Polyglot Service Stitching

A Python producer feeds work items into a pool. A Go consumer processes them.

**CLI version** — no bindings needed:

```bash
# producer.py
for i in $(seq 1 5); do
  pls poke jobs --create "{\"task\": \"resize-image\", \"id\": $i}"
done

# consumer (any language, any terminal)
pls peek jobs
```

**With native bindings:**

Python producer:

```python
from plasmite import Client, Durability

client = Client("./data")
pool = client.create_pool("jobs", 4 * 1024 * 1024)

for i in range(5):
    pool.append_json(
        f'{{"task": "resize-image", "id": {i}}}'.encode(),
        ["img"],
        Durability.FAST,
    )
```

Go consumer:

```go
client, _ := plasmite.NewClient("./data")
defer client.Close()

pool, _ := client.OpenPool(plasmite.PoolRefName("jobs"))
defer pool.Close()

out, errs := pool.Tail(ctx, plasmite.TailOptions{Timeout: 5 * time.Second})
for msg := range out {
    fmt.Println(string(msg))
}
if err := <-errs; err != nil { log.Fatal(err) }
```

Both processes hit the same on-disk pool — no serialization adapter, no broker.

---

## Multi-Writer Event Bus

Several scripts publish tagged events to one pool. A reader filters by tag.

**Writers** (run from different scripts or cron jobs):

```bash
pls poke events --create --tag deploy '{"service": "api", "sha": "f4e5d6c"}'
pls poke events --tag alert '{"service": "api", "msg": "latency spike"}'
pls poke events --tag metric '{"service": "web", "rps": 1420}'
```

**Reader** — show only alerts:

```bash
pls peek events --tag alert
```

Combine tags with `--where` for finer filtering:

```bash
pls peek events --tag alert --where '.data.service == "api"'
```

---

## Replay & Debug

Replay recent messages at speed, filter by time range, and export to a file.

```bash
# Replay the last hour at 10× real-time speed
pls peek incidents --since 1h --replay 10

# Replay at original speed (1×)
pls peek incidents --since 1h --replay 1

# Show only the last 20 messages
pls peek incidents --tail 20

# Export recent errors to a file for sharing
pls peek incidents --tag error --tail 100 --jsonl > /tmp/errors.jsonl
```

Combine `--since`, `--tag`, and `--where` to narrow down exactly what you need:

```bash
pls peek incidents --since 2h --tag sev1 --where '.data.code == 503'
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
pls poke http://server:9700/events '{"sensor": "temp", "value": 23.5}'
pls peek http://server:9700/events --tail 20
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

Operational notes:
- For an HTTPS page, use HTTPS on the pool endpoint too (browser mixed-content rules).
- `--cors-origin` is exact-match only and repeatable for multiple origins.
- If you require bearer auth, avoid putting long-lived tokens in public frontend code.
- See `docs/record/browser-cors.md` for complete deployment and troubleshooting guidance.

---

## Ingest an API Event Stream

Pipe a streaming HTTP response directly into a pool.

```bash
curl -N https://api.example.com/events | pls poke api-events --create
```

Then tail it from another terminal:

```bash
pls peek api-events
```

Or filter for specific events as they arrive:

```bash
pls peek api-events --where '.data.type == "payment.completed"'
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

- **Rust API**: [docs/record/api-quickstart.md](record/api-quickstart.md)
- **Go bindings**: [docs/record/go-quickstart.md](record/go-quickstart.md)
- **Python bindings**: [../bindings/python/README.md](../bindings/python/README.md)
- **Node bindings**: [../bindings/node/README.md](../bindings/node/README.md)
- **CLI spec**: [../spec/v0/SPEC.md](../spec/v0/SPEC.md)
- **Pattern matching & filtering**: [record/pattern-matching.md](record/pattern-matching.md)
- **README**: [../README.md](../README.md)
