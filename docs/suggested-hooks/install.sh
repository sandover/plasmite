#!/usr/bin/env bash
#
# Purpose: Install/uninstall delegating git hooks that exec the tracked hooks in `docs/suggested-hooks/`.
# Exports: N/A (CLI script).
# Role: Contributor convenience helper; keeps `.git/hooks/*` tiny while versioning real hooks in-tree.
# Invariants: Never edits tracked hook scripts; only writes to `.git/hooks/`.
# Invariants: Refuses to overwrite existing hooks unless `--force` is provided.
set -euo pipefail

usage() {
  cat <<'TXT'
Usage:
  ./docs/suggested-hooks/install.sh [--force] [--uninstall]

Installs delegating hooks into .git/hooks/ for:
  - prepare-commit-msg
  - pre-commit
  - pre-push

Options:
  --force      Overwrite existing hooks (backs up to <hook>.bak.<timestamp>).
  --uninstall  Remove delegating hooks if they point at docs/suggested-hooks/.
TXT
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "${repo_root}" ]]; then
  echo "error: not in a git repo" >&2
  exit 1
fi

force=0
uninstall=0
while [[ $# -gt 0 ]]; do
  case "${1}" in
    --force) force=1; shift ;;
    --uninstall) uninstall=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "error: unknown arg: ${1}" >&2
      usage >&2
      exit 2
      ;;
  esac
done

hooks_dir="${repo_root}/.git/hooks"
tracked_dir="${repo_root}/docs/suggested-hooks"
hooks=(prepare-commit-msg pre-commit pre-push)

backup_existing() {
  local hook_path="${1}"
  if [[ ! -e "${hook_path}" ]]; then
    return 0
  fi
  local ts
  ts="$(date +%Y%m%d-%H%M%S)"
  mv -- "${hook_path}" "${hook_path}.bak.${ts}"
}

write_delegator() {
  local hook="${1}"
  local dst="${hooks_dir}/${hook}"
  local src="${tracked_dir}/${hook}"

  if [[ ! -f "${src}" ]]; then
    echo "error: missing tracked hook: ${src}" >&2
    exit 1
  fi

  if [[ -e "${dst}" && "${force}" -ne 1 ]]; then
    echo "error: hook exists: ${dst}" >&2
    echo "hint: re-run with --force to overwrite (it will be backed up)." >&2
    exit 1
  fi

  if [[ -e "${dst}" && "${force}" -eq 1 ]]; then
    backup_existing "${dst}"
  fi

  cat >"${dst}" <<TXT
#!/usr/bin/env bash
exec "${src}" "\$@"
TXT
  chmod +x "${dst}"
}

remove_delegator_if_ours() {
  local hook="${1}"
  local dst="${hooks_dir}/${hook}"
  local src="${tracked_dir}/${hook}"

  if [[ ! -f "${dst}" ]]; then
    return 0
  fi

  if grep -Fq "exec \"${src}\"" "${dst}" 2>/dev/null; then
    rm -f -- "${dst}"
    return 0
  fi

  echo "note: leaving existing hook in place (not managed by this installer): ${dst}" >&2
}

main() {
  if [[ "${uninstall}" -eq 1 ]]; then
    local h
    for h in "${hooks[@]}"; do
      remove_delegator_if_ours "${h}"
    done
    echo "ok: uninstall complete" >&2
    exit 0
  fi

  mkdir -p "${hooks_dir}"
  local h
  for h in "${hooks[@]}"; do
    write_delegator "${h}"
  done
  echo "ok: installed hooks into ${hooks_dir}" >&2
}

main
