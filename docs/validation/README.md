# Validation reports

These reports record checks against particular builds and machines on particular dates. Each describes what was observed and what the check does not establish. Current setup instructions live in the [cookbook](../cookbook.md), the [serving guide](../record/serving.md) and [distribution](../record/distribution.md).

These reports predate 2.0. Repeat a check before claiming its result for 2.0.

- [Windows and Mac services](windows-boot-validation.md): native Windows service startup before sign-in, updates and rollback, and a macOS LaunchDaemon trial (October 2026, `1.1.0-dev`).
- [Debian VM services](debian-vm-validation.md): startup before login, reboot recovery, connection failures and retained sensor history (November 2026, `1.3.0-dev`).
- [Linux ARM SDK CI run](arm-sdk-ci.md): the hosted CI run that first built and exercised the ARM64 and ARMv7 archives.
- [Dependency audit, February 2026](dependency-audit-2026-02-08.md): a 1.x-era review of dependency and supply-chain risk. Run `just audit` for current advisories.

## Recording a check

Automated tests cannot show that a server starts before anyone logs in, or that an archive works on physical hardware. Record those claims here as dated reports.

For an installed server, on each host:

1. Install as the account that owns the pool and access keys; on Windows, approve the User Account Control prompt with that account. Check `plasmite serve status --all`, the public URL, the listener and a client request.
2. On a host whose storage mounts without sign-in, reboot and leave the owner signed out. From another machine, read a pool over HTTPS through a saved connection. If a disk needs a person to unlock it, test recovery after the unlock and record that narrower result.
3. Sign in as the owner. Check `plasmite serve status --all` and `plasmite serve logs`, then run `stop`, `start`, `restart` and `uninstall` for that directory.

For a Raspberry Pi, record the model, OS and architecture, archive version, the commands run and the pool sizes exercised. Hosted CI, a service restart and a physical reboot establish different facts, so name which one each result comes from.

No physical Linux or Raspberry Pi boot check has passed yet. A Debian VM result covers its tested software and environment, not a physical Pi or recovery from power loss.
