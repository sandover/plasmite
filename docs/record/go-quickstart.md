# Go Quickstart

This guide shows how to use the Go bindings in `bindings/go`.

## Requirements

- Go 1.22+
- `libplasmite` built from this repo

Build libplasmite:

```bash
cargo build -p plasmite
```

## Install / Build

From `bindings/go`:

```bash
CGO_LDFLAGS="-L$(pwd)/../../target/debug" go test ./...
```

On macOS, set `DYLD_LIBRARY_PATH` to `target/debug` when running binaries.
On Linux, set `LD_LIBRARY_PATH`.

## Append Example

```go
client, err := plasmite.NewClient("./data")
if err != nil {
    // handle err
}
defer client.Close()

pool, err := client.CreatePool(plasmite.PoolRefName("docs"), 64*1024*1024)
if err != nil {
    // handle err
}
defer pool.Close()

payload := map[string]any{"kind": "note", "text": "hello"}
msg, err := pool.Append(payload, []string{"note"}, plasmite.DurabilityFast)
if err != nil {
    // handle err
}
_ = msg
```

## Tail Example

```go
ctx, cancel := context.WithCancel(context.Background())
defer cancel()

out, errs := pool.Tail(ctx, plasmite.TailOptions{
    SinceSeq:    nil,
    MaxMessages: nil,
    Timeout:     time.Second,
    Buffer:      32,
})

for msg := range out {
    // msg is a JSON envelope []byte
    _ = msg
}

if err := <-errs; err != nil {
    // handle err (context cancellation ends with ctx.Err())
}
```

## Error Handling

Errors are returned as `*plasmite.Error` with:

- `Kind` (stable error kinds like `ErrorNotFound`)
- `Message` (human-readable string)
- Optional fields: `Path`, `Seq`, `Offset`

Check the error kind before retrying or falling back.

## Binary Blobs

Binary blob helpers are not implemented in v0. Store binary payloads out of band
and keep JSON references in Plasmite messages.
