<!--
Purpose: Describe the Plasmite conformance model and how runners execute manifests.
Exports: N/A (documentation for harness implementers).
Role: Canonical entry point for language binding conformance expectations.
Invariants: Manifests are versioned; runners must be deterministic and side-effect scoped.
Notes: This document is normative for conformance runner behavior.
-->

# Conformance Suite

This directory defines the **conformance model** for Plasmite bindings and the contract
between a manifest author and a conformance runner implementation.

## Goals

- Provide a **language-agnostic manifest** that describes expected behavior.
- Ensure **consistent semantics** across Rust/Go/Python/Node bindings.
- Keep execution **deterministic** (no flaky timing/ordering assumptions).

## Runner Contract (v0)

A conformance runner:

- Reads a JSON manifest file with `conformance_version: 0`.
- Executes steps in order, against a fresh working directory.
- Treats all pool paths as **relative to the manifest working directory** unless absolute.
- Fails fast on the first unmet expectation.
- Emits a machine-readable summary (format runner-specific for now).

## Manifest Files

- The canonical manifest format is described in `manifest-v0.md`.
- Sample manifests live alongside the spec (e.g., `sample-v0.json`).

## Scope

Initial coverage focuses on:

- `create_pool`
- `append`
- `get`
- `tail`

Additional operations will be added additively with new fields or step types.
