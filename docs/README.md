# Plasmite documentation

New to Plasmite? Start with the [README](../README.md): it explains the model in a few minutes.

## Use Plasmite

| Goal | Read |
| --- | --- |
| Install the CLI or a library | [Distribution](record/distribution.md) |
| Copy a recipe for a common task | [Cookbook](cookbook.md) |
| Let AI agents coordinate through pools | [Plasmite for agents](agents.md) |
| Predict what a command does: pool references, reads, formats, exit codes | [CLI guide](cli.md) |
| Share pools, connect a browser or AI client, or fix access | [Serving](record/serving.md) |
| Choose an interface or check performance | [Performance](performance/README.md) |
| Move a 1.x application or script to 2.0 | [Upgrading to 2.0](record/upgrading-2.0.md) |

Each library has its own guide: [Rust](../README-crate.md), [Python](../bindings/python/README.md), [JavaScript](../bindings/node/README.md), [Go](../bindings/go/README.md) and [C](../include/plasmite.h).

## Develop Plasmite

- [Vision](record/vision.md): what Plasmite is for and the principles that settle design questions.
- [Architecture](record/architecture.md): a map of the code and the lasting design choices.
- [Building](building.md), [testing](record/testing.md) and [releasing](record/releasing.md): how to build, check and ship it.
- [Specifications](../spec/README.md): exact behavior. The [2.0 contract](../spec/v2/SPEC.md) covers the CLI, libraries, HTTP, UDP and MCP.
- [Validation reports](validation/README.md): dated checks on physical hardware, virtual machines and hosted CI, with the limits of each.

The Ergo backlog in `.ergo/` tracks unfinished work. Git history keeps superseded proposals and investigations.
