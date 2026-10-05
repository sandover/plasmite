# Windows boot service validation

This report records the Windows service checks from October 5, 2026.
Read [serving](serving.md) for installation and operation.

## Candidate and environment

The implementation commit is `68851df`, based on main `6d32e1f`; command
help and operating guidance follow in `7cac029`. The configured VMware Fusion
VM runs ARM64 Windows. These checks used the official x64 target under
emulation, Rust 1.88.0, MSVC 14.44.35207 and clang-cl 23.1.2.

The guest received a source archive without Git metadata. Its final executable
reports `1.1.0-dev+unknown`; SHA-256 identifies the tested and installed bytes:
`15aba12c6a54775cca911ea4805ceaee389b396dc405106c191b97e60f28f039`.

## Native service and permissions

The selected pool path contains spaces and Unicode. Windows runs its automatic
service under a pool-specific virtual account, with no stored login password.
Checks covered local read/write, discovery, logs, owner CLI controls, native
start/stop, crash recovery, two independent services, uninstall preservation,
and refusal of foreign services and unrecognized installation files.

Effective permission checks denied BUILTIN Users, LOCAL SERVICE and the other
fixture service access to the selected private state. AccessChk simulated these
principals; this check did not create a separate ordinary-user login. Restricted
token tests checked parent-directory pinning without administrator bypass.
Saved client credentials remained owner-only. New services stayed disabled until
the protected executable, setup and permissions existed.

## Updates and recovery

A build with a distinct version and executable hash replaced the candidate,
then the candidate replaced it. Repeating installation without options retained
settings. Moving the original launcher aside did not affect the installed copy.

A held executable refused replacement and restored the prior running service.
An occupied listener rolled back the executable, setup and identity for both
running and stopped services. The stopped case remained stopped for at least
15 seconds after the command ended. A held operation lock refused concurrent
installation and control without changing the running service.

Holding the rollback executable forced repair to fail. Plasmite stopped and
disabled the service, cleared crash recovery, and retained the named executable,
setup and identity backups. Repair restored those files and the native policies;
the server then accepted its saved credentials and retained pool data.

During reboot preparation, one running-service update refused file replacement
with Windows error 5 and restored the exact prior running service. An explicit
normal stop followed by the same installation succeeded. The check did not
establish the cause of this intermittent refusal.

Explicit TLS inputs became retained private copies. Moving the original files
aside left restart and repeated installation working. Invalid and mismatched
inputs failed before native changes. A different valid certificate combined with
an occupied listener rolled back to the prior certificate and identity.
Key comparisons checked IDs, names, creation times, revocations and secret hashes;
authenticated use advanced only the expected last-used timestamps.

## Boot and remote access

The first reboot reached the lock screen, but the owner entered the PIN during
its network delay. That run cannot establish startup before sign-in.

The second reboot left Windows at the lock screen with no interactive user.
The test kept the selected network adapter disabled for three minutes after boot.
The service recorded repeated selected-address bind failures (Windows error
10049), and the Service Control Manager kept retrying automatically. The test
task enabled the adapter at 20:35:36 UTC and observed its configured address at
20:35:38. The successful service process started at 20:35:39.798067 UTC.

Before opening any new owner SSH connection, the Mac verified the certificate
and hostname, appended and read sequence 10 over HTTPS, and completed OAuth and
a direct MCP tool call. A Fusion capture at 20:35:50 UTC showed the lock screen.
The test used the private VMware network, with no Tailscale dependency.

Cleanup removed the exact temporary startup task, scoped firewall rule and test
files. The adapter retained its original enabled DHCP configuration. The primary
service, pools, keys, certificates, source and evidence remained intact.

## Mac reboot and persistent operation

The follow-up installed a Mac launchd service for a dedicated shared-pool
directory. Its startup definition enables `RunAtLoad` and `KeepAlive`. The
installed executable matches the tested release build; installer fix `127668f`
passed the full release gate and reached main.

The owner restarted the Mac on October 5 at 14:17:24 Pacific time and logged in
normally. The Mac service returned automatically with PID 1530. VMware Fusion
opened through macOS Login Items and restored the Windows VM. Windows resumed
its existing session with the same running service PID 3488; this host restart
checks VM recovery, while the earlier Windows reboot checks fresh guest startup.
Neither server needed a manual start.

After the host reboot, Windows verified the Mac certificate and hostname,
appended and read sequence 2 in the shared test pool over HTTPS, and completed
OAuth and a direct MCP tool call. The Mac's saved native client connection
verified Windows access, then appended and fetched sequence 13. Before the host
reboot, both directions also passed HTTPS and direct MCP checks.

Both servers remain running on the private VMware network. The Mac server
shares only the dedicated test directory; existing agent pools remain private.
The retained Windows firewall rule permits only the Mac's VMware address to
reach the selected HTTPS listener. No private TLS key crossed machines.

FileVault remains enabled. The owner must enter the normal Mac login password
after a host restart. Fusion and the VM start after Mac login. This check does
not establish access before Mac login or disk unlock.

Windows could reach the Mac but could not save its native client connection
through the SSH session: Windows credential encryption returned access denied
(error 5). HTTPS and direct MCP checks passed with the private invitation.
An interactive Windows client connection still needs a separate check.

## Review and gates

Sol-high reviewed correctness, service ownership, permissions, rollback and
simplicity. The implementation addressed the material findings. The Mac full
`just release-gate` passed, including formatting, Clippy, `just check`, ABI and
package checks. Final Windows all-target release Clippy, service binary tests,
private permission tests and the release build passed.

These results do not establish native Windows ARM64 artifact support, older
Windows compatibility, Tailscale unattended routing, physical Linux/Pi boot,
or macOS startup before disk unlock. The report excludes credentials and private
TLS keys. Abrupt installer or host termination during an update can leave retained
backups and a disabled service for administrator repair.
