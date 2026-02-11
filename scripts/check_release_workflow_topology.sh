#!/usr/bin/env bash
# Purpose: Validate release workflow topology with structured YAML parsing.
# Key outputs: Non-zero exit when final release dependencies are incomplete.
# Role: Enforce fail-closed publish sequencing invariants in CI and local checks.
# Invariants: release-publish release job must depend on all publish-* jobs.
# Invariants: Check parses YAML semantically (not line-oriented regex matching).
# Notes: Run from repo root; safe read-only validation script.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUBLISH_WORKFLOW="$ROOT/.github/workflows/release-publish.yml"

if [[ ! -f "$PUBLISH_WORKFLOW" ]]; then
  echo "error: missing workflow file: $PUBLISH_WORKFLOW" >&2
  exit 1
fi

ruby - "$PUBLISH_WORKFLOW" <<'RUBY'
require "yaml"

workflow_path = ARGV.fetch(0)
workflow_raw = File.read(workflow_path)
workflow = YAML.safe_load(workflow_raw, aliases: true) || {}
jobs = workflow.fetch("jobs") do
  warn "error: #{workflow_path} missing top-level jobs map"
  exit 1
end

release = jobs.fetch("release") do
  warn "error: #{workflow_path} missing release job"
  exit 1
end

release_needs = Array(release["needs"]).map(&:to_s)
publish_jobs = jobs.keys.select { |name| name.start_with?("publish-") }.sort
missing = publish_jobs.reject { |name| release_needs.include?(name) }

if publish_jobs.empty?
  warn "error: #{workflow_path} has no publish-* jobs to validate"
  exit 1
end

unless missing.empty?
  warn "error: release job is missing publish dependencies: #{missing.join(", ")}"
  warn "hint: release.needs currently: #{release_needs.sort.join(", ")}"
  exit 1
end

puts "ok: release job depends on all publish jobs (#{publish_jobs.join(", ")})"
RUBY
