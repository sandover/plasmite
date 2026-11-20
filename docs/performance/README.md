# Performance

Start with the result your application needs. Sparse field access, full native
objects and unchanged forwarding perform different work; a faster route for one
can cost more for another.

| Question | Read |
| --- | --- |
| Which language interface should I use? | [Language interfaces](language-interfaces.md) |
| How do views affect Rust, Python, Go and Node? | [Owned-view measurements](language-boundary-results/owned-views/README.md) |
| What does HTTP encoding and forwarding cost? | [HTTP views](language-boundary-results/http-views/README.md) |
| How does portable JavaScript compare with JSON? | [Portable views](language-boundary-results/portable-views/README.md) |
| How do UDP and HTTP compare? | [UDP transport](udp-transport.md) |
| How does Plasmite compare with a Python pipe? | [Pipe comparison](pipe-comparison.md) |
| What do local, HTTP and MCP paths cost? | [Transport comparison](transport-comparison.md), [Windows access](windows-access-1.0.md) |
| How many live watchers can a server support? | [Live watcher capacity](watcher-capacity/README.md) |
| What did the 1.0 release change? | [Release measurements](1.0-release.md) |

## Measure a change

Compare the same requested result with fixed before/after builds, alternating
run order and checking output correctness. Include complete producer/consumer
work where it matters. Separate startup from warmed loops and allocation probes
from ordinary timing. Report slower cases, retained memory and noisy controls
alongside improvements. The [testing guide](../record/testing.md) covers checks;
each report links its harness and readable results.

Runnable benchmarks live in [scripts](../../scripts/),
[examples](../../examples/), and the language bindings. The results directories
retain raw samples and environment metadata. Historical results describe their
recorded builds and hosts; rerun the relevant benchmark to judge current code.

## Focused measurements

These reports explain individual costs when changing the implementation:

- [C JSON formatting](language-boundary-results/c-json/README.md).
- [Notification waits](language-boundary-results/wakeups/README.md).
- [Append locking](language-boundary-results/append-descriptors/README.md).
- [Encoding and sizing](language-boundary-results/encoding/README.md).
- [Append receipts](language-boundary-results/append-receipts/README.md).
- [Binary validation](language-boundary-results/core-binary/README.md).
- [Node buffer ownership](language-boundary-results/node-owned/README.md).
