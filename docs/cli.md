# CLI guide

This guide explains how `plasmite` reads, writes and reports errors, so you can predict what a script will do. `plasmite COMMAND --help` lists every option, the [cookbook](cookbook.md) has complete recipes, and the [2.0 contract](../spec/v2/SPEC.md#cli) specifies the edge cases.

## The model

A pool is a file that keeps the most recent messages, up to a fixed size, and overwrites the oldest when it fills. Three commands do the work: `feed` writes, `follow` waits for new messages, and `fetch` reads what the pool holds and exits.

Every message has four fields:

```json
{"seq":42,"time":"2026-11-19T09:00:00.123456789Z","meta":{"tags":["deploy"]},"data":{"service":"api"}}
```

The pool assigns `seq`, which starts at 1, never repeats, and defines the order. `time` records the clock when the pool stored the message. Clocks can step backward, so keep a measurement's own time in `data` when it matters. `meta.tags` holds labels for filtering, and `data` is your JSON object. If you write through a server with an access key, `meta.sender` holds the key name.

## Reading

Every read starts at a position. With none, `follow` waits for the next new message and `fetch` starts at the oldest message the pool holds.

```bash
plasmite follow events                   # the next new message onward
plasmite fetch events                    # everything the pool holds, then exit
plasmite follow events --since oldest    # everything, then keep waiting
plasmite fetch events --since 42         # sequence 42 onward
plasmite fetch events --since 5m         # the last five minutes
plasmite fetch events --since 2026-11-19T09:00:00Z
plasmite follow events --last 20         # the 20 newest, then keep waiting
```

`--since` takes a sequence, `oldest`, a duration such as `5m` or `2h`, or an RFC 3339 time. `--last N` (or `-n N`) starts N messages back. A read takes one position, so `--since` with `--last` fails.

`--tag` filters after the position is set. `--last 100 --tag error` looks at the 100 newest messages and keeps those tagged `error`, so it can print fewer than 100. Repeat `--tag` to require several tags. To filter on fields inside `data`, pipe to `jq`:

```bash
plasmite follow events --tag deploy | jq -c 'select(.data.service == "api")'
```

`fetch` stops at the newest message present when it started, even while writers keep going. `follow` runs until you stop it, with two exceptions that make it useful in scripts:

- `follow --one` exits after the first matching message: `plasmite follow builds --tag ci --one && ./deploy.sh`.
- On Linux and macOS, `follow` exits when the command it pipes into closes, as in `plasmite follow events | head -1`. PowerShell can leave the pipe open, so use `--one` in scripts that must run there too.

To read one message by sequence, pass it after the pool name:

| `plasmite fetch events 42`, when 42 is | Result | Exit code |
| --- | --- | --- |
| still in the pool | prints the message | 0 |
| already overwritten | `RetentionGap` | 9 |
| the next sequence, not yet written | `NotFound` | 3 |
| further ahead, or zero | `Usage` | 2 |

### When a reader falls behind

A reader can ask for messages the pool has already overwritten: a `--since` sequence that is too old, or a `follow` that can't keep up with fast writers. Plasmite never skips them silently. It reports `RetentionGap` with `missing`, the first sequence lost, and `next_seq`, the oldest one still held:

```json
{"error":{"hint":"Reopen at the next available sequence or rebuild from an authoritative source.","kind":"RetentionGap","message":"messages 1-187 are no longer retained; the next available message is 188","missing":1,"next_seq":188,"seq":1}}
```

`fetch` prints the error on stderr and exits 9. `follow` prints it on stderr and carries on from `next_seq`. A filter can't hide a gap, because Plasmite checks for loss before it applies `--tag`. A time or duration start never reports a gap: the pool can't know whether the messages it dropped fell inside your window, so the read starts at the oldest message it holds.

To resume after a restart, save the `seq` of the last message you handled and start the next run at `--since` that number plus one. The [cookbook](cookbook.md#resume-after-a-restart) shows the pattern.

## Writing

`feed` stores a JSON object, given as an argument or piped on stdin, and prints the stored message:

```bash
plasmite feed events '{"step": "test", "status": "ok"}'
plasmite feed events --tag deploy --tag api '{"sha": "f4e5d6c"}'
jq -c '.[]' readings.json | plasmite feed sensors
```

`data` must be an object. `--create` makes a missing pool at the default 1 MiB; `plasmite pool create sensors --size 64M` chooses a size. If one input in a stream is invalid, `feed` stops with an error, and the messages before it stay stored.

By default `feed` returns once the message is in the pool. `--durability flush` also waits for the disk. A flush or network failure can arrive after the pool stored the message, so check the pool before you send it again.

**Command output.** `--input-format lines` stores each line of text as `{"line": "..."}`:

```bash
make 2>&1 | plasmite feed builds --input-format lines
```

**Forwarding.** An object with exactly `seq`, `time`, `meta` and `data` counts as a message when `meta` is an object with `tags`. `feed` keeps its data and tags. The destination assigns a new sequence and time and supplies any sender name. That makes copying a pool one pipe:

```bash
plasmite follow events | plasmite feed archive
```

A forwarded message brings its own tags, so adding `--tag` fails.

**Other inputs.** `feed` also detects YAML documents and framed Lite3 binary on stdin. `--input-format` names one format when detection isn't what you want: `json`, `yaml`, `lite3`, `lite3-raw` or `lines`.

**Size limits.** A message can hold up to 256 MiB and must fit in the pool. A server accepts 1 MiB per request unless started with `--max-body-bytes`, and a UDP datagram holds at most about 64 KiB.

## Output

In a terminal, `follow`, `fetch`, and `duplex` show one line per message with its local time, sequence, sender when available, tags, and JSON data. The display includes the date for messages from other days and trims the line to the terminal width. `feed` shows indented JSON in a terminal. Pipes and files receive one complete JSON message per line by default. Choose a format with `--format` or `-o`.

| `--format` | Output |
| --- | --- |
| `compact` | one readable line per message |
| `pretty` | readable, indented JSON |
| `json` | one compact message per line |
| `yaml` | one YAML document per message |
| `lite3` | a binary stream for fast copies between pools |
| `lite3-raw` | one message's raw binary, with `fetch POOL SEQ` |

`--color auto` uses color in a terminal unless you set `NO_COLOR` to a nonempty value. Use `--color always` or `--color never` to choose explicitly.

`feed` reads JSON, YAML and `lite3` output back directly. Bash and Zsh pass binary through pipes unchanged, and so does [PowerShell 7.4 and later](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_redirection#example-7-redirecting-binary-data-from-a-native-command); with older PowerShell, run binary pipelines in `cmd.exe`. Keep stderr out of a binary pipe. The [binary stream contract](../spec/v2/SPEC.md#binary-stream) gives the byte layout.

## Describe a pool

Give readers a short description and an example of the data the pool carries.

```bash
plasmite pool create builds --about "Build results" --example '{"status":"passed"}'
plasmite pool info builds
plasmite pool describe builds --about "Build and deploy results" --example '{"status":"deployed"}'
```

`pool describe` replaces both fields. Leave out a field to clear it. Each field allows up to 1,024 bytes. `pool info` and structured `pool list` output show both fields. The terminal list shows the description.

## Pool references

A pool name such as `events` lives in `~/.plasmite/pools`, or in the directory you pass with `--dir`. A path such as `./data/events.plasmite` names a file directly. Global options (`--dir`, `--format`, `--color`) go before or after the command.

Remote pools use a URL in the same place:

- `https://host:port/events` reaches a pool on a shared server, using the key you saved with `plasmite access connect`.
- `http://127.0.0.1:9700/events` reaches a running local server from the same machine, with no key.
- `udp://host:port/events` sends and receives datagrams that may be lost, duplicated or reordered.

| Command | Local | HTTPS | UDP |
| --- | --- | --- | --- |
| `feed`, `fetch`, `follow`, `duplex` | yes | yes | yes |
| `pool info`, `pool list`, `pool describe` | yes | yes | no |
| `pool create`, `pool delete`, `doctor` | yes | no | no |

The [serving guide](record/serving.md) covers starting a server, access keys and [UDP](record/serving.md#send-and-read-over-udp).

## Duplex

`duplex POOL` feeds what you type and follows the pool in one session, which makes a two-way chat:

```bash
plasmite duplex chat --create --last 20
```

You see your own messages along with everyone else's. When your input ends, duplex stops sending and keeps listening until you press Ctrl+C or the downstream command closes.

## Errors and exit codes

Errors go to stderr, as readable text in a terminal and JSON in a pipe.

| Exit code | Error kind | Typical cause |
| --- | --- | --- |
| 0 | | success, including an empty `fetch` |
| 1 | `Internal` | an unexpected failure |
| 2 | `Usage` | bad arguments or invalid input |
| 3 | `NotFound` | a missing pool, or a sequence not yet written |
| 4 | `AlreadyExists` | `pool create` for a pool that exists |
| 5 | `Busy` | another process holds the pool or resource |
| 6 | `Permission` | the system or server refused access |
| 7 | `Corrupt` | a damaged pool file |
| 8 | `Io` | a disk or network failure |
| 9 | `RetentionGap` | the read needed messages the pool already overwrote |
