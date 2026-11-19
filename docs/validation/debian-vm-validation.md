# Debian service and connection-failure checks

The November 16, 2026 checks passed for source revision `69cafd7` in an ARM64 Debian 13.7 VMware Fusion guest. A synthetic collector and Plasmite's native server service started after reboot, before a new SSH or desktop login. The Mac used its saved access key to read both fresh and retained data.

Use the [cookbook](../cookbook.md#run-a-collector-at-boot) for operating guidance. The [readings and failure observations](debian-vm-validation/) retain the raw results and a runnable [collector example](debian-vm-validation/collector.py). Full machine logs and setup scripts stay outside Git.

## Build and environment

- Source archive: `git archive 69cafd7`; no Git metadata in the guest checkout.
- Guest: aarch64, Debian 13.7, kernel `6.12.115+deb13.1-arm64`, glibc 2.41, four virtual CPUs and 8 GiB of memory.
- Compiler: Rust 1.99.1; `CARGO_BUILD_JOBS=3 cargo build --release --locked --bin plasmite`.
- Guest executable: `1.3.0-dev+unknown`, SHA-256 `a5a0c12f593721534402b655a2fc042379d8ca8bfb956da068b47356e683d2dc`.
- Both services ran as `brandon`, with private disposable pool/access state. Administrator authentication happened in an interactive terminal.

The exercise used a source build at `69cafd7` and rebooted the guest with Fusion's soft reset. A published ARM archive, an older glibc baseline, physical sensors, Raspberry Pi hardware, Wi-Fi hardware and power-loss recovery need their own checks.

## Startup, crash and reboot

The ordinary `serve install` command registered the server with systemd. A separate collector unit used `Restart=on-failure`, `RestartSec=1`, `User=brandon` and `UMask=0077`. Its Python program sent one identifiable reading every quarter second through a persistent `feed --durability flush` child and checked each receipt. Samples carried a boot ID, a collector-run ID and a sample counter.

1. The collector produced matching receipts and the Mac read its stored data through authenticated HTTPS.
2. Killing the collector with SIGKILL caused systemd to restart it. The process and run ID changed, and the restart count increased.
3. After a soft reboot, the Mac read a sample with a new boot ID before the test opened any SSH connection. The guest still showed its desktop login screen. Both units started about 3.36 seconds after boot.
4. The same saved client connection worked. Fetching acknowledged sequence 1020 returned the exact pre-reboot message.

The boot ID changed from `7c2006e7-19ac-432a-a8f2-f74356aca369` to `5528158e-4a32-4620-ae0c-7c834d81cced`. A normal reboot and a killed collector exercise different failure cases; neither establishes power-loss durability.

## Connection and output failures

A Mac-local relay forwarded encrypted TCP bytes to the guest's HTTPS listener. The client kept certificate-name and public-key checks enabled. The relay discarded the next response after a successful warmup write, while the test inspected the pool independently.

| Check | Observed result |
| --- | --- |
| Connection refused before sending | `feed` exited 8; the reading never appeared. |
| Connection lost after storage, before receipt | The reading appeared once; `feed` exited 8 with `kind: Io` and no receipt for it. |
| `--retry 3` during that I/O failure | No automatic replay; one stored copy before manual action. |
| Later queued input | The next reading never appeared. |
| Input backpressure | The pipe accepted 65,536 bytes, then reported that it could accept no more while the request waited. |
| Upstream after `feed` exited | The next write raised a broken-pipe error. |
| Manual resend of the uncertain reading | Two stored copies with distinct destination sequences. |
| Stdout failed after a local append | `/dev/full` caused exit 8. The first reading remained stored; the second never appeared. |

The stdout error explained that completed appends remain stored. The network errors used the generic hint about checking the path, filesystem and disk space. `src/api/remote.rs::request_result` creates the transport I/O error, and `src/cli/support.rs::add_io_hint` supplies that generic hint. A network-specific explanation would improve clarity. This exercise records that finding and keeps the existing retry and delivery behavior.

## Measurement time and bounded history

A reading carried an application measurement time 60 seconds in the past. The destination retained that value and assigned its own append time, about 60.002 seconds later. Applications choose their measurement fields; the pool supplies the destination sequence and time.

A separate 64 KiB pool accepted 1,000 readings whose payload included a 256-byte string. It retained sequences 927–1000: 74 consecutive records with matching values. Its header and inline index consumed 8 KiB, leaving a 57,344-byte ring. At four readings per second, 74 readings would cover about 18.5 seconds. The test wrote a burst; it measured retained count, not 18.5 seconds of elapsed collection. Message size and configuration affect the estimate.

## Recorded guidance and cleanup

Brandon authorized the VM checks and closure of applicable tickets. The resulting guidance uses ordinary systemd services, identifiable samples, destination reads, application measurement timestamps and measured retention. Connection failures end `feed`; callers inspect application IDs before retrying an uncertain write. The guidance adds no delivery queue, automatic I/O replay or duplicate removal.

The checks support consultation tickets `7KCG37`, `2WHZE2`, `KFVC5R`, `CKFILU`, `BGJFNZ`, `JS272O` and `Q63O3Y`. Physical Pi installation and sharing checks remain separate. `VIKZVO` still concerns the chosen Pi/OS installation experience; its earlier statement that ARM64 falls outside the target set is obsolete.

The cleanup removed both temporary startup jobs, the disposable guest checkout and state, and the Mac test credentials. The previous guest checkout remained clean at its original revision. The test left Debian running.
