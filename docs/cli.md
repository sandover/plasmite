# CLI operating model

This guide explains how Plasmite commands find a pool, read, write and report errors, so you can predict what a script will do. Run `plasmite <command> --help` for exact arguments, use the [cookbook](cookbook.md) for complete tasks, and see the [2.0 contract](../spec/v2/SPEC.md#cli) for the specification.

## Pool references

A local pool reference is a name such as `events` or a path such as `./data/events.plasmite`. A name resolves inside the pool directory: `~/.plasmite/pools` unless you pass `--dir`. Global options (`--dir`, `--format`, `--color`) work before or after the command. One `--dir` applies to the whole invocation, including `mcp`, and conflicting repeats fail.

```console
plasmite --dir ./pools follow events
```

`https://host:port/pool` names a pool on a shared server and uses the connection you saved with `access connect`. While `plasmite serve` runs, `http://127.0.0.1:9700/pool` reaches its pools from the same machine without credentials. `udp://host:port/pool` sends and receives datagrams that may be lost, duplicated or reordered; see [UDP messages](record/serving.md#send-and-read-over-udp).

| Command | Local name/path | HTTP(S) URL | UDP URL |
| --- | --- | --- | --- |
| `pool create/delete` | yes | no | no |
| `pool info` | yes | yes | no |
| `pool list [SERVER]` | yes | yes (server origin) | no |
| `feed` | yes | yes | yes |
| `fetch` | yes | yes | yes |
| `follow` | yes | yes | yes |
| `duplex` | yes | yes | yes |
| `doctor` | yes | no | no |

To serve pools, install a server at boot or manage access keys, see [Share pools across machines](record/serving.md).

## Messages

Every stored message has four fields:

```json
{"seq":42,"time":"2026-11-19T09:00:00.123456789Z","meta":{"tags":["deploy"]},"data":{"service":"api"}}
```

`seq` identifies the message and orders it within its pool; it starts at 1 and never repeats. `time` records when the pool stored the message, in UTC with nine fractional digits. Clocks can step, so `time` can repeat or go backward; keep a measurement's own time in `data` when it matters. `meta.tags` lists strings that readers can filter on. `data` holds your object.

A pool keeps a fixed amount of history. When it fills, each new message overwrites the oldest ones.

## Reading

`follow` waits for new messages and runs until you stop it. `fetch` reads what the pool holds and exits. Both choose a starting position. Local and HTTP reads deliver in sequence order. UDP reads deliver in arrival order and can include duplicates.

```console
plasmite follow events                        # wait for the next new message
plasmite fetch events                         # every retained message, then exit
plasmite follow events --since oldest         # every retained message, then keep waiting
plasmite fetch events --since 42              # sequence 42 onward
plasmite fetch events --since 5m              # start at the first record at or after five minutes ago
plasmite fetch events --since 2026-11-19T09:00:00Z
plasmite follow events --last 20              # the 20 newest, then keep waiting
```

With no position, `follow` starts at the next new message and `fetch` starts at the oldest retained message. `--since` takes a sequence, `oldest`, a duration such as `5m` or `2h`, or an RFC 3339 time. `--last N` (or `-n N`) starts N messages back from the newest. A read takes one starting position, so combining `--since` and `--last` fails with `Usage`.

A sequence start below the oldest retained message raises `RetentionGap`. Zero and values beyond the next unwritten sequence fail with `Usage`. A time or duration start finds the first retained message at or after the requested time, or starts at the next unwritten sequence if none qualifies. Gap checks begin at the selected position. Later records can carry earlier timestamps if the clock moved backward.

`--tag` filters after Plasmite chooses the start. `--last 100 --tag error` looks at the 100 newest messages and keeps the ones tagged `error`, so it can return fewer than 100. Repeat `--tag` to require every listed tag. Pipe to `jq` to filter on fields inside `data`:

```console
plasmite fetch events --last 100 --tag error
plasmite follow events --tag deploy | jq -c 'select(.data.service == "api")'
```

`fetch` stops at the newest message present when it started, even while writers continue. `follow --one` exits after the first matching message, which makes it a wait in scripts:

```console
plasmite follow builds --tag ci --one
```

`fetch POOL SEQ` reads exactly one message:

| SEQ is | Result | Exit code |
| --- | --- | --- |
| Retained | Prints the message | 0 |
| Already overwritten | `RetentionGap` | 9 |
| The next unwritten sequence | `NotFound` | 3 |
| Beyond the next unwritten sequence, or zero | `Usage` | 2 |

Exact fetch over UDP also exits 3 if the subscription completes without receiving that sequence. Omit `--since`, `--last` and `--tag` when passing a positional sequence.

### Gaps

A reader that falls behind can find that writers overwrote messages it hasn't read. Plasmite never skips them silently. It reports `RetentionGap` with `missing`, the first sequence you missed, and `next_seq`, the oldest sequence still retained:

```json
{"error":{"hint":"Reopen at the next available sequence or rebuild from an authoritative source.","kind":"RetentionGap","message":"messages 1-187 are no longer retained; the next available message is 188","missing":1,"next_seq":188,"seq":1}}
```

`fetch` prints this on stderr and exits 9. `follow` prints it on stderr and continues at `next_seq`. Local and HTTP readers check for gaps before applying `--tag`. In UDP, the server checks for retention gaps before the client applies tags. A missing datagram produces no gap by itself. A UDP range fetch that completes after receiving an oversized or pressure skip notice exits 1.

To resume after a restart, save the `seq` of the last message you handled and start the next run at `--since` that sequence plus one. The [cookbook](cookbook.md#resume-after-a-restart) shows the pattern.

## Writing

`feed POOL [OBJECT]` appends one message per object, taken from the argument or piped through stdin. Local and HTTP feeds print each stored message to stdout. UDP feed prints a notice to stderr and returns without a storage receipt.

```console
plasmite feed events '{"step": "test", "status": "ok"}'
plasmite feed events --tag deploy --tag api '{"sha": "f4e5d6c"}'
jq -c '.[]' readings.json | plasmite feed sensors
```

`data` must be an object. Other values fail with `Usage`. `--create` creates a missing local or HTTP pool at the default size of 1 MiB. Run `pool create POOL --size SIZE` on the server or the local machine to choose another size. UDP requires an existing pool. Invalid input stops `feed` with a nonzero exit, and earlier appends stay in the pool. Empty input succeeds and appends nothing.

### Input formats

By default `feed` detects the stream's format from its opening bytes. It accepts JSON objects separated by whitespace (one object may span lines), YAML documents marked with `---` and `...`, and framed Lite3 starting with `PLS2`. `--input-format json|yaml|lite3|lite3-raw|lines` names one format. Explicit `yaml` also accepts a single unmarked document. YAML input requires JSON-compatible values with string keys and rejects custom tags, aliases, duplicate keys and non-finite numbers. `lite3-raw` reads one Lite3 attachment through end of input. Both explicit Lite3 input modes require stdin and take their tags from the messages.

### Lines

`--input-format lines` stores each UTF-8 line of text as `{"line": "..."}`. It keeps blank lines, removes a trailing `\r`, and stores a final line that lacks a newline. Invalid UTF-8 fails with `Usage` at that line, after the earlier lines are stored.

```console
make 2>&1 | plasmite feed builds --input-format lines
```

### Forwarded messages

An object with exactly `seq`, `time`, `meta` (holding only `tags`) and `data` is a complete message. `feed` keeps its data and tags and gives it a new sequence and time in the destination pool. This lets the output of `fetch` or `follow` flow straight into `feed`:

```console
plasmite follow events | plasmite feed archive
```

A complete message brings its own tags, so adding `--tag` fails with `Usage`. An object with that exact shape but an invalid field (a zero `seq`, a bad timestamp, non-string tags or non-object `data`) also fails with `Usage`. Objects with only some of those fields, or extra ones, are ordinary data.

### Durability and size

`--durability fast`, the default, returns once the message is in the pool. `--durability flush` also waits for storage to flush it to disk.

The encoded message payload can hold up to 256 MiB and the full record must fit in the pool. An HTTP server accepts request bodies up to 1 MiB unless it runs with `serve --max-body-bytes`. UDP accepts datagrams up to 65,507 bytes, including routing and framing, and supports only fast durability.

### Retrying writes

A local flush or remote connection can fail after the pool stores the message. Check the pool before sending it again.

## Output formats

With no `--format`, a terminal gets readable output and a pipe or file gets compact JSON. `--format` sets the output encoding only; `--input-format` sets the input.

| Format | Output |
| --- | --- |
| `pretty` | Readable, indented JSON |
| `json` | One compact JSON message per line |
| `yaml` | One YAML document per message, starting with `---` and ending with `...` |
| `lite3` | A binary stream: the bytes `PLS2`, then each message with its length |
| `lite3-raw` | One message's raw Lite3 bytes; `fetch POOL SEQ` only |

You can pipe JSON, YAML and framed Lite3 directly into `feed`. Use `--input-format lite3-raw` to import a raw attachment. If you force `--format pretty`, also pass `--color never` to produce plain JSON. Reports such as `pool info` follow the same JSON and YAML rules. Binary formats apply to messages only. The [binary stream contract](../spec/v2/SPEC.md#binary-stream) gives the Lite3 layout.

```console
plasmite follow sensors --format lite3 | plasmite feed archive
plasmite fetch sensors 1 --format lite3-raw > reading.lite3
```

Bash and Zsh preserve bytes in a binary pipe. PowerShell 7.4 and later preserves bytes between native commands and when redirecting native stdout to a file. Keep stderr separate in a binary pipeline. For older PowerShell, run the binary pipeline in `cmd.exe`. See [PowerShell redirection](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_redirection#example-7-redirecting-binary-data-from-a-native-command).

## Duplex

`duplex POOL` sends and receives in one session. It detects JSON, marked YAML and framed Lite3 on stdin, including when you type input at a terminal. Duplex opens its reader before it sends the first message, so its own appends are eligible for output along with messages from other writers. Retention can still overtake the reader, and UDP can lose datagrams.

```console
plasmite duplex chat --create --last 20
```

End of input stops sending. Reception continues until you stop the process, the read fails, or Plasmite detects that the downstream command closed the pipe. Use `feed` to copy a finite input and exit. Duplex takes `--since` or `--last` and supports `--create` for local and HTTP pools.

## Errors and exit codes

Errors and notices go to stderr. They use the selected output format for text commands and readable text for binary commands. With no `--format`, Plasmite chooses readable text or JSON based on whether stdout is a terminal.

| Exit code | Error kind | Typical cause |
| --- | --- | --- |
| 0 | | Success |
| 1 | `Internal` | An unexpected failure, including a server error |
| 2 | `Usage` | Bad arguments or invalid input |
| 3 | `NotFound` | Missing pool, or `fetch POOL SEQ` for the next unwritten sequence |
| 4 | `AlreadyExists` | `pool create` for an existing pool |
| 5 | `Busy` | Another process holds the pool, directory or resource |
| 6 | `Permission` | The OS or server refused access |
| 7 | `Corrupt` | Pool file damage |
| 8 | `Io` | A disk or network failure |
| 9 | `RetentionGap` | The read needed messages the pool already overwrote |

Some outcomes that scripts depend on:

- An empty `fetch` exits 0. Text output is empty and framed Lite3 output contains only `PLS2`.
- `follow --one` exits 0 after printing its message.
- On Linux and macOS, `follow` exits 0 when the downstream command closes the pipe, as in `plasmite follow events | head -1`, even while the pool is quiet. Use `--one` in scripts that must also work in PowerShell.
- `follow` reports a retention gap on stderr and keeps running; `fetch` exits 9.
- `pool delete` exits nonzero if any requested deletion fails, and `doctor` exits nonzero if any inspected pool is corrupt.
