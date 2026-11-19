# Plasmite Go Bindings

These bindings wrap the `libplasmite` C ABI via cgo and expose typed Go contracts.
`Feed` returns the committed `*Message`. `Follow` reads new messages and `Fetch`
reads retained history. Use the view methods for selected reads, typed decoding
and forwarding encoded data.
Owned views keep immutable encoded bytes in Go memory and need no per-message
`Close`. These bindings operate on local pools and UDP endpoints; they do
not expose the Rust/CLI secure native access-key client. Messages disappear
when the bounded ring fills, independent of consumer progress.

See [distribution and SDK setup](https://github.com/sandover/plasmite/blob/main/docs/record/distribution.md) for native
prerequisites and the [1.0 upgrade guide](https://github.com/sandover/plasmite/blob/main/docs/record/upgrading-1.0.md)
for CLI and sharing changes.

## Package Layout

- `github.com/sandover/plasmite/bindings/go/local` — cgo-backed local client and pool operations.
- `github.com/sandover/plasmite/bindings/go/api` — pure Go contracts and shared model types.
- `github.com/sandover/plasmite/bindings/go/udp` — cgo-backed UDP sender and follower.

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

## Usage

```go
package main

import (
    "context"
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

    pool, err := client.OpenPool("events", plasmite.Create(true))
    if err != nil {
        panic(err)
    }
    defer pool.Close()

    live, err := pool.Follow(plasmite.Tags("sensor"))
    if err != nil {
        panic(err)
    }
    defer live.Close()

    stored, err := pool.Feed(map[string]int{"temperature": 23}, []string{"sensor"})
    if err != nil {
        panic(err)
    }
    ctx, cancel := context.WithTimeout(context.Background(), time.Second)
    defer cancel()
    received, err := live.Next(ctx)
    if err != nil {
        panic(err)
    }
    fmt.Println(received.Seq, received.Time, string(received.Data))

    history, err := pool.Fetch(plasmite.Since(stored.Seq))
    if err != nil {
        panic(err)
    }
    defer history.Close()
    replayed, err := history.Next(ctx)
    if err != nil {
        panic(err)
    }
    fmt.Println(string(replayed.Data))
}
```

`Follow` opens on new messages. `Fetch` reads retained history and returns
`io.EOF` after the last message present at opening. Each `Next` takes a
context. `Pool.Info` reports the currently retained range.

### UDP sender and follower

The `udp` package takes an explicit endpoint and opens a named pool. Sending
returns an error only for local validation or operating-system send failures;
a nil error does not confirm storage. A follower yields frames, explicit server
skip notices, and progress events. Its context cancels a blocked wait within
50 milliseconds. The
[receive policy](../../spec/udp/v1/SPEC.md#receive-ordering-and-notices) requires
arrival-order delivery, including late messages and duplicates; sequence/progress
jumps never establish loss or suppress frames. This source change awaits a
major release.
Finite follows end on terminal progress and may leave DATA in flight.

```go
import (
    "context"
    "encoding/json"
    "errors"
    "io"
    "time"

    "github.com/sandover/plasmite/bindings/go/udp"
)

client, err := udp.NewClient("udp://127.0.0.1:9701")
if err != nil {
    panic(err)
}
defer client.Close()

pool, err := client.OpenPool("sensors")
if err != nil {
    panic(err)
}
defer pool.Close()

follow, err := pool.Follow()
if err != nil {
    panic(err)
}
defer follow.Close()

if err := pool.Feed(map[string]any{"temperature": 23}, []string{"sensor"}); err != nil {
    panic(err)
}
ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
defer cancel()
for {
    event, err := follow.Next(ctx)
    if errors.Is(err, io.EOF) {
        break
    }
    if err != nil {
        panic(err)
    }
    if frame, ok := event.(*udp.Frame); ok {
        var data map[string]any
        if err := json.Unmarshal(frame.Message.Data, &data); err != nil {
            panic(err)
        }
        _ = data["temperature"]
        break
    }
}
```

`Next` returns a `*udp.Frame`, `*udp.Skip`, or `*udp.Progress` through the
`udp.Event` interface. Frames include the original Lite3 bytes and a decoded
`*api.Message`. Cancel the context to stop a blocked wait. A UDP follow requires
the server's explicit `--udp-follow` setting; it never switches to HTTP.

### Concurrent use

Client, pool and reader methods serialize access to each native handle. A reader
owns a separate handle and can keep reading after its pool closes. A context
deadline bounds each `Next(ctx)` call, including time spent waiting for another
call on the same reader.

For playback timing, use ordinary streams and an application scheduler; see the
[2.0 upgrade guide](../../docs/record/upgrading-2.0.md#applications-control-playback-timing).

Native readers transfer their fresh JSON buffer into `Message.Raw` without a
second copy. `Message.Data` keeps independent storage, so changing Raw does not
change Data. `api.DecodeMessage` still copies caller-provided raw bytes;
`api.DecodeOwnedMessage` lets an adapter transfer a fresh buffer explicitly.
After that transfer, the caller must not mutate or reuse the input slice.

## Numeric feed receipts

```go
receipt, err := pool.FeedReceipt(map[string]any{"temperature": 23.5}, []string{"sensor"})
// receipt.Seq and receipt.Time are exact uint64 values.
```

`FeedJSONReceipt` accepts JSON bytes and `FeedLite3Receipt` accepts encoded
Lite3 bytes. These calls return the committed sequence and time.

## Managed owned views

`FetchView` and `FollowView` give the caller independent immutable Lite3
snapshots. A view remains valid after the reader closes or the pool overwrites
the message.

```go
reader, err := pool.FetchView(plasmite.Since(seq))
if err != nil { return err }
defer reader.Close()
message, err := reader.Next(ctx)
if err != nil { return err }
var reading struct {
    Sensor string `json:"sensor"`
    Metrics struct { Power int64 `json:"power"` } `json:"metrics"`
}
if err := message.Decode(&reading); err != nil { return err }

selection, err := plasmite.Prepare("/sensor", "/metrics/power")
if err != nil { return err }
defer selection.Close() // reuse this small prepared plan across messages
values, err := selection.Read(message)
if err != nil { return err }
sensor, sensorPresent := values[0].String()
power, powerPresent := values[1].Int64()
fmt.Println(sensor, sensorPresent, power, powerPresent)
```

Identity uses `message.Seq()` and `message.Time()`, which returns Unix nanoseconds. `Data()` and `Meta()` return child references. `Decode`
builds an independent ordinary value with `encoding/json` semantics. `DataJSON`
and `JSON` format the data subtree and canonical complete envelope respectively;
`Meta().JSON()` keeps all stored metadata fields. `Lite3()` returns a detached
mutable copy. Strings and bytes extracted from selected values own their data.
Closing a prepared selector waits for its active reads; message reads need no
pool lock after acquisition. Children retain their original encoded/proof buffers
when a caller reassigns `*MessageView`; copying a wrapper copies only immutable
slice headers, not payload bytes.

The [Go view measurements](../../docs/performance/language-boundary-results/owned-views/go/README.md)
cover sparse reads, typed decoding, full conversion, cgo crossings, allocation
counts and the initial-buffer memory tradeoff. Complete exact structs use keyed lookups. Partial ASCII structs scan only
selected objects and skip unrequested subtrees; Unicode/ambiguous aliases and
complex Go types keep the standard JSON fallback. The measurements include full
conversion costs and the small-read buffer retention tradeoff.

## Native producers

`FeedView` walks ordinary Go values into Lite3 and returns their committed
snapshot. `FeedJSONView` accepts explicit JSON source. Both return Go-managed
views.

```go
message, err := pool.FeedView(
    map[string]any{"sensor": "north", "samples": []byte{0, 127, 255}},
    []string{"sensor"},
)
if err != nil { return err }

// Forward the whole encoding and tags, with a new identity.
forwarded, err := pool.FeedView(message, nil)
if err != nil { return err }
fmt.Println(forwarded.Seq())
```

Native signed/unsigned integers must fit i64. Finite floats stay binary64;
float32 promotes its exact value. Native `[]byte` becomes Lite3 bytes: nil becomes
null and a nonnil empty slice stays empty bytes. Explicit JSON and custom
`MarshalJSON` output keep their JSON types, including base64 strings. Standard
JSON exports format native bytes as base64; typed `Decode` and `Value.Bytes`
produce independent mutable byte slices.

The encoder follows Go's field promotion, tag dominance, `omitempty`, `string`,
map-key `TextMarshaler` and addressable pointer-method rules. Go 1.24 and newer
also honor `omitzero`; older supported compilers ignore it as their JSON library
does. JSON/Text hooks run once in the usual visit order. Map values snapshot
before sorted-key callbacks; mutable byte values snapshot at visit, so a later
hook cannot rewrite an earlier value. Custom JSON subtrees parse once, while
ordinary floats and other fields stay native.

The binding pins immutable strings and private view buffers only during its
synchronous native call. C retains no Go pointers. It releases the pool mutex
before copying the checked producer snapshot into aligned Go memory, and reuses
the construction proof. Closing a pool inside a serializer causes a closed-handle
error before publication. `json.Marshal(message)` and `json.Marshal(message.Data())`
use the normal Go marshaler interface for explicit JSON conversion.


## Owned local and UDP streams

`FollowView` and `FetchView` return readers with `Next(ctx)` and `Close()`.
They take the same `Since`, `Last` and `Tags` options as `Follow` and `Fetch`.
Use the context to set a deadline or cancel a wait. The reader can outlive the
pool handle.

```go
stream, err := pool.FollowView(local.Since(first))
if err != nil { return err }
defer stream.Close()
message, err := stream.Next(ctx)
if err != nil { return err }
var reading struct { Value int64 }
if err := message.Decode(&reading); err != nil { return err }
forwarded, err := destination.FeedView(message, nil)
```

Whole-message `FeedView(message, nil)` retains the exact payload and tags, commits
it with fresh destination identity and shares the immutable Go backing storage.
A nonnil tag slice is an error when forwarding a message. Feed `message.Data()`
to supply new tags. Independent mutable
exports still copy. An append option can close the destination or reassign the
caller's source variable without changing the source snapshot already visited.

`udp.Pool.FollowView` uses the `Follow.Next(ctx)`, `Cancel` and `Close`
lifecycle. DATA arrives as `*udp.ViewFrame` with an ordinary
`*local.MessageView` in `View`; Skip and Progress keep their existing types.
`FollowLite3` returns the exact owned payload. `Follow` also decodes the message.
All three retain arrival order, late and
duplicate packets, server skip/progress records and finite completion. No
receive adapter infers packet loss or adds retransmission.

A native receiver validates each incoming owner once. Go copies its bytes and
relative duplicate-key proof into aligned private Go allocations, then releases
the native owner. It performs no second validation and builds no data tree.
Both the C receiver snapshot and the final Go snapshot copy payload bytes; the
binding retains Go-managed storage without a per-message Close. Children and
forwarded messages remain valid after source pool/follow/stream close.

[Go stream benchmark coverage and fixed builds](../../docs/performance/language-boundary-results/owned-views/go/streams/README.md)
separate receive/copy costs, explicit conversion, forwarding and packet pacing.

## Follow and fetch

Open a reader with `pool.Follow()` for new messages or `pool.Fetch()` for
retained history. Pass `Since` or `Last` to select a starting point and use
`Tags` to require tags. `Since` accepts an inclusive sequence, `"oldest"`,
a duration, or an instant marked by `At`. Each `Next(ctx)` call uses its own
context and leaves the reader open if the context expires. Call `Close` when
you finish reading.

### HTTPS pools

Save a connection with `plasmite access connect https://pools.example.net:9743`, then pass the HTTPS pool reference to the same client.

```go
pool, err := client.OpenPool("https://pools.example.net:9743/events", plasmite.Create(true))
if err != nil { return err }
defer pool.Close()
_, err = pool.Feed(map[string]any{"ready": true}, []string{"status"})
```

`CreatePool` accepts the same reference and an explicit size. Use `WithDurability` on any feed variant to request a flush.
