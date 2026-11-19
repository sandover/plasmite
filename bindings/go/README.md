# Plasmite for Go

Plasmite stores JSON messages in pools: bounded ring buffers on disk that many processes can write and read at once. `Feed` writes a message, `Follow` waits for new ones, and `Fetch` reads what the pool holds and stops. Each message carries a sequence number (`Seq`), a time, tags and a JSON object as `Data`. When a pool fills, the oldest messages make room for new ones. These packages open local pools, pools on a shared HTTPS server and UDP endpoints. The [project README](https://github.com/sandover/plasmite#readme) explains the model and the CLI.

## Install

```bash
go get github.com/sandover/plasmite/bindings/go/v2/local@v2.0.0
```

The bindings use cgo and need Go 1.22 or newer, `pkg-config` and the Plasmite 2.x SDK. On macOS, `brew install sandover/tap/plasmite` installs the SDK; [distribution](https://github.com/sandover/plasmite/blob/main/docs/record/distribution.md) covers other platforms.

For an SDK unpacked into a private directory, point `PKG_CONFIG_PATH` at its `lib/pkgconfig` directory. For programs that ship with a fixed SDK path, set `CGO_LDFLAGS="-Wl,-rpath,$(pkg-config --variable=libdir plasmite)"` at build time so the system loader can find the shared library. You can also set `LD_LIBRARY_PATH` on Linux or `DYLD_LIBRARY_PATH` on macOS when running the program.

| Package | Contents |
| --- | --- |
| `bindings/go/v2/local` | Clients and pools, local or HTTPS |
| `bindings/go/v2/udp` | UDP sender and follower |
| `bindings/go/v2/api` | Shared types such as `Message` and `Error`, with no cgo |

## Quick start

Record a temperature reading, watch for it, then read recent history. The examples in this README use the `must` helper shown here to keep error handling short.

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

func must[T any](value T, err error) T {
	if err != nil {
		log.Fatal(err)
	}
	return value
}

func main() {
	ctx := context.Background()
	client := must(plasmite.NewClient("./data"))
	defer client.Close()
	pool := must(client.OpenPool("sensors", plasmite.Create(true)))
	defer pool.Close()

	live := must(pool.Follow(plasmite.Tags("kitchen")))
	must(pool.Feed(map[string]any{"temp_c": 21.4}, []string{"kitchen"}))
	fmt.Println(string(must(live.Next(ctx)).Data)) // {"temp_c":21.4}
	live.Close()

	recent := must(pool.Fetch(plasmite.Last(10)))
	defer recent.Close()
	for {
		message, err := recent.Next(ctx)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			log.Fatal(err)
		}
		fmt.Println(message.Seq, message.Tags(), string(message.Data))
	}
}
```

`Follow` fixes its position before it returns, so it sees the feed that comes after it. `Fetch` returns `io.EOF` after the last message present when it opened. `Message.Data` holds the raw JSON and `Message.Time` counts Unix nanoseconds. `pool.Info()` reports the oldest and newest retained sequence in `Bounds`. `NewDefaultClient()` uses the default directory, `~/.plasmite/pools`. Use `client.CreatePool(ref, sizeBytes)` to choose a size; `Create(true)` makes a 1 MiB pool.

## Reading

Every read starts at a position. `Follow` starts at the next new message by default; `Fetch` starts at the oldest retained one.

```go
plasmite.Since("oldest")                         // everything the pool still holds
plasmite.Since(42)                               // sequence 42 onward, inclusive
plasmite.Since(5 * time.Minute)                  // or Since("5m")
plasmite.Since("2026-11-19T09:00:00Z")           // RFC 3339, or a time.Time
plasmite.Since(plasmite.At(1795078800000000000)) // the same time as Unix nanoseconds
plasmite.Last(20)                                // the 20 most recent messages
plasmite.Tags("error")                           // only messages carrying every listed tag
```

Pass these options to `Fetch` or `Follow`, as in `pool.Fetch(plasmite.Since("oldest"), plasmite.Tags("error"))`. Use `Since` or `Last`, not both.

Resume after a restart by saving the next sequence to read. If the pool overwrote messages before you read them, opening the reader or calling `Next` can return `RetentionGap`. This example also uses `os` and `strconv`.

```go
start := any("oldest")
saved, err := os.ReadFile("sensors.seq")
if err == nil {
	start = must(strconv.ParseUint(string(saved), 10, 64))
} else if !errors.Is(err, os.ErrNotExist) {
	log.Fatal(err)
}
save := func(seq uint64) {
	if err := os.WriteFile("sensors.seq", []byte(strconv.FormatUint(seq, 10)), 0o644); err != nil {
		log.Fatal(err)
	}
}

read := func() error {
	reader, err := pool.Follow(plasmite.Since(start))
	if err != nil {
		return err
	}
	defer reader.Close()
	for {
		message, err := reader.Next(ctx)
		if err != nil {
			return err
		}
		fmt.Println(string(message.Data))
		save(message.Seq + 1)
	}
}
for {
	err := read()
	var gap *plasmite.Error
	if errors.As(err, &gap) && gap.Kind == plasmite.ErrorRetentionGap {
		log.Printf("lost %d..%d; resuming at %d", gap.Missing, gap.NextSeq-1, gap.NextSeq)
		save(gap.NextSeq)
		start = gap.NextSeq
		continue
	}
	log.Fatal(err)
}
```

`gap.Missing` is the first sequence you missed and `gap.NextSeq` the oldest one still retained. Saving `NextSeq` accepts the loss and moves on.

Each `Next` takes a context. Cancelling that context stops the wait and leaves the reader open. A native file operation already waiting on an operating system lock can delay the return. Call `Close` when you finish reading.

## HTTPS pools and UDP

To use a pool on a shared server, connect once with `plasmite access connect https://pools.example.net:9743`, then open the pool by URL. Client-wide `ListPools()` still lists the local directory.

```go
pool := must(client.OpenPool("https://pools.example.net:9743/sensors", plasmite.Create(true)))
defer pool.Close()
must(pool.Feed(map[string]any{"temp_c": 21.4}, []string{"kitchen"}))
```

UDP trades delivery guarantees for low overhead. A nil error from a send means the operating system accepted the datagram. Reads deliver frames in arrival order, including duplicates and late arrivals. Before running this example, create `sensors` with `plasmite pool create sensors` and run `plasmite serve --udp-follow` in another terminal.

```go
import "github.com/sandover/plasmite/bindings/go/v2/udp"

client := must(udp.NewClient("udp://127.0.0.1:9701"))
defer client.Close()
pool := must(client.OpenPool("sensors"))
defer pool.Close()
if err := pool.Feed(map[string]any{"temp_c": 21.4}, []string{"kitchen"}); err != nil {
	log.Fatal(err)
}
events := must(pool.Follow(udp.Since("oldest")))
defer events.Close()
for {
	if frame, ok := must(events.Next(ctx)).(*udp.Frame); ok {
		fmt.Println(frame.Seq, string(frame.Message.Data))
	}
}
```

A UDP read also yields `*udp.Skip` (a range the server skipped) and `*udp.Progress` events. A finished `Fetch` returns `io.EOF`. The [UDP spec](https://github.com/sandover/plasmite/blob/main/spec/udp/v1/SPEC.md) defines delivery and limits.

## Errors and concurrency

Plasmite failures are `*plasmite.Error` values (an alias of `api.Error`). `Kind` holds one of the nine error kinds: `ErrorUsage`, `ErrorNotFound`, `ErrorAlreadyExists`, `ErrorBusy`, `ErrorPermission`, `ErrorCorrupt`, `ErrorIO`, `ErrorRetentionGap` and `ErrorInternal`. `Path`, `Seq` and `Offset` are set when known, and `Missing` and `NextSeq` for a retention gap. Use `errors.As` to reach them.

Clients, pools and readers serialize calls on their own native handle, so goroutines can share them. Separate handles can run concurrently. A reader owns its own handle and keeps working after its pool closes. A context deadline also limits the wait behind another `Next` on the same reader.

## Advanced

These methods avoid conversion work when you forward messages or read a few fields from large ones. The [language interface measurements](https://github.com/sandover/plasmite/blob/main/docs/performance/language-interfaces.md) show when each one pays off.

**Views.** `FetchView` and `FollowView` open readers that yield immutable `*MessageView` values. `FeedView` returns the view for the message it wrote. A view lives in Go memory, needs no `Close`, and stays valid after the reader closes or the pool overwrites the message.

```go
reader := must(pool.FetchView(plasmite.Last(1)))
view := must(reader.Next(ctx))
reader.Close()

var reading struct {
	TempC float64 `json:"temp_c"`
}
must(0, view.Decode(&reading)) // fills the struct by encoding/json rules
fmt.Println(view.Seq(), reading.TempC)
must(pool.FeedView(view, nil)) // forward: new seq and time, same data and tags
```

`Prepare("/temp_c")` compiles JSON Pointer paths once. Call `selector.Read(view)` to read those fields from each message, then call `selector.Close()` when you finish. `view.JSON()` formats the full envelope and `view.Lite3()` copies the encoded bytes. `FeedView` also accepts Go values that encode as a data object. Inside that object, `[]byte` becomes native bytes. Pass `view.Data()` with new tags to retag a message. `udp.Pool.FollowView` delivers `*udp.ViewFrame` events that carry the same views. See the [Go view measurements](https://github.com/sandover/plasmite/blob/main/docs/performance/language-boundary-results/owned-views/go/README.md) and [stream measurements](https://github.com/sandover/plasmite/blob/main/docs/performance/language-boundary-results/owned-views/go/streams/README.md).

**Receipts.** `FeedReceipt`, `FeedJSONReceipt` and `FeedLite3Receipt` return only `Seq` and `Time`, skipping the committed message.

**Lite3.** `FeedLite3`, `FetchLite3` and `FollowLite3` move the binary encoding Plasmite stores. Reads yield `*Lite3Frame` values with `Seq`, `Time` and `Payload`.

**Durability.** For local and HTTPS pools, pass `plasmite.WithDurability(plasmite.DurabilityFlush)` to ask Plasmite to flush the write before reporting success.

## Building from source

From the repo root, build and stage the native library and run the binding tests.

```bash
just bindings-go-test
```

With an installed SDK, `go test ./...` in `bindings/go` runs the same tests. The `just` recipe points cgo and the system loader at the repo's build.
