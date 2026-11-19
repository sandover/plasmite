# Release target manifest

`targets.json` is the machine-readable source of truth for the native targets
that Plasmite builds and packages. Human-authored support policy remains in
`docs/record/distribution.md`; automation validates that policy against this
manifest rather than generating the document.

The top-level `schema_version` identifies the manifest shape. `targets` contains
one object per Rust target:

- `rust_target` is the Rust compilation triple.
- `runner` is the GitHub Actions runner label.
- `sdk_platform` is the stable suffix used by Python artifacts and, when
  `build_sdk` is true, SDK archives.
- `node_platform` is the directory and artifact suffix used by npm.
- `build_sdk`, `upload_sdist`, and `upload_wheel` select the targets that upload
  SDK archives, Python source distributions and Python wheels, respectively.
- `channels` records whether the target is `official`, `preview`, or absent
  (`null`) for each target-dependent delivery channel.

Run `just check-release-targets` after changing the manifest. That recipe checks the
schema, the distribution guide and the workflow scripts that use the manifest.
For release sequencing and credentials, see the [release runbook](../skills/plasmite-release-manager/SKILL.md).
