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
- `build_sdk`, `upload_sdist`, and `upload_wheel` control existing release
  workflow behavior.
- `channels` records whether the target is `official`, `preview`, or absent
  (`null`) for each target-dependent delivery channel.

Keep this file small. It describes target identity and channel capability; it
does not encode release sequencing, credentials, or policy prose. Run
`scripts/validate_release_targets.sh` after changing it.
