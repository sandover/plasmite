# Platform validation

These reports record checks against particular builds and machines. They describe
the observed behavior and remaining limits; current setup instructions live in
the [cookbook](../cookbook.md) and [serving guide](../record/serving.md).

- [Windows boot and access](windows-boot-validation.md): native startup, remote
  access and recovery checks.
- [Debian VM services](debian-vm-validation.md): startup before login, reboot
  recovery, connection failures and retained sensor history.

Raspberry Pi hardware checks remain in the backlog. A Debian VM result covers
its tested software and environment, not a physical Pi or power-loss recovery.
