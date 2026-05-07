#!/usr/bin/env bash
set -euo pipefail

BASE_REF=""
HEAD_REF=""
ALL_ZERO_SHA='0000000000000000000000000000000000000000'

usage() {
  cat <<'EOF'
Usage:
  scripts/should-run-hostexec-e2e.sh --base-ref REF --head-ref REF
EOF
}

log_reason() {
  printf '%s\n' "$1" >&2
}

emit_true() {
  log_reason "$1"
  printf 'true\n'
  exit 0
}

emit_false() {
  log_reason "$1"
  printf 'false\n'
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base-ref)
      BASE_REF=${2:-}
      shift 2
      ;;
    --head-ref)
      HEAD_REF=${2:-}
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      exit 1
      ;;
  esac
done

[[ -n "${BASE_REF}" && -n "${HEAD_REF}" ]] || {
  usage >&2
  exit 1
}

if [[ "${BASE_REF}" == "${ALL_ZERO_SHA}" ]]; then
  emit_true "hostexec e2e enabled: push has no usable base sha"
fi

if ! git rev-parse --verify "${BASE_REF}^{commit}" >/dev/null 2>&1; then
  emit_true "hostexec e2e enabled: base ref '${BASE_REF}' is unavailable"
fi

if ! git rev-parse --verify "${HEAD_REF}^{commit}" >/dev/null 2>&1; then
  emit_true "hostexec e2e enabled: head ref '${HEAD_REF}' is unavailable"
fi

mapfile -t changed_files < <(git diff --name-only "${BASE_REF}" "${HEAD_REF}")

if [[ ${#changed_files[@]} -eq 0 ]]; then
  emit_false "hostexec e2e skipped: no changed files"
fi

for path in "${changed_files[@]}"; do
  case "${path}" in
    .github/workflows/hostexec-e2e.yml|Cargo.toml|Cargo.lock|.nats-version|ployz.sh|Dockerfile.e2e-node|Dockerfile.payload-builder|scripts/build-install-payload.sh|scripts/install-ebpf-bytecode.sh|packaging/e2e/*|packaging/systemd/*|ebpf/*)
      emit_true "hostexec e2e enabled: critical input changed (${path})"
      ;;
  esac
done

changed_payload="$(printf '%s\n' "${changed_files[@]}")"

python3_output="$(
  CHANGED_FILES="${changed_payload}" python3 <<'PY'
import json
import os
import subprocess
import sys
from pathlib import Path

ROOTS = {"ployz-e2e", "ployzd", "ployz-gateway", "ployz-dns"}
changed_files = [line for line in os.environ.get("CHANGED_FILES", "").splitlines() if line]

try:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        check=True,
        capture_output=True,
        text=True,
    )
    metadata = json.loads(result.stdout)
except Exception as exc:
    print(f"ERROR cargo metadata failed: {exc}")
    sys.exit(0)

workspace_members = set(metadata["workspace_members"])
packages_by_id = {
    package["id"]: package for package in metadata["packages"] if package["id"] in workspace_members
}
workspace_names = {package["name"] for package in packages_by_id.values()}
package_dirs = []
deps_by_name = {}

for package in packages_by_id.values():
    manifest_path = Path(package["manifest_path"])
    try:
        package_dir = manifest_path.parent.relative_to(Path.cwd())
    except ValueError:
        package_dir = manifest_path.parent
    package_dirs.append((str(package_dir).rstrip("/"), package["name"]))
    deps_by_name[package["name"]] = [
        dep["name"] for dep in package.get("dependencies", []) if dep["name"] in workspace_names
    ]

relevant = set()
stack = [name for name in ROOTS if name in deps_by_name]
while stack:
    name = stack.pop()
    if name in relevant:
        continue
    relevant.add(name)
    stack.extend(deps_by_name.get(name, []))

package_dirs.sort(key=lambda item: len(item[0]), reverse=True)

def changed_package(path: str):
    for package_dir, package_name in package_dirs:
        prefix = f"{package_dir}/"
        if path == package_dir or path.startswith(prefix):
            return package_name
    return None

for path in changed_files:
    if path.startswith("crates/"):
        package_name = changed_package(path)
        if package_name is None:
            print(f"RUN crate path not mapped to a package: {path}")
            sys.exit(0)
        if package_name in relevant:
            print(f"RUN relevant workspace crate changed: {package_name} ({path})")
            sys.exit(0)
        continue

    if path.startswith(("scripts/", "packaging/", "ebpf/")):
        print(f"RUN unclassified code-bearing path changed: {path}")
        sys.exit(0)

print("SKIP no relevant hostexec e2e inputs changed")
PY
)"

case "${python3_output}" in
  RUN\ *)
    emit_true "${python3_output#RUN }"
    ;;
  SKIP\ *)
    emit_false "${python3_output#SKIP }"
    ;;
  ERROR\ *)
    emit_true "${python3_output#ERROR }"
    ;;
  *)
    emit_true "hostexec e2e enabled: unexpected detector output"
    ;;
esac
