# Plasmite Go Bindings (v0)

These bindings wrap the `libplasmite` C ABI via cgo and return JSON message bytes
that match the v0 API spec.

## Build Requirements
- Go 1.22+
- `libplasmite` built from this repo (`cargo build -p plasmite`)

## Build & Test
From the repo root:

```bash
cargo build -p plasmite
```

Then from `bindings/go`:

```bash
CGO_LDFLAGS="-L$(pwd)/../../target/debug" go test ./...
```

For release builds, swap in `target/release` and run `cargo build -p plasmite --release`.
On macOS, set `DYLD_LIBRARY_PATH` to the same directory when running binaries.
On Linux, use `LD_LIBRARY_PATH`.

## Usage

```go
client, err := plasmite.NewClient("./data")
if err != nil {
    // handle err
}
defer client.Close()

pool, err := client.CreatePool("docs", 64*1024*1024)
if err != nil {
    // handle err
}
defer pool.Close()

msg, err := pool.AppendJSON([]byte(`{"kind":"note","text":"hi"}`), []string{"note"}, plasmite.DurabilityFast)
if err != nil {
    // handle err
}
_ = msg
```
