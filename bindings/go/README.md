# Plasmite Go Bindings

These bindings wrap the `libplasmite` C ABI via cgo and expose typed Go contracts.
`Append`, `Get`, `Tail`, and `Replay` return `*Message` values, while Lite3 APIs
provide raw binary frames. These bindings operate on local pools; they do
not expose the Rust/CLI secure native access-key client. Messages disappear
when the bounded ring fills, independent of consumer progress.

See [distribution and SDK setup](https://github.com/sandover/plasmite/blob/main/docs/record/distribution.md) for native
prerequisites and the [1.0 upgrade guide](https://github.com/sandover/plasmite/blob/main/docs/record/upgrading-1.0.md)
for CLI and sharing changes.

## Package Layout

- `github.com/sandover/plasmite/bindings/go/local` — cgo-backed local client and pool operations.
- `github.com/sandover/plasmite/bindings/go/api` — pure Go contracts and shared model types.

Install the local binding package in a downstream module:

```bash
go get github.com/sandover/plasmite/bindings/go/local
```

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

Conformance runner (manifest parity with Rust/Node/Python):

```bash
cargo build -p plasmite
cd bindings/go
DYLD_LIBRARY_PATH="$(pwd)/../../target/debug${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" \
LD_LIBRARY_PATH="$(pwd)/../../target/debug${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
PKG_CONFIG=/usr/bin/true \
CGO_CFLAGS="-I$(pwd)/../../include" \
CGO_LDFLAGS="-L$(pwd)/../../target/debug" \
go run ./cmd/plasmite-conformance ../../conformance/sample-v0.json
```

## Usage

```go
package main

import (
    "context"
    "errors"
    "fmt"
    "time"

    plasmite "github.com/sandover/plasmite/bindings/go/local"
)

func main() {
    client, err := plasmite.NewClient("./data")
    if err != nil {
        panic(err)
    }
    defer client.Close()

    pool, err := client.Pool(plasmite.PoolRefName("docs"), 64*1024*1024)
    if err != nil {
        panic(err)
    }
    defer pool.Close()

    msg, err := pool.Append(
        map[string]any{"kind": "note", "text": "hi"},
        []string{"note"},
        plasmite.WithDurability(plasmite.DurabilityFast),
    )
    if err != nil {
        panic(err)
    }
    fmt.Println(msg.Seq, msg.Tags())
    fmt.Println(string(msg.Data))

    fetched, err := pool.Get(msg.Seq)
    if err != nil {
        panic(err)
    }
    fmt.Println(string(fetched.Data))

    same, err := client.Pool(plasmite.PoolRefName("docs"), 0) // open or create
    if err != nil {
        panic(err)
    }
    defer same.Close()

    frame, err := pool.GetLite3(msg.Seq)
    if err != nil {
        panic(err)
    }
    fmt.Println(frame.Seq, frame.Time())

    ctx, cancel := context.WithCancel(context.Background())
    defer cancel()
    one := uint64(1)
    tail, errs := pool.Tail(ctx, plasmite.TailOptions{
        SinceSeq:    &msg.Seq,
        MaxMessages: &one,
        Tags:        []string{"note"},
        Timeout:     100 * time.Millisecond,
    })
    for item := range tail {
        fmt.Println(item.Seq, item.Tags(), string(item.Data))
    }
    if err := <-errs; err != nil && !errors.Is(err, context.Canceled) {
        panic(err)
    }
}
```

`TailOptions.Tags` applies exact tag matching and composes with other filters
via AND semantics. `SinceSeq` is inclusive; save the last handled sequence
plus one to resume. `Timeout` bounds each blocking read so the goroutine can
check cancellation; it does not end the tail on its own. Use a context deadline
to bound the whole operation, or `MaxMessages` to stop after a chosen number
of delivered messages.

Local tails continue from the next retained message by default when their
cursor falls behind the ring buffer. Set `ErrorOnGap` when lost messages must
stop processing:

```go
ctx, cancel := context.WithCancel(context.Background())
defer cancel()
var handlerErr error
tail, errs := pool.Tail(ctx, plasmite.TailOptions{
    SinceSeq:   &checkpoint,
    ErrorOnGap: true,
})
for message := range tail {
    if err := handle(message); err != nil {
        handlerErr = err
        cancel() // unblock the producer before waiting for its error channel
        break
    }
    checkpoint = message.Seq + 1
}
if err := <-errs; err != nil && !errors.Is(err, context.Canceled) {
    var plasmiteErr *plasmite.Error
    if errors.As(err, &plasmiteErr) &&
        plasmiteErr.Kind == plasmite.ErrorRetentionGap {
        // Choose a new checkpoint or rebuild state before restarting.
    }
}
if handlerErr != nil {
    // Report the handler failure; checkpoint still names the unhandled message.
}
```

The error channel receives `ErrorRetentionGap` before the first message or
Lite3 frame after the gap. Tag filters do not conceal the error.

### Concurrent use

Client, Pool, Stream, and Lite3Stream methods serialize access to each native
handle, including Close. A Tail goroutine can reopen its independent stream
while foreground pool operations run. Handles must not be copied. Close waits
for an active native read; set a finite stream timeout for bounded shutdown.
Independent handles may run concurrently, and message values are owned snapshots.
