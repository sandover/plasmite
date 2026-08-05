# Windows VM control experiment: 3PMJEU

Date: 2026-08-05

Goal: Verify the existing Mac-host-to-Windows Plasmite MCP bridge while evaluating the `windows-vm-control` skill. This log contains no credentials.

| Step | Time | Difficulty | Outcome and hiccups |
| --- | ---: | --- | --- |
| Read the VM-control and Ergo workflows; inspect task `3PMJEU` | <1s tool time | Easy | The decision tables made direct SSH the clear default. The task supplied concrete services, security constraints, and acceptance evidence. |
| Probe configured `windows-vm` SSH alias | 8.01s | Easy to classify | Timed out at `192.168.175.128:22`. The skill's failure table points to stale address, listener, or firewall—not credentials. Next step is to refresh the guest address through VMware Guest Operations. |
| Refresh guest IP with VMware Tools | 0.32s | Easy | Guest moved from `.128` to `.129`; no guest change was required. |
| Probe refreshed IP | 0.14s | Easy to classify | Reached SSH, then correctly failed host-key verification. |
| Verify host identity | 0.15s | Moderate | `ssh-keyscan` showed the new endpoint's ED25519 key exactly matched the trusted `.128` key. This justified updating the alias without weakening host-key checking. |
| Update alias and verify SSH | 0.89s | Easy after identity check | Changed only the alias address, accepted the already-verified key, and reached `BBOOK-WIN` as the expected user. |
| Audit host services and listeners | 0.08s | Easy | Both launch agents were running on distinct loopback and VMware host-only listeners. |
| First host health probe | 0.02s | Easy to correct | Used `/health`; both listeners correctly returned 404. Source inspection identified `/healthz`. |
| Correct host health probe | 0.01s | Easy | Both `/healthz` endpoints returned `{"ok":true}`. |
| Probe Codex from no-profile SSH | 0.61s | Moderate | `codex` was not on the SSH PATH, matching the skill's warning that guest sessions can differ. |
| Repair host credential modes and restart services | 0.02s | Easy | Changed only the bridge directory to `0700` and token to `0600`; restarted the two named launch agents. |
| Verify repaired host state | 0.09s | Easy | Both services restarted with exit code 0, listeners were distinct, and health passed. |
| Copy and run sanitized guest inspection | 0.30s + 0.69s | Easy | Confirmed host health from Windows. The canonical token variable and MCP entry were absent. |
| Locate standalone Codex | 37.50s | Difficult/inefficient | A recursive search found releases `0.144.1`, `0.144.5`, and `0.145.0`; it was interrupted after useful results. No stable CLI shim was present. |
| Inspect Codex CLI and MCP syntax | 0.66s | Easy once located | Verified `codex-cli 0.145.0` and current MCP commands. |
| First multi-file transfer attempt | 0.52s | Avoidable hiccup | Tried to give two sources two remote names in one `scp`; the unsupported shape failed and created a task-owned destination directory. |
| Explicit script and token transfers | 0.27s + 0.23s | Easy | Separate transfers succeeded over the verified SSH channel. |
| First setup-script run | 0.65s | Moderate hiccup | Noninteractive `Remove-Item` refused to remove the mistaken directory, so execution stopped before MCP registration. No token was persisted. |
| Diagnose transfer shape and correct cleanup | 0.92s plus inspection | Moderate | Inspected names/metadata only, read the exact token child, and used `cmd /c rmdir /s /q` in `finally`. |
| Persist token and register canonical MCP entry | 0.88s | Easy after correction | Stored `PLASMITE_MCP_TOKEN` for the user, removed the transfer directory, and added `plasmite-host`. A sanitized list exposed an older duplicate entry. |
| Compare old/new variables safely | 0.54s | Easy | Both 64-character values existed and matched; no value was printed. |
| Authenticated MCP verification from Windows | 0.58s | Easy | `tools/list` returned 8 tools and authenticated `plasmite_pool_list` succeeded with one pool. |
| Remove redundant old entry and variable | 0.67s | Easy | Removed only `plasmite_bridge` and `PLASMITE_BRIDGE_TOKEN`; retained the proven canonical setup. |
| Remove task-owned guest scripts | 0.30s | Easy | Deleted four exact temporary scripts; local scratch scripts were also removed. |
| Final host and guest verification | 0.09s + 0.54s | Easy | Permissions, services, listeners, health, persistent token metadata, canonical MCP list, alias, and zero temporary artifacts all passed. |

## Running observations

- The separation between SSH, Guest Operations, and visible desktop evidence prevented treating “Windows is visibly running” as proof that SSH was usable.
- The host-key warning added a useful safety step. The skill names the condition but could make the same-VM address-change verification recipe more explicit.
- The multiline-script guidance was much easier and safer than nested shell quoting for token handling and JSON-RPC verification.
- Separating host-owned support from guest execution kept the credential boundary understandable and prevented token values from entering output or command history.
- No skill files were modified.

## Final reflection

The skill made the overall control model idiomatic: try SSH first, classify the failed layer, use Guest Operations only to repair access, use copied scripts for multiline PowerShell, match evidence to claims, and delete only task-owned artifacts. Its guardrails were especially useful around host identity, credentials, and preserving the persistent workstation.

It was not maximally easy in four places:

1. Add a concise DHCP-address-change recipe: refresh with `getGuestIPAddress`, compare the new public host key with the trusted old entry, then update the alias and accept the verified key.
2. Add a Codex standalone-location recipe for Windows. The desktop installation can live under `.codex\packages\standalone\releases` with no stable executable on an SSH or PowerShell PATH.
3. Clarify Windows `scp` destination behavior and recommend one source/destination pair per command when exact remote names matter.
4. For noninteractive cleanup, recommend `cmd.exe /c del /f /q` and `rmdir /s /q` for exact task-owned paths when Windows PowerShell's `Remove-Item` attempts to prompt.

These are workflow refinements, not flaws in the skill's safety or executor model. The task completed without desktop input, UAC, guest reset, credential disclosure, or unrelated guest changes.
