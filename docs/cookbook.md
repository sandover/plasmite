# Plasmite cookbook

Short recipes for everyday tasks, from the shell and from Python, JavaScript and Go. The [CLI operating model](cli.md) explains the rules behind them, and the [2.0 contract](../spec/v2/SPEC.md) specifies them.

- [Log events and watch them](#log-events-and-watch-them)
- [Gate a deploy on CI](#gate-a-deploy-on-ci)
- [Capture command output](#capture-command-output)
- [Keep a rolling system log](#keep-a-rolling-system-log)
- [Let several programs write to one pool](#let-several-programs-write-to-one-pool)
- [Read only the messages you need](#read-only-the-messages-you-need)
- [Resume after a restart](#resume-after-a-restart)
- [Save history to a file and replay it](#save-history-to-a-file-and-replay-it)
- [Chat between two terminals](#chat-between-two-terminals)
- [Share a pool with a teammate](#share-a-pool-with-a-teammate)
- [Collect sensor readings over UDP](#collect-sensor-readings-over-udp)
- [Run a collector at boot](#run-a-collector-at-boot)
- [Record TouchOSC messages](#record-touchosc-messages)
- [Give an AI agent your pools](#give-an-ai-agent-your-pools)
- [Use Plasmite from Python, JavaScript and Go](#use-plasmite-from-python-javascript-and-go)
- [When Plasmite isn't the right fit](#when-plasmite-isnt-the-right-fit)

## Log events and watch them

```bash
plasmite feed builds --create '{"step": "compile", "status": "ok"}'
plasmite feed builds '{"step": "test", "status": "failed"}'
plasmite follow builds --last 10     # in another terminal
plasmite fetch builds --since 1h     # later, to review
```

`follow` prints the ten newest messages and then each new one as it arrives. `fetch` prints the last hour and exits.

## Gate a deploy on CI

```bash
# Deploy job: wait until this run's tests pass.
plasmite follow "ci-$CI_RUN_ID" --create --since oldest --tag ok --one > /dev/null && ./deploy.sh

# Test job: announce success.
./run-tests.sh && plasmite feed "ci-$CI_RUN_ID" --create --tag ok '{"step": "test", "status": "ok"}'
```

Both jobs set `CI_RUN_ID` to the same value. `--since oldest` catches a result that arrived before the deploy job started, and `--one` exits as soon as one matching message appears.

## Capture command output

```bash
set -o pipefail
cargo build 2>&1 | plasmite feed builds --create --input-format lines
plasmite fetch builds --last 10
```

Each line becomes a message such as `{"line": "warning: unused variable"}`. `2>&1` includes stderr, and `pipefail` makes the pipeline fail when the build fails.

## Keep a rolling system log

```bash
plasmite pool create syslog --size 64M
journalctl -o json -f | plasmite feed syslog                  # Linux
/usr/bin/log stream --style ndjson | plasmite feed syslog     # macOS
```

The pool never grows past 64 MiB. New entries displace the oldest ones. To search the recent window, use the field your collector writes.

```bash
plasmite fetch syslog --since 30m | jq -c 'select((.data.MESSAGE // .data.eventMessage) | strings | test("panic"))'
```

## Let several programs write to one pool

```bash
plasmite feed events --create --tag deploy --tag api '{"service": "api", "sha": "f4e5d6c"}'  # deploy script
plasmite feed events --tag error '{"service": "api", "msg": "latency spike"}'  # health check
plasmite feed events --tag metric '{"service": "web", "rps": 1420}'            # metrics job
plasmite follow events --tag error                                             # on-call terminal
```

After the first command creates the pool, each writer can append independently. Each append gets the next sequence number, and each reader picks its messages by tag.

## Read only the messages you need

```bash
plasmite follow events --tag error
plasmite fetch events --since 1h --tag deploy --tag api
plasmite fetch events --last 100 | jq -c 'select(.data.service == "api")'
```

Repeated `--tag` options require every tag. Use `jq` to filter on fields inside `data`.

## Resume after a restart

```bash
next=$(cat sensors.next 2>/dev/null || echo 1)
plasmite follow sensors --since "$next" | while read -r msg; do
  jq -c .data <<<"$msg"
  jq '.seq + 1' <<<"$msg" > sensors.next
done
```

`sensors.next` holds the next sequence to handle, so a restarted script continues where the last run stopped. If the pool overwrote messages while the script was down, `follow` reports a `RetentionGap` on stderr and continues with the oldest message it still holds. The [library examples](#use-plasmite-from-python-javascript-and-go) show the same pattern with explicit gap handling.

## Save history to a file and replay it

```bash
plasmite fetch events --since 1h > incident.jsonl
plasmite feed replay --create < incident.jsonl
plasmite fetch replay --tag error
```

Saved messages keep their data and tags. The destination pool gives each one a new sequence and time.

## Chat between two terminals

```bash
plasmite duplex chat --create --last 20
```

Run this in two terminals and type a message such as `{"from": "ana", "text": "hi"}` in either. Each side sees every message, including its own. Press Ctrl+C to leave.

## Share a pool with a teammate

```bash
# On the server:
plasmite pool create builds
plasmite serve https://pools.example.net:9743
plasmite access invite laptop        # in a second terminal; prints an access key

# On the teammate's laptop:
plasmite access connect https://pools.example.net:9743     # enter the key when asked
plasmite follow https://pools.example.net:9743/builds --last 20
```

The key grants full access to every pool in the server's directory. [Share pools across machines](record/serving.md#share-your-first-pool) covers certificates, browsers and starting the server at boot.

## Collect sensor readings over UDP

```bash
plasmite pool create sensors
plasmite serve --udp-follow                                                  # collector
plasmite follow udp://127.0.0.1:9701/sensors --since oldest                   # dashboard, in another terminal
plasmite feed udp://127.0.0.1:9701/sensors --tag sensor '{"temp_c": 21.4}'      # sensor, in a third terminal
```

The default listener accepts only local traffic at `127.0.0.1:9701`. A successful send confirms the operating system accepted the datagram. Readings can disappear, duplicate or arrive out of order. Anyone who can reach a listener with `--udp-follow` can read and write. Use UDP on a trusted network for frequent readings, where the next reading makes up for a lost one. [UDP messages](record/serving.md#send-and-read-over-udp) covers addresses, size limits and senders without Plasmite installed.

## Run a collector at boot

On a Raspberry Pi or other Linux sensor host, let systemd start your collector and restart it if it fails. Save `/etc/systemd/system/sensor-collector.service`:

```ini
[Unit]
Description=Sensor collector
RequiresMountsFor=/home/sensors/.plasmite

[Service]
User=sensors
Environment=HOME=/home/sensors
ExecStart=/home/sensors/collect.sh
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now sensor-collector.service
journalctl -u sensor-collector.service -n 20
```

Create an executable `/home/sensors/collect.sh` for the `sensors` account. Its commands can pipe readings to `plasmite feed sensors --create`. Use absolute paths for binaries if the service's search path differs from your shell's. A restarted process may have to resend a reading whose write was interrupted. If duplicates matter, give each reading an application ID and check retained history for that ID before resending. To serve the pools at boot too, use [the server installer](record/serving.md#start-a-server-at-boot).

## Record TouchOSC messages

[TouchOSC](https://hexler.net/touchosc) sends Open Sound Control (OSC) messages over UDP. From a checkout of this repository:

```bash
uv run examples/osc_to_json.py | plasmite feed osc --create --tag osc
uv run examples/osc_to_json.py --send-test     # in a second terminal
plasmite fetch osc --last 1
```

The adapter turns each OSC message into data such as `{"address": "/fader/1", "args": [0.75]}`. To record a phone, start the adapter with `--host 0.0.0.0` on a trusted network and point a TouchOSC UDP connection at this computer's address, port 9000.

## Give an AI agent your pools

Add Plasmite to the agent's Model Context Protocol (MCP) configuration:

```json
{"mcpServers": {"plasmite": {"command": "plasmite", "args": ["mcp"]}}}
```

The agent can list pools and call `plasmite_feed`, `plasmite_fetch` and `plasmite_follow`. MCP filters by tags only. For a shared server, connect the agent to `https://pools.example.net:9743/mcp`; see [Connect an AI client](record/serving.md#connect-an-ai-client).

## Use Plasmite from Python, JavaScript and Go

Each language offers the same operations under its own naming. The first program in each section writes a message and reads recent ones back. The second follows a pool and saves its place, like [Resume after a restart](#resume-after-a-restart). If the pool overwrote messages while the program was down or behind, the read raises `RetentionGap`: `missing` is the first sequence lost and `next_seq` the oldest one still retained. These programs log the lost range and continue at `next_seq`. An application that can't accept missing messages would rebuild its state there instead.

### Python

Install with `uv add plasmite`. See the [Python README](../bindings/python/README.md) for more.

```python
from plasmite import open_pool

with open_pool("builds", create=True) as pool:
    pool.feed({"step": "test", "status": "ok"}, tags=["ci"])
    with pool.fetch(last=10, tags=["ci"]) as reader:
        for msg in reader:
            print(msg.seq, msg.tags, msg.data)
```

```python
from pathlib import Path
from plasmite import RetentionGapError, open_pool

checkpoint = Path("sensors.next")  # the next sequence to handle
since = int(checkpoint.read_text()) if checkpoint.exists() else 1

with open_pool("sensors", create=True) as pool:
    while True:
        try:
            with pool.follow(since=since) as reader:
                for msg in reader:
                    print(msg.seq, msg.data)
                    since = msg.seq + 1
                    checkpoint.write_text(str(since))
        except RetentionGapError as gap:
            print(f"missed {gap.missing}-{gap.next_seq - 1}; resuming at {gap.next_seq}")
            since = gap.next_seq
```

### JavaScript

Install with `npm install plasmite` (Node.js 24 or newer). Save these as `.mjs` files. See the [Node README](../bindings/node/README.md) for more.

```js
import { openPool } from "plasmite";

const pool = openPool("builds", { create: true });
await pool.feed({ step: "test", status: "ok" }, { tags: ["ci"] });
for await (const msg of await pool.fetch({ last: 10, tags: ["ci"] })) {
  console.log(msg.seq, msg.tags, msg.data);
}
pool.close();
```

```js
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { ErrorKind, openPool } from "plasmite";

const checkpoint = "sensors.next"; // the next sequence to handle
let since = existsSync(checkpoint) ? BigInt(readFileSync(checkpoint, "utf8")) : 1n;

const pool = openPool("sensors", { create: true });
for (;;) {
  try {
    for await (const msg of await pool.follow({ since })) {
      console.log(msg.seq, msg.data);
      since = msg.seq + 1n;
      writeFileSync(checkpoint, String(since));
    }
  } catch (err) {
    if (err.kind !== ErrorKind.RetentionGap) throw err;
    console.log(`missed ${err.missing}-${err.nextSeq - 1n}; resuming at ${err.nextSeq}`);
    since = err.nextSeq;
  }
}
```

Sequences and times are `bigint` values, so the arithmetic uses `1n`.

### Go

Install with `go get github.com/sandover/plasmite/bindings/go/v2/local`; the package links against the Plasmite library, described in the [Go README](../bindings/go/README.md). The `must` helper stops the program on any error.

```go
package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log"

	plasmite "github.com/sandover/plasmite/bindings/go/v2/local"
)

func main() {
	client := must(plasmite.NewDefaultClient())
	defer client.Close()
	pool := must(client.OpenPool("builds", plasmite.Create(true)))
	defer pool.Close()

	must(pool.Feed(map[string]any{"step": "test", "status": "ok"}, []string{"ci"}))
	reader := must(pool.Fetch(plasmite.Last(10), plasmite.Tags("ci")))
	defer reader.Close()
	for {
		msg, err := reader.Next(context.Background())
		if errors.Is(err, io.EOF) {
			break
		}
		must(msg, err)
		fmt.Println(msg.Seq, msg.Tags(), string(msg.Data))
	}
}

func must[T any](value T, err error) T {
	if err != nil {
		log.Fatal(err)
	}
	return value
}
```

The second program has the same imports, minus `io` and plus `os` and `strconv`, and the same `must` helper.

```go
const checkpoint = "sensors.next" // the next sequence to handle

func main() {
	client := must(plasmite.NewDefaultClient())
	defer client.Close()
	pool := must(client.OpenPool("sensors", plasmite.Create(true)))
	defer pool.Close()

	since := uint64(1)
	if saved, err := os.ReadFile(checkpoint); err == nil {
		since = must(strconv.ParseUint(string(saved), 10, 64))
	}
	for {
		err := follow(pool, &since)
		var gap *plasmite.Error
		if !errors.As(err, &gap) || gap.Kind != plasmite.ErrorRetentionGap {
			log.Fatal(err)
		}
		fmt.Printf("missed %d-%d; resuming at %d\n", gap.Missing, gap.NextSeq-1, gap.NextSeq)
		since = gap.NextSeq
	}
}

// follow prints each message from since onward and saves the next sequence.
func follow(pool *plasmite.Pool, since *uint64) error {
	reader, err := pool.Follow(plasmite.Since(*since))
	if err != nil {
		return err
	}
	defer reader.Close()
	for {
		msg, err := reader.Next(context.Background())
		if err != nil {
			return err
		}
		fmt.Println(msg.Seq, string(msg.Data))
		*since = msg.Seq + 1
		if err := os.WriteFile(checkpoint, []byte(strconv.FormatUint(*since, 10)), 0o644); err != nil {
			return err
		}
	}
}
```

## When Plasmite isn't the right fit

Plasmite serves processes on one machine and the machines close to it. Other tools fit these needs better:

| If you need | Consider |
| --- | --- |
| Replication across a cluster of hosts | Kafka, NATS JetStream or Redpanda; a pool lives on one filesystem. |
| Schema registry or contract enforcement | Confluent Schema Registry or Buf; Plasmite accepts any JSON object. |
| Workflow orchestration with retries and state machines | Temporal or Inngest. |
| The fastest channel between threads in one process | OS pipes, `crossbeam` or Go channels; writing to disk adds cost. |
| Durable storage for large files | S3 or MinIO; a pool holds a bounded window of small messages. |
