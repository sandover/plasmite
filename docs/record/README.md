# Docs of record

These documents describe Plasmite as it is now: its policies, its design and how to operate it. For task guides and specifications, start with the [docs index](../README.md).

- [Vision](vision.md): read when deciding whether something belongs in Plasmite.
- [Architecture](architecture.md): read when changing internals, finding where code lives, or tracing a request through the layers.
- [Testing](testing.md): read when choosing which checks to run or adding a test.
- [Releasing](releasing.md): read when preparing a release or changing versions, compatibility or the native library's major version.
- [Distribution](distribution.md): read when installing on a particular platform, adding a channel or target, or linking against the SDK.
- [Secure sharing](serving.md): read when sharing pools, managing access, setting browser trust or operating a server.
- [Upgrading to 2.0](upgrading-2.0.md): read when moving 1.x scripts, listeners, applications and HTTP clients to 2.0.
- [C header](../../include/plasmite.h): read when calling the C library; it states ownership rules and the stability contract.

Proposals, design drafts and roadmap notes do not belong here: the Ergo backlog tracks unfinished design work, and Git history keeps superseded plans. Dated platform checks live in [validation reports](../validation/README.md).
