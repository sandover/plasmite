# Linux ARM SDK CI run

On October 1, 2026 (UTC), hosted CI built and exercised both Linux ARM SDK archives at commit `b3fd007`. The archives carried package version `0.8.0`. Read [distribution](../record/distribution.md#linux-arm-sdk-preview-install) for current installation and support tier.

## What ran

The [ARM SDK CI run](https://github.com/sandover/plasmite/actions/runs/36803923793) built and packaged `aarch64-unknown-linux-gnu` and `armv7-unknown-linux-gnueabihf`. For each target it extracted the archive, compiled and linked a C SDK consumer, exercised the CLI and the HTTPS restart path, and ran the `cli_pool`, `secure_serving`, `secure_sharing`, `oauth_access`, `access_lifecycle` and `remote_integration` suites.

- ARM64 ran natively on `ubuntu-22.04-arm` with glibc 2.35.
- ARMv7 cross-built on Ubuntu 22.04 against a glibc 2.35 hard-float sysroot and ran under QEMU. It also passed shared-pool handoffs from a 64-bit CLI to an ARMv7 CLI and back.

The run uploaded `ci-sdk-aarch64-unknown-linux-gnu` and `ci-sdk-armv7-unknown-linux-gnueabihf`, each with `sha256sums.txt` and `ci-build.json`, with a 30-day retention period. Releases publish their own ARM archives as preview assets.

## Limits

Hosted CI and QEMU do not establish behavior on physical Raspberry Pi hardware, installation by hand, or startup after a reboot. No physical Pi check has passed yet.
