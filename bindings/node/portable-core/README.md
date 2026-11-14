# Checked portable Node views

Remote-only installations use the same checked Lite3 snapshots and input builder
as native bindings. The private JavaScript adapter supplies the existing Proxy
protocols; it does not parse or write the physical format. The packaged Wasm
module needs no native addon, compiler, WASI runtime, files, network or clock
imports. The host loader targets Node 24+.

`portable.js` privately exposes asynchronous `decodeMessage(bytes, seq, ns)` and
`encodeMessage(data, tags)` operations. Their capture work runs synchronously
before returning a Promise. Lazy synchronous compilation avoids suspending with
mutable input or constructing an intermediate capture tree on the first call.
`messageWithIdentity` creates another message wrapper over the same backend.

The reader validates the complete envelope once and retains the resulting
snapshot. Property lookup, bounded iteration, duplicate-key resolution, exact
integers and JSON formatting use the shared Rust/C implementation. Small stored
integers cross as numbers; unsafe i64s cross as bigint. The adapter reads safe
integer words without creating a temporary bigint, and writes safe integers as
two 32-bit words rather than boxing each value for a Wasm i64 argument.

Explicit `toNative` may use the VM's JSON parser when the immutable owner's
checked scalar proof permits it. Unsafe integers and bytes use exact traversal.
This optimization builds only a requested native result; views retain Lite3 as
the record throughout their lifetime.

## Ownership and copies

| Operation | Memory work |
| --- | --- |
| Decode received bytes | One copy into private Wasm memory, then checked admission; no retained caller alias. |
| Read a field/child | A scalar or small Proxy wrapper; children share the original owner. |
| Walk an array | A fixed 16-entry cursor; no collection-sized descriptor buffer. |
| Encode ordinary input | UTF-8/bytes enter a reusable per-operation workspace; the checked builder copies them into its encoding. |
| Export an encoded HTTP body | One independent Buffer copy out of Wasm memory. |
| Forward a complete MessageView with omitted tags | Copy its existing encoding for the HTTP body; preserve tags and avoid rebuilding. |
| Supply explicit tags or copy a subtree | Write the required checked encoding; avoid JSON and native data trees. |
| Make a mutable native/byte export | Copy the requested result. |

Encoded-only producers release their temporary Wasm owner immediately. Read
views use garbage collection: live children retain their full snapshot, and the
last wrapper's finalizer releases it. Linear memory grows in pages and does not
shrink; the allocator reuses freed space. A fixed 256-byte lookup workspace
survives between calls; larger key workspaces free after lookup. New memory views
always follow memory growth, so no stale ArrayBuffer alias reaches the caller.

The module caps linear memory at 1 GiB; the shared writer caps one encoding at
256 MiB. Aggregate live snapshots, temporary encoding copies and allocator
fragmentation can exhaust the module earlier. Garbage collection/finalizers have
no immediate-release guarantee. Diagnostic backing-byte counts exclude owner
metadata, workspace overhead and allocator fragmentation; linear-memory size
reports the whole reserved high-water buffer.

## Maintainer build

Use the repository's pinned Rust toolchain and the official
[WASI SDK 34](https://github.com/WebAssembly/wasi-sdk/releases/tag/wasi-sdk-34).
Only its compiler, headers and standalone `memchr` object enter the build. C
allocations use Rust's heap; a second libc allocator or WASI startup does not
link into the module. The final import check enforces that boundary. The package
includes the upstream musl copyright notice for the standalone `memchr` code.

```sh
rustup target add wasm32-unknown-unknown
WASI_SDK_PATH=/path/to/wasi-sdk-34.0-arm64-macos npm run build:portable
npm run check:portable
npm run test:portable
```

The Rust target follows the [Rust target contract](https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html).
`PLASMITE_PORTABLE_BUILD_DIR` selects an isolated build directory. Set
`CARGO_NET_OFFLINE=true` to reuse cached dependencies without network access.
The build writes `portable/plasmite_core.wasm` and a manifest with compiler,
artifact and shared-source hashes. Normal npm installs use those checked-in
files and run no build. Package tests install a tarball containing no native
artifacts and exercise exact views, binary input and conversion.

Shared `abi_builder` has a storage-free internal sealing step; host appends use
the same step before committing. Its public C signatures stay unchanged. The
portable crate imports shared reader/writer source files directly, including
all validation and duplicate rules. Its 32-bit `DocRef` import keeps the native
method signature and documents the target-specific Clippy convention exception.

The private HTTP `prepareAppend(data, tags)` captures input synchronously and
returns `{payload, message, discard}`. `payload` owns an independent Buffer;
`message` retains the checked encoding that produced it. After receipt,
`messageWithIdentity` supplies committed sequence and time without importing or
validating the body again. On request failure, the caller invokes `discard()`
to free a newly built unpublished owner. Forwarding a whole MessageView with
omitted tags shares its caller-owned backend, so `discard()` does nothing.
Explicit tags rebuild one owner. Successful requests leave release to the
returned view's lifetime. The encoded-only helper still releases its temporary
owner immediately.
