# Upgrading CLI scripts to 2.0

Plasmite 2.0 makes complete messages the common unit of CLI pipes. This is a
hard cutover: old switches fail rather than selecting compatibility behavior.
Pool files, language APIs, HTTP and MCP keep their existing contracts.

## Copy messages with defaults

```bash
plasmite follow source | plasmite feed destination
plasmite follow source | jq -c '.data.temperature += 1' | plasmite feed destination
plasmite follow source --tail 100 --no-follow > messages.jsonl
plasmite feed destination < messages.jsonl
```

The destination keeps contents and tags, and assigns its own sequence and time.
`feed` writes complete newly appended messages, so another `feed` can continue
the chain. The reader processes each complete message immediately.

## Create messages explicitly

Before 2.0, feed treated every JSON object as application contents. Add `--data`
when that is your intent:

```bash
plasmite feed events --data '{"temperature":21}'
plasmite feed events --data --tag sensor '{"temperature":21}'
jq -c '.[]' readings.json | plasmite feed events --data
```

Without `--data`, feed requires a complete message with object `data` and
optional `meta.tags`. Unknown message/meta fields fail. To extract application
contents, use `jq -c '.data'`; a following feed needs `--data` to wrap them.

## Choose one output format

| Before | 2.0 |
| --- | --- |
| `--json`, `--jsonl`, `--format jsonl` | `--format json`, or omit it in a pipe/file |
| `--yaml` | `--format yaml` |
| `fetch --format lite3` | `fetch --format lite3-raw` for an attachment; `--format lite3` for a stream |
| `feed --in lite3` | `feed --input-format lite3-raw` |
| `feed --in jsonl` or `--in jq` | Omit it; JSON objects stream across whitespace/newlines |
| `--data-only` | `jq -c '.data'` |
| `--errors skip` | Removed; bad input stops and earlier appends remain |

Readable output remains the terminal default. Pipes and files default to compact
JSON. Global `--format` works before or after the command. It chooses output;
`feed --input-format` chooses input when automatic syntax detection cannot.

YAML output now works as feed input. Lite3 output includes an identifying prefix
and per-message lengths, so one and many messages use the same format. Raw Lite3
has a separate explicit spelling because EOF supplies its only boundary.

## Check completion

`access status` returns nonzero when the server is unreachable or rejects access.
A finite history read returns nonzero if retention removes selected messages.
Live follow reports dropped messages and continues. If stdout reports a closed
pipe, the command stops normally and makes no further appends. Other output
failures return nonzero; earlier appends remain stored.
