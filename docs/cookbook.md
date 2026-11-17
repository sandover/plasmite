# Plasmite Cookbook

## Contents

- [CI Gate](#ci-gate)
- [Live Event Stream](#live-event-stream)
- [Record TouchOSC messages with OSC](#record-touchosc-messages-with-open-sound-control-osc)
- [Lite3 Documents](#lite3-documents)
- [Capture command output](#capture-command-output)
- [Two-way communication](#two-way-communication)
- [System Log Ring Buffer](#system-log-ring-buffer)
- [Replay & Debug](#replay--debug)
- [Remote Pool Access](#remote-pool-access)
- [Detect Retention Gaps](#detect-retention-gaps)
- [MCP Agent Access](#mcp-agent-access)
- [When Plasmite Isn't the Right Fit](#when-plasmite-isnt-the-right-fit)
- [Next Steps](#next-steps)

---

A pool retains a bounded history, and readers keep their own checkpoints.
`--last N` selects the last N retained messages before filters. Use
`fetch` to read retained messages and exit. Pipes and files default to JSON; choose
`--format yaml` when that helps your workflow. See the [CLI guide](cli.md) for shared rules and the
[1.2 upgrade guide](record/upgrading-1.2.md) for changed defaults.

## YAML Messages

```bash
plasmite pool create source destination
plasmite feed source --tag sensor '{"temperature":21}'

# Copy history as YAML documents.
plasmite fetch source --format yaml |
  plasmite feed destination
```

YAML carries the same contents and tags as JSON. Each document starts with
`---` and ends with `...`, so a receiving command can append it immediately.
Reports also support `--format yaml`.

---

## CI Gate

A deploy script needs to wait until the test runner says "green". No polling loops, no lock files, no shared database.

```bash
# deploy.sh — blocks until this run's tests pass
pls follow "ci-$CI_RUN_ID" --create --last 1 --tag green --one > /dev/null && ./deploy-to-staging.sh

# test-runner.sh — signals when done (--create makes the pool if needed)
./run-tests.sh && pls feed "ci-$CI_RUN_ID" --create --tag green '{"status": "green", "commit": "a1b2c3d", "suite": "unit"}'
```

Set `CI_RUN_ID` to the same unique value in both jobs. `--one` exits when a matching message arrives.

<details>
<summary><strong>Python · Node · Go</strong></summary>

**Python — wait for green**

```python
from os import environ
from plasmite import Client

with Client() as c, c.open_pool(f"ci-{environ['CI_RUN_ID']}", create=True) as pool:
    with pool.follow(last=1, tags=["green"]) as reader:
        msg = next(reader)
        print(f"commit {msg.data['commit']} passed")
```

**Python — signal green**

```python
from os import environ
from plasmite import Client

with Client() as c, c.open_pool(f"ci-{environ['CI_RUN_ID']}", create=True) as pool:
    pool.feed({"status": "green", "commit": "a1b2c3d", "suite": "unit"}, tags=["green"])
```

**Node — wait for green**

```js
const { Client } = require("plasmite");
(async () => {
  const c = new Client();
  let pool;
  try {
    pool = c.openPool(`ci-${process.env.CI_RUN_ID}`, { create: true });
    for await (const msg of await pool.follow({ last: 1, tags: ["green"] })) {
      console.log(`commit ${msg.data.commit} passed`);
      break;
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
p, _ := c.OpenPool(plasmite.PoolRefName("ci-"+os.Getenv("CI_RUN_ID")), plasmite.Create(true))
defer p.Close()
defer c.Close()
ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
defer cancel()
reader, err := p.Follow(plasmite.Last(1), plasmite.Tags("green"))
if err != nil { log.Fatal(err) }
defer reader.Close()
msg, err := reader.Next(ctx)
if err != nil { log.Fatal(err) }
fmt.Println(string(msg.Data))
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

# on a laptop, after saving access to the Pi's shared server:
pls follow https://pi.example.net:9743/telemetry --tag sensor | jq -c 'select(.data.value > 80000)'

# replay the last hour of readings to see the trend
pls fetch https://pi.example.net:9743/telemetry --tag sensor --since 1h \
  | jq '.data | [.ts, .value]'
```

Use the [sharing walkthrough](record/serving.md#share-your-first-pool) to serve
the Pi's pool and save the laptop's connection. A local name such as `telemetry`
refers to that machine's pool directory; use the shared URL from another machine.

### Check receipt, time and retained history

Send an identifiable reading, then read it at the destination:

```sh
pls feed https://pi.example.net:9743/telemetry \
  '{"reading_id":"check-001","measured_at":"2026-11-16T20:00:00Z","temperature_c":21.5}'
pls fetch https://pi.example.net:9743/telemetry --last 1
```

Check `data.reading_id` and the value. The destination assigns `seq` and `time`
while handling the append. Keep the sensor's measurement time in `data`, as
`measured_at` does here. An earlier measurement can reach the pool later.

Each successful `feed` output confirms one append. `--durability flush` requests
the destination's storage flush before success; the default is `fast`.
An I/O error can occur after storage but before the sender receives the result.
Inspect the pool for the reading's application ID before sending it again.
A manual resend can create another message with the same data and a new sequence.

A connection failure ends `feed` and leaves later input unsent. A full input pipe
slows the producer, and closing it exposes a broken pipe to upstream writers.
When a downstream stdout failure ends `feed`, completed appends remain stored.
For shell pipelines, use `set -o pipefail` in Bash to preserve failure status.

On the pool machine, inspect capacity with `pls pool info telemetry`. Feed
representative readings through a small test pool until it wraps, then divide
its retained message count by the sampling rate to estimate history. Header,
index and message framing consume space too. In the
[Debian check](validation/debian-vm-validation.md), a 64 KiB pool retained 74 of the
1,000 test readings: about 18.5 seconds at four readings per second. Different
readings change that estimate. Choose a larger pool for longer history;
`--since 1h` can only return history the pool still holds.

### Run a Linux collector at boot

Let systemd start and monitor the collector. Use the account that owns the pools
and absolute paths. This example uses `brandon`; replace the account and paths
with your own. `collect-to-pool.py` represents your collector program: it sends
readings through `feed` and exits nonzero when that child fails. The
[tested fixture and reboot record](validation/debian-vm-validation.md) show a complete
synthetic collector using Python's standard library.

Save `/etc/systemd/system/sensor-collector.service`:

```ini
[Unit]
Description=Sensor collector
After=network.target
RequiresMountsFor=/home/brandon/pools

[Service]
Type=simple
User=brandon
Environment=HOME=/home/brandon
ExecStart=/usr/bin/python3 /home/brandon/sensors/collect-to-pool.py
Restart=on-failure
RestartSec=1
UMask=0077

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now sensor-collector.service
sudo journalctl -u sensor-collector.service -n 30 --no-pager
```

The service runs before sign-in when its executable and pool storage are
available at boot. `After=network.target` orders startup; a remote destination
can still be unavailable. `Restart=on-failure` starts a new collector process
after failure. Restarting a process does not recover its previous unread input
or establish whether an uncertain write succeeded. Keep that choice in the
application. For a remote destination, save access as the service account.

Use [Plasmite's server installer](record/serving.md#start-a-server-at-boot) for
the shared server itself. After reboot, read a fresh identifiable sample from
another machine and fetch an acknowledged older sample to check retained data.

### Send small messages over UDP

UDP input is useful when a sender can tolerate loss, duplicates, and reordering.
Start a server with `--udp`; the receiver appends one JSON or Lite3 message to
the named pool. The address below is local to the server. Change it to the
server's chosen bind address when the sender runs elsewhere.

#### Feed and follow with the CLI

Create the pool, then start the server over its directory:

```sh
plasmite pool create sensors
plasmite serve --udp-follow
```

In another terminal, follow it:

```sh
plasmite follow udp://127.0.0.1:9701/sensors --tag sensor
```

Send a reading:

```sh
plasmite feed udp://127.0.0.1:9701/sensors '{"temperature_c":22.4}' --tag sensor
```

`feed` writes no receipt to stdout. Its successful exit confirms local send,
not storage. The
[UDP receive policy](../spec/udp/v1/SPEC.md#receive-ordering-and-notices) requires
`follow` to deliver matching messages in arrival order, including late messages
and duplicates, and report only explicit server skips. Sequence jumps never create notices or
discard messages. Use `fetch POOL --last 10` for a best-effort finite read;
explicit oversized/pressure skips make it exit 1, and a confirmed retention
gap makes it exit 9. Terminal progress ends the read and can leave DATA in
flight. Exit 0 gives no assurance of complete delivery. Filters never create
skip notices. Use HTTP when you need reliable history or a committed write
receipt.

The same commands accept JSON, YAML, and framed Lite3 input or output through
the existing format options. The maximum datagram size is 65,507 bytes including
routing and application framing. A stored message too large for outgoing UDP
produces a skip notice. The native SDKs expose separate UDP clients with
`send` methods and follow events; see their READMEs.

#### Send JSON from Python

This example uses only Python's standard library and checks the full datagram
size before sending:

```python
import json
import socket

pool = "sensors"
message = {"data": {"temperature_c": 22.4, "sample": 81},
           "tags": ["sensor"]}
payload = pool.encode("utf-8") + b"\x00" + json.dumps(
    message, separators=(",", ":")
).encode("utf-8")
if len(payload) > 65_507:
    raise ValueError("UDP payload exceeds 65507 bytes; use HTTP")

with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 128 * 1024)
    sock.sendto(payload, ("127.0.0.1", 9701))
```

`sendto` only reports that the local operating system accepted the datagram.
It does not confirm that Plasmite received or stored it.

#### Relay a Lite3 message from the CLI

`fetch --format lite3` writes the raw Lite3 body, without its message envelope.
Run this script with a sequence number, such as `python3 relay.py 1`. It fetches
one local message and sends those bytes after the pool name and NUL delimiter:

```python
import socket
import subprocess
import sys

pool = "sensors"
seq = sys.argv[1]
body = subprocess.run(
    ["plasmite", "fetch", pool, seq, "--format", "lite3"],
    check=True,
    stdout=subprocess.PIPE,
).stdout
payload = pool.encode("utf-8") + b"\x00" + body
if len(payload) > 65_507:
    raise ValueError("UDP payload exceeds 65507 bytes; use HTTP")

with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 128 * 1024)
    sock.sendto(payload, ("127.0.0.1", 9701))
```

The destination pool assigns a new sequence and append time. The Lite3 bytes
keep their original content and tags. See the
[UDP input contract](../spec/udp/v0/SPEC.md) for the format and access rules.

### Multi-writer event bus

Several processes write to the same pool with different tags. An on-call engineer tails only what matters.

```bash
pls feed telemetry --tag deploy '{"service": "api", "sha": "f4e5d6c"}'
pls feed telemetry --tag alert  '{"service": "api", "msg": "latency spike"}'
pls feed telemetry --tag metric '{"service": "web", "rps": 1420}'

# on-call — show only api alerts
pls follow telemetry --last 100 --tag alert | jq -c 'select(.data.service == "api")'

# postmortem — what happened in the 10 minutes before the alert?
mkdir -p tmp
pls fetch telemetry --since 10m > tmp/timeline.jsonl
```

### Import external API events

Stripe's [events API](https://docs.stripe.com/api/events/list) returns a page. Pipe each event into a pool to inspect it later.

```bash
# import one page of Stripe events
curl -fsS -G https://api.stripe.com/v1/events \
  -H "Authorization: Bearer $STRIPE_KEY" -d limit=100 \
  | jq -c '.data[]' | pls feed stripe-events --create

# filter imported completed payments
pls fetch stripe-events --last 100 | jq -c 'select(.data.type == "payment_intent.succeeded")'

# replay messages imported in the last 20 minutes
pls fetch stripe-events --since 20m

# export the last 500 events for offline analysis, then exit
mkdir -p tmp
pls fetch stripe-events --last 500 > tmp/stripe-dump.jsonl
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

# a deploy script — ship only when the completed build succeeded
pls follow build --tag done --last 100 --one | jq -e '.data.ok == true' > /dev/null && ./deploy.sh

# next morning — what happened overnight?
pls fetch build --since 12h
```

<details>
<summary><strong>Python · Node · Go</strong></summary>

**Python — produce tagged events**

```python
from plasmite import Client

with Client() as c, c.open_pool("telemetry", create=True) as pool:
    pool.feed({"sensor": "temp", "value": 42100}, tags=["sensor"])
    pool.feed({"service": "api", "sha": "f4e5d6c"}, tags=["deploy"])
    pool.feed({"service": "api", "msg": "latency spike"}, tags=["alert"])
```

**Python — filter by tag**

```python
from plasmite import Client

with Client() as c, c.open_pool("telemetry") as pool:
    with pool.follow(tags=["alert"]) as reader:
        for msg in reader:
            print(msg.seq, msg.tags, msg.data)
```

**Node — produce tagged events**

```js
const { Client } = require("plasmite");
(async () => {
  const c = new Client();
  let pool;
  try {
    pool = c.openPool("telemetry", { create: true });
    await pool.feed({ sensor: "temp", value: 42100 }, { tags: ["sensor"] });
    await pool.feed({ service: "api", sha: "f4e5d6c" }, { tags: ["deploy"] });
    await pool.feed({ service: "api", msg: "latency spike" }, { tags: ["alert"] });
  } finally {
    pool?.close();
    c.close();
  }
})();
```

**Go — produce tagged events**

```go
c, _ := plasmite.NewDefaultClient()
p, _ := c.OpenPool(plasmite.PoolRefName("telemetry"), plasmite.Create(true))
p.Feed(map[string]any{"sensor": "temp", "value": 42100}, []string{"sensor"})
p.Feed(map[string]any{"service": "api", "sha": "f4e5d6c"}, []string{"deploy"})
p.Feed(map[string]any{"service": "api", "msg": "latency spike"}, []string{"alert"})
p.Close(); c.Close()
```

**Go — filter by tag**

```go
c, _ := plasmite.NewDefaultClient()
p, _ := c.OpenPool(plasmite.PoolRefName("telemetry"))
defer c.Close()
defer p.Close()
reader, err := p.Follow(plasmite.Tags("alert"))
if err != nil { log.Fatal(err) }
defer reader.Close()
for {
    msg, err := reader.Next(ctx)
    if err != nil { break }
    fmt.Println(msg.Seq, msg.Tags(), string(msg.Data))
}
```

</details>

---

## Record TouchOSC messages with Open Sound Control (OSC)

TouchOSC can send Open Sound Control (OSC) messages over User Datagram
Protocol (UDP). A small Python adapter turns supported values into JSON Lines
for Plasmite. It uses `uv` to run the adapter and the installed Plasmite CLI to
write to a pool; it does not need Plasmite's Python bindings.

This recipe requires Plasmite 1.2.0 or newer, `uv`, and a checkout of this repository.
From the repository root, create a pool in a separate directory:

```bash
plasmite --dir ./tmp/osc-pools pool create osc
```

In one terminal, start the receiver. It listens on `127.0.0.1:9000` by
default, writes JSON Lines to stdout, and sends status messages to stderr. The
`--send-test` option sends one sample message to the configured host and port,
then exits, so you can try the full pipe without TouchOSC:

```bash
uv run examples/osc_to_json.py \
  | plasmite --dir ./tmp/osc-pools feed osc --tag osc
```

In a second terminal, send a test message:

```bash
uv run examples/osc_to_json.py --send-test
```

Read the recorded message in a third terminal. The adapter emits an object
with an OSC address and its arguments. `feed` wraps it in the usual
Plasmite envelope; `follow` reads those messages:

```bash
plasmite --dir ./tmp/osc-pools fetch osc --last 1 --format json \
  | jq -c 'select(.data.address == "/fader/1")'
```

The result contains `"data":{"address":"/fader/1","args":[0.75]}` along
with the envelope's sequence, time, and metadata. To browse the pool while
messages arrive, start the local map server in another terminal and open its
URL:

```bash
plasmite --dir ./tmp/osc-pools serve --remote-bind 127.0.0.1:9743
```

Then open `http://127.0.0.1:9700/ui/map`.

### Connect TouchOSC on a phone

Use a trusted local network: this receiver accepts UDP messages without authentication.
For a phone on the same network, stop the receiver and restart it so it listens
on the computer's network interfaces:

```bash
uv run examples/osc_to_json.py --host 0.0.0.0 \
  | plasmite --dir ./tmp/osc-pools feed osc --tag osc
```

Keep the Plasmite feed pipe running with the receiver as before. In TouchOSC,
enable a UDP OSC connection. Set its destination host to the computer's local
network IP address and the port to `9000`. Do not enter `0.0.0.0` or the phone's own IP as the
destination. If the phone cannot connect, allow UDP port `9000` through the
computer's firewall on the local network. See [TouchOSC's OSC connection
guide](https://hexler.net/touchosc/manual/connections-osc) for its connection
settings.

The adapter supports OSC numbers, strings, booleans, null values, and arrays
that can be represented in JSON. It skips blobs and non-finite numbers with a
notice on stderr. Bundle messages are recorded separately as they arrive;
their timetags and OSC type tags are not retained. This recipe records incoming
messages and does not provide playback or an OSC output adapter.

---

## Copy, Edit, and Save Messages

```bash
# Create the two pools.
plasmite pool create source destination

# Add a tagged message.
plasmite feed source --tag sensor '{"temperature":21}'

# Copy future messages in another terminal.
plasmite follow source | plasmite feed destination

# Or edit a field as each message passes through.
plasmite follow source |
  jq -c '.data.temperature += 1' |
  plasmite feed destination

# Save retained messages and load them later.
plasmite fetch source > messages.jsonl
plasmite feed destination < messages.jsonl
```

Pipes carry complete messages, including their tags. Each destination assigns
its own sequence and time. `feed` writes the newly appended messages, so a
second `feed` can continue the chain.

## Lite3 Documents

Use a framed stream to copy one or many messages without changing their payload bytes.

```bash
plasmite fetch source 1 --format lite3 | plasmite feed destination
plasmite follow source --format lite3 | plasmite feed destination
```

Use the raw form when a device needs one attachment.

```bash
# Create a pool and add a heartbeat.
plasmite pool create relay
plasmite feed relay '{"kind":"heartbeat"}'

# Save the original attachment bytes.
plasmite fetch relay 1 --format lite3-raw > heartbeat.lite3

# Append those bytes as a new message.
plasmite feed relay --input-format lite3-raw < heartbeat.lite3
plasmite fetch relay 2
```

Local and remote pools share these forms. A payload must fit the pool and
Plasmite's 256 MiB limit; remote servers may set a lower request limit.
Bash and Zsh preserve binary pipes. PowerShell needs version 7.4 or later
for native binary redirection; earlier versions can use `cmd.exe`.

---

## Capture command output

Pipe a command's output into `feed --input-format lines` to store one message
per line. Use your shell's redirection to include stderr.

```bash
set -o pipefail
cargo build 2>&1 | pls feed build --create --input-format lines
pls fetch build --last 10
```

For a larger history, create the pool with `pls pool create build --size 64M`
before running the build.

---

## Two-way communication

`duplex` sends messages from stdin while receiving complete messages on stdout.
It uses the same JSON, marked YAML, and framed Lite3 input rules as `feed`,
including when stdin is a terminal. Application data has no reserved sender or
chat fields. Both sides can use this command against the same pool:

```bash
pls duplex events --create --last 20
```

Enter a JSON object such as `{"event":"ready"}`. The receive stream includes
all matching messages, including your own appends. Applications can use data and
tags for their own conventions. For remote use, connect with `access connect`,
then run `pls duplex https://192.0.2.10:9743/events --last 20`.

Input EOF closes sending while reception continues, so a script can send a
request and wait for another process's reply:

```bash
printf '{"ping": true}\n' | pls duplex events --timeout 5s || [ $? -eq 124 ]
```

The shell example accepts exit 124 as the chosen end of the receive session.
`--timeout` exits 124 after a period without output. Ctrl+C or closed stdout
also ends the session. Use `feed` for a finite copy that exits at input EOF.
See the [2.0 upgrade guide](record/upgrading-2.0.md) when replacing
older `--me`/`--echo-self` usage.

---

## System Log Ring Buffer

Pipe system logs into a pool. The ring buffer caps disk usage, and anything in the window can be replayed or searched.

```bash
# Linux — journald
journalctl -o json -f | pls feed syslog --create

# macOS — unified log
/usr/bin/log stream --style ndjson | pls feed syslog --create
```

Default pool size is 1 MB. For busier systems, make a bigger buffer:

```bash
pls pool create syslog --size 8M
journalctl -o json -f | pls feed syslog
```

Then, when something crashes:

```bash
# replay the last 30 minutes
pls fetch syslog --since 30m

# find kernel panics
pls fetch syslog --since 1h | jq -c 'select(.data.MESSAGE | test("panic"))'

# pipe to jq for further analysis
pls fetch syslog --since 10m | jq '.data | {SYSLOG_IDENTIFIER, MESSAGE}'
```

---

## Replay & Debug

Every message in a pool has a sequence number and a nanosecond timestamp, so replaying a time window is a one-liner.

An incident pool has been accumulating events. Something went wrong in the last hour:

```bash
# inspect the last hour in sequence order
pls fetch incidents --since 1h

# narrow down: only sev1 events with a 503 code
pls fetch incidents --since 2h --tag sev1 | jq -c 'select(.data.code == 503)'

# show just the last 20 messages
pls fetch incidents --last 20

# export the evidence for a postmortem
mkdir -p tmp
pls fetch incidents --tag error --last 100 > tmp/errors.jsonl
```

---

## Remote Pool Access

A machine exposes its local pools over HTTPS.

Replace `pools.example.net` with a hostname or IP the client can reach.

**On the server:**

```bash
# Create a pool and start the shared server.
plasmite pool create events
plasmite serve https://pools.example.net:9743
```

In another terminal on the server, create an invitation. It grants access to
every pool in the default pool directory:

```bash
# Create an access key for the laptop.
plasmite access invite laptop
```

Send the HTTPS address and access key separately, and keep the key private. On
the client, connect and enter the key at the prompt; Plasmite does not echo it:

```bash
# Connect and enter the access key.
plasmite access connect https://pools.example.net:9743

# Send a sensor reading.
plasmite feed https://pools.example.net:9743/events '{"sensor": "temp", "value": 23.5}'

# Read recent messages and watch for new ones.
plasmite follow https://pools.example.net:9743/events --last 20
```

Open the same HTTPS address in a browser to sign in with the key and browse the
pools. See [Open the pools in a browser](record/serving.md#open-pools-in-a-browser)
for certificate trust and browser setup.

---

## Detect Retention Gaps

A slow consumer can lose messages when writers wrap the pool. Local and HTTPS
readers raise `RetentionGap` before returning a later message, even if a tag
filter would have skipped the missing records.

```python
from plasmite import RetentionGapError

try:
    with pool.follow(since=last_handled + 1) as reader:
        for message in reader:
            process(message)
            last_handled = message.seq
except RetentionGapError as error:
    print(f"first missing sequence: {error.missing}")
    print(f"oldest retained sequence: {error.next_seq}")
```

If your application can rebuild its state, rebuild before resuming. If it can
accept incomplete history, reopen at `error.next_seq`. Use `pool.info()` to
inspect the retained range.

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
- `plasmite_fetch` reads retained messages. `plasmite_follow` waits for new messages by default.

An invited key grants full access to every pool in the served directory,
including creating and deleting pools, reading messages, and appending
messages. Existing pools also appear as MCP resources; reading one returns up
to the latest 20 messages. Tag filters require every specified tag. MCP does
not currently expose jq `where` filtering.

`plasmite_feed` is an append, not an upsert. If its transport fails after an
ambiguous response, retrying can append a duplicate; use an application-level
stable identifier when retries must be safe.

Creating a pool that already exists returns `AlreadyExists` without changing
the pool or its messages. Use the existing pool as-is unless the task explicitly
requires discarding it.

### Local MCP server (stdio)

For pools on this machine, configure the harness to launch Plasmite directly:

```json
{
  "mcpServers": {
    "plasmite": {
      "command": "plasmite",
      "args": ["mcp"]
    }
  }
}
```

For a shared server, configure the harness to connect directly to its HTTPS
`/mcp` address. The harness opens the server's OAuth authorization flow when
you add or log in to the server:

```console
claude mcp add --scope user --transport http plasmite https://pools.example.com:8443/mcp
claude mcp login plasmite
codex mcp add plasmite --url https://pools.example.com:8443/mcp --oauth-client-registration dcr --oauth-resource https://pools.example.com:8443/mcp
codex mcp login plasmite --oauth-client-registration dcr
```

Complete the OAuth login in your harness. See the
[Claude Code MCP guide](https://code.claude.com/docs/en/mcp)
and [Codex CLI MCP guide](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)
for current command options.

The harness stores its OAuth credentials for that server. The same tools,
schemas, and write behavior apply to local and remote pools.
See the [2.0 MCP tool contract](../spec/v2/SPEC.md#mcp). See the
[serving guide](record/serving.md) for direct HTTP MCP setup.

### Reading and resuming

Call `plasmite_follow` with the pool name to wait for new messages. To inspect
retained history, call `plasmite_fetch`. Either tool accepts `since` or
`last`, plus `tags` to require every listed tag. Use `count` to set the
maximum messages in a response. It defaults to 20 and accepts up to 200.

Each result includes `next_seq`, the next sequence the tool hasn't examined,
including when all examined messages failed the tag filter. Pass that number
as `since` to continue. The `ended` field tells you whether fetch completed
or the call reached a record, wait, or server limit.

If the pool overwrote messages after your checkpoint, the tool returns a
`RetentionGap` error. `missing` gives the first sequence you missed.
To accept the gap, continue from the error's `next_seq`.

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
- CI pipeline writes with CLI/API (`plasmite feed` or HTTP `POST /v2/pools/POOL/feed`).
- Agents read with MCP (`plasmite_fetch`, `count: 1`, optional `since`).

For browser access, open the server's HTTPS address and enter the access key on
the page. See [Open the pools in a browser](record/serving.md#open-pools-in-a-browser).

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
- The served browser UI uses a same-origin session cookie. Browser login and
  writes require an Origin that matches the shared HTTPS address; separate
  cross-origin frontend access is unsupported.
- Keep access keys and native Bearer secrets out of frontend source code and URLs.
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
