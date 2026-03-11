#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/bootstrap-linux.sh --artifacts-dir PATH [options]
  scripts/bootstrap-linux.sh --artifacts-url URL [options]

Options:
  --artifacts-dir PATH     Install from a local artifact directory.
  --artifacts-url URL      Download a tar.gz artifact bundle from URL.
  --prefix PATH            Install prefix. Defaults to /usr/local.
  --data-dir PATH          Ployz data dir. Defaults to /var/lib/ployz.
  --mode MODE              Runtime mode. Only host-service is supported in v1.
  --skip-start             Install and enable the service but do not start it.
  --force-download         Re-download the artifact bundle even if cached.
  --help                   Show this help text.

Artifact directory contents:
  ployzd
  corrosion
  ployzd.service
  bootstrap-manifest.env   (optional)
  ployz                    (optional; wrapper will be created if absent)
EOF
}

PREFIX="/usr/local"
DATA_DIR="/var/lib/ployz"
MODE="host-service"
ARTIFACTS_DIR=""
ARTIFACTS_URL=""
SKIP_START=0
FORCE_DOWNLOAD=0
STATE_DIR=""
WORK_DIR=""

log() {
  printf '[bootstrap] %s\n' "$*"
}

fail() {
  printf '[bootstrap] ERROR: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  local cmd=$1
  command -v "$cmd" >/dev/null 2>&1 || fail "required command not found: $cmd"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --artifacts-dir)
      ARTIFACTS_DIR=${2:-}
      shift 2
      ;;
    --artifacts-url)
      ARTIFACTS_URL=${2:-}
      shift 2
      ;;
    --prefix)
      PREFIX=${2:-}
      shift 2
      ;;
    --data-dir)
      DATA_DIR=${2:-}
      shift 2
      ;;
    --mode)
      MODE=${2:-}
      shift 2
      ;;
    --skip-start)
      SKIP_START=1
      shift
      ;;
    --force-download)
      FORCE_DOWNLOAD=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

if [[ ${EUID} -ne 0 ]]; then
  fail "run as root (for example via sudo)"
fi

case "${MODE}" in
  host-service) ;;
  *)
    fail "unsupported mode '${MODE}' (expected host-service)"
    ;;
esac

if [[ -n "${ARTIFACTS_DIR}" && -n "${ARTIFACTS_URL}" ]]; then
  fail "use either --artifacts-dir or --artifacts-url, not both"
fi
if [[ -z "${ARTIFACTS_DIR}" && -z "${ARTIFACTS_URL}" ]]; then
  fail "one of --artifacts-dir or --artifacts-url is required"
fi

ARCH=$(uname -m)
case "${ARCH}" in
  x86_64|amd64) ;;
  *)
    fail "unsupported architecture '${ARCH}' (v1 supports x86_64 only)"
    ;;
esac

need_cmd uname
need_cmd install
need_cmd systemctl

if [[ ! -f /etc/os-release ]]; then
  fail "/etc/os-release is missing"
fi

# shellcheck disable=SC1091
source /etc/os-release
DISTRO_ID=${ID:-unknown}
DISTRO_LIKE=${ID_LIKE:-}

is_like() {
  local needle=$1
  [[ " ${DISTRO_ID} ${DISTRO_LIKE} " == *" ${needle} "* ]]
}

package_manager() {
  if command -v apt-get >/dev/null 2>&1; then
    printf 'apt'
    return
  fi
  if command -v dnf >/dev/null 2>&1; then
    printf 'dnf'
    return
  fi
  if command -v pacman >/dev/null 2>&1; then
    printf 'pacman'
    return
  fi
  fail "unsupported distro '${DISTRO_ID}' (no apt, dnf, or pacman found)"
}

install_core_packages() {
  local pm
  pm=$(package_manager)
  case "${pm}" in
    apt)
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -y
      apt-get install -y \
        ca-certificates \
        curl \
        jq \
        openssh-client \
        python3 \
        tar \
        gzip \
        iproute2 \
        wireguard-tools
      ;;
    dnf)
      dnf install -y \
        ca-certificates \
        curl \
        jq \
        openssh-clients \
        python3 \
        tar \
        gzip \
        iproute \
        wireguard-tools
      ;;
    pacman)
      pacman -Sy --noconfirm \
        ca-certificates \
        curl \
        jq \
        openssh \
        python \
        tar \
        gzip \
        iproute2 \
        wireguard-tools
      ;;
  esac
}

install_docker() {
  if command -v docker >/dev/null 2>&1; then
    return
  fi

  if is_like arch; then
    pacman -Sy --noconfirm docker
    return
  fi

  log "installing docker via official convenience script"
  curl -fsSL https://get.docker.com | sh
}

enable_docker() {
  systemctl enable docker.service >/dev/null 2>&1 || true
  systemctl restart docker.service
}

prepare_state_dirs() {
  STATE_DIR="${DATA_DIR}/bootstrap"
  WORK_DIR="${STATE_DIR}/work"
  install -d "${STATE_DIR}" "${WORK_DIR}" "${PREFIX}/bin"
}

stage_from_local_dir() {
  local source_dir=$1
  [[ -d "${source_dir}" ]] || fail "artifact directory not found: ${source_dir}"
  local staged="${WORK_DIR}/bundle"
  rm -rf "${staged}"
  install -d "${staged}"
  cp -R "${source_dir}"/. "${staged}/"
  printf '%s' "${staged}"
}

stage_from_url() {
  local bundle_url=$1
  local archive_path="${WORK_DIR}/bundle.tar.gz"
  local staged="${WORK_DIR}/bundle"
  if [[ ${FORCE_DOWNLOAD} -eq 1 || ! -f "${archive_path}" ]]; then
    log "downloading artifact bundle from ${bundle_url}"
    curl -fsSL "${bundle_url}" -o "${archive_path}"
  fi
  rm -rf "${staged}"
  install -d "${staged}"
  tar -xzf "${archive_path}" -C "${staged}"
  printf '%s' "${staged}"
}

write_wrapper() {
  local wrapper_path=$1
  cat >"${wrapper_path}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

exec /usr/local/bin/ployzd "$@"
EOF
  chmod 0755 "${wrapper_path}"
}

install_artifacts() {
  local staged=$1
  local ployzd_src="${staged}/ployzd"
  local corrosion_src="${staged}/corrosion"
  local ployz_src="${staged}/ployz"
  local unit_src="${staged}/ployzd.service"
  local wrapper_tmp="${WORK_DIR}/ployz-wrapper"

  [[ -f "${ployzd_src}" ]] || fail "artifact missing: ${ployzd_src}"
  [[ -f "${corrosion_src}" ]] || fail "artifact missing: ${corrosion_src}"
  [[ -f "${unit_src}" ]] || fail "artifact missing: ${unit_src}"

  install -m 0755 "${ployzd_src}" "${PREFIX}/bin/ployzd"
  install -m 0755 "${corrosion_src}" "${PREFIX}/bin/corrosion"

  if [[ -f "${ployz_src}" ]]; then
    install -m 0755 "${ployz_src}" "${PREFIX}/bin/ployz"
  else
    write_wrapper "${wrapper_tmp}"
    install -m 0755 "${wrapper_tmp}" "${PREFIX}/bin/ployz"
  fi

  install -d /etc/systemd/system
  install -m 0644 "${unit_src}" /etc/systemd/system/ployzd.service
}

write_bootstrap_state() {
  local staged=$1
  local state_file="${STATE_DIR}/bootstrap-state.env"
  local manifest="${staged}/bootstrap-manifest.env"
  {
    printf 'BOOTSTRAP_COMPLETED_AT=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'BOOTSTRAP_SOURCE=%s\n' "${ARTIFACTS_URL:-${ARTIFACTS_DIR}}"
    printf 'BOOTSTRAP_MODE=%s\n' "${MODE}"
    printf 'BOOTSTRAP_DISTRO=%s\n' "${DISTRO_ID}"
    if [[ -f "${manifest}" ]]; then
      cat "${manifest}"
    fi
  } >"${state_file}"
}

verify_service() {
  systemctl daemon-reload
  systemctl enable ployzd.service >/dev/null
  if [[ ${SKIP_START} -eq 0 ]]; then
    systemctl restart ployzd.service
    systemctl is-enabled ployzd.service >/dev/null
    systemctl is-active ployzd.service >/dev/null
  fi
}

main() {
  log "installing prerequisites for ${DISTRO_ID}"
  install_core_packages
  install_docker
  enable_docker
  prepare_state_dirs

  local staged
  if [[ -n "${ARTIFACTS_DIR}" ]]; then
    staged=$(stage_from_local_dir "${ARTIFACTS_DIR}")
  else
    staged=$(stage_from_url "${ARTIFACTS_URL}")
  fi

  install_artifacts "${staged}"
  write_bootstrap_state "${staged}"
  verify_service

  log "bootstrap complete"
  if [[ ${SKIP_START} -eq 0 ]]; then
    log "service enabled: $(systemctl is-enabled ployzd.service)"
    log "service active: $(systemctl is-active ployzd.service)"
  fi
}

main "$@"
