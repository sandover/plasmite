# Plasmite Go Bindings (v0)

These bindings wrap the `libplasmite` C ABI via cgo and return JSON message bytes
that match the v0 API spec. Lite3 fast-path APIs accept and return raw Lite3
payload bytes.

## Build Requirements
- Go 1.22+
- `pkg-config` (`pkgconf`) available on PATH
- `libplasmite` SDK installed (recommended on macOS: `brew install sandover/tap/plasmite`)

## Build & Test
From the repo root:

```bash
cargo build -p plasmite
```

Canonical repo-root command:

```bash
just bindings-go-test
```

Equivalent system-SDK command (from `bindings/go`):

```bash
go test ./...
```

Development override (repo-local library without Homebrew install):

```bash
cargo build -p plasmite
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" \
PKG_CONFIG=/usr/bin/true \
CGO_CFLAGS="-I$(pwd)/../../include" \
CGO_LDFLAGS="-L$(pwd)/../../target/debug" \
go test ./...
```

`just bindings-go-test` runs this override automatically for CI and local development.

## Usage

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

msg, err := pool.Append(map[string]any{"kind": "note", "text": "hi"}, []string{"note"}, plasmite.DurabilityFast)
if err != nil {
    // handle err
}
_ = msg

frame, err := pool.GetLite3(1)
if err != nil {
    // handle err
}
_ = frame

ctx := context.Background()
tail, errs := pool.Tail(ctx, plasmite.TailOptions{
    Tags:        []string{"note"},
    Timeout:     100 * time.Millisecond,
})
_ = tail
_ = errs
```

`TailOptions.Tags` applies exact tag matching and composes with other filters via AND semantics.
