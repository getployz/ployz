#!/usr/bin/env bash
set -euo pipefail

PLOYZ_REPO="${PLOYZ_REPO:-getployz/ployz}"

usage() {
  cat <<'EOF'
Usage:
  ployz.sh install [options]
  ployz.sh probe --json

Options:
  --mode MODE            docker, host-exec, or host-service
  --source SOURCE        release, git, or payload
  --version VERSION      Release version or "latest"
  --git-url URL          Git repository URL for --source git
  --git-ref REF          Git ref for --source git
  --payload-dir PATH     Payload directory for --source payload
  --no-daemon-install    Skip `ployz daemon install`
EOF
}

shell_quote() {
  printf "'%s'" "${1//\'/\'\"\'\"\'}"
}

json_escape() {
  local value=${1//\\/\\\\}
  value=${value//\"/\\\"}
  value=${value//$'\n'/\\n}
  value=${value//$'\r'/\\r}
  value=${value//$'\t'/\\t}
  printf '%s' "${value}"
}

current_os() {
  case "$(uname -s)" in
    Linux) printf 'linux' ;;
    Darwin) printf 'darwin' ;;
    *) printf 'other' ;;
  esac
}

current_arch() {
  case "$(uname -m)" in
    x86_64|amd64) printf 'x86_64' ;;
    aarch64|arm64) printf 'aarch64' ;;
    *) printf '%s' "$(uname -m)" ;;
  esac
}

default_mode() {
  case "$(current_os)" in
    darwin)
      printf 'docker'
      ;;
    linux)
      if command -v systemctl >/dev/null 2>&1 && { [[ ${EUID} -eq 0 ]] || sudo -n true >/dev/null 2>&1; }; then
        printf 'host-service'
      else
        printf 'host-exec'
      fi
      ;;
    *)
      printf 'host-exec'
      ;;
  esac
}

user_bin_dir() {
  printf '%s/.local/bin' "${HOME}"
}

default_data_dir() {
  case "$(current_os)" in
    linux)
      if [[ ${EUID} -eq 0 ]]; then
        printf '/var/lib/ployz'
      else
        printf '%s' "${XDG_DATA_HOME:-${HOME}/.local/share}/ployz"
      fi
      ;;
    darwin)
      printf '%s/Library/Application Support/ployz' "${HOME}"
      ;;
    *)
      printf '%s/.ployz' "${HOME}"
      ;;
  esac
}

default_config_path() {
  case "$(current_os)" in
    linux)
      printf '%s/.config/ployz/config.toml' "${HOME}"
      ;;
    darwin)
      printf '%s/Library/Application Support/ployz/config.toml' "${HOME}"
      ;;
    *)
      printf '%s/.config/ployz/config.toml' "${HOME}"
      ;;
  esac
}

default_socket_path() {
  case "$(current_os)" in
    linux)
      if [[ ${EUID} -eq 0 ]]; then
        printf '/run/ployz/ployzd.sock'
      else
        printf '%s/ployz/ployzd.sock' "${XDG_RUNTIME_DIR:-/tmp}"
      fi
      ;;
    darwin)
      printf '%s/ployz/ployzd.sock' "${TMPDIR:-/tmp}"
      ;;
    *)
      printf '/tmp/ployz/ployzd.sock'
      ;;
  esac
}

download_file() {
  local url=$1
  local dest=$2
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "${url}" -o "${dest}"
    return
  fi
  if command -v wget >/dev/null 2>&1; then
    wget -qO "${dest}" "${url}"
    return
  fi
  printf 'curl or wget is required to download %s\n' "${url}" >&2
  exit 1
}

manifest_path() {
  printf '%s/install/manifest.env' "$(default_data_dir)"
}

assets_dir() {
  printf '%s/install/assets' "$(default_data_dir)"
}

required_payload_file() {
  local base=$1
  local path=$2
  [[ -e "${base}/${path}" ]] || {
    printf 'payload missing %s\n' "${base}/${path}" >&2
    exit 1
  }
}

write_manifest() {
  local path=$1
  local source_kind=$2
  local requested_mode=$3
  local source_version=$4
  local source_git_url=$5
  local source_git_ref=$6
  local bin_dir=$7
  local assets_dir_path=$8
  local config_path=$9
  local data_dir=${10}
  local socket_path=${11}

  install -d "$(dirname "${path}")"
  cat > "${path}" <<EOF
SOURCE_KIND=$(shell_quote "${source_kind}")
SOURCE_VERSION=$(shell_quote "${source_version}")
SOURCE_GIT_URL=$(shell_quote "${source_git_url}")
SOURCE_GIT_REF=$(shell_quote "${source_git_ref}")
BIN_DIR=$(shell_quote "${bin_dir}")
ASSETS_DIR=$(shell_quote "${assets_dir_path}")
CONFIG_PATH=$(shell_quote "${config_path}")
DATA_DIR=$(shell_quote "${data_dir}")
SOCKET_PATH=$(shell_quote "${socket_path}")
INSTALLER_PATH=$(shell_quote "${bin_dir}/ployz.sh")
PLOYZ_PATH=$(shell_quote "${bin_dir}/ployz")
PLOYZD_PATH=$(shell_quote "${bin_dir}/ployzd")
PLOYZ_GATEWAY_PATH=$(shell_quote "${bin_dir}/ployz-gateway")
PLOYZ_DNS_PATH=$(shell_quote "${bin_dir}/ployz-dns")
CORROSION_PATH=$(shell_quote "${bin_dir}/corrosion")
REQUESTED_MODE=$(shell_quote "${requested_mode}")
CONFIGURED_MODE=$(shell_quote "")
SERVICE_BACKEND=$(shell_quote "")
EOF
}

install_payload() {
  local payload_dir=$1
  local source_kind=$2
  local requested_mode=$3
  local source_version=$4
  local source_git_url=$5
  local source_git_ref=$6
  local bin_dir manifest assets_path

  required_payload_file "${payload_dir}" "ployz.sh"
  required_payload_file "${payload_dir}" "bin/ployz"
  required_payload_file "${payload_dir}" "bin/ployzd"
  required_payload_file "${payload_dir}" "bin/ployz-gateway"
  required_payload_file "${payload_dir}" "bin/ployz-dns"
  required_payload_file "${payload_dir}" "bin/corrosion"
  required_payload_file "${payload_dir}" "assets/systemd/ployzd.service"

  bin_dir="$(user_bin_dir)"
  manifest="$(manifest_path)"
  assets_path="$(assets_dir)"

  install -d "${bin_dir}" "${assets_path}"
  install -m 0755 "${payload_dir}/ployz.sh" "${bin_dir}/ployz.sh"
  install -m 0755 "${payload_dir}/bin/ployz" "${bin_dir}/ployz"
  install -m 0755 "${payload_dir}/bin/ployzd" "${bin_dir}/ployzd"
  install -m 0755 "${payload_dir}/bin/ployz-gateway" "${bin_dir}/ployz-gateway"
  install -m 0755 "${payload_dir}/bin/ployz-dns" "${bin_dir}/ployz-dns"
  install -m 0755 "${payload_dir}/bin/corrosion" "${bin_dir}/corrosion"
  install -d "${assets_path}/systemd"
  install -m 0644 "${payload_dir}/assets/systemd/ployzd.service" "${assets_path}/systemd/ployzd.service"

  write_manifest \
    "${manifest}" \
    "${source_kind}" \
    "${requested_mode}" \
    "${source_version}" \
    "${source_git_url}" \
    "${source_git_ref}" \
    "${bin_dir}" \
    "${assets_path}" \
    "$(default_config_path)" \
    "$(default_data_dir)" \
    "$(default_socket_path)"
}

download_release_payload() {
  local version=$1
  local work_dir=$2
  local asset url
  asset="ployz-payload-$(current_os)-$(current_arch).tar.gz"
  if [[ "${version}" == "latest" ]]; then
    url="https://github.com/${PLOYZ_REPO}/releases/latest/download/${asset}"
  else
    url="https://github.com/${PLOYZ_REPO}/releases/download/${version}/${asset}"
  fi
  mkdir -p "${work_dir}/payload"
  download_file "${url}" "${work_dir}/payload.tgz"
  tar -xzf "${work_dir}/payload.tgz" -C "${work_dir}/payload"
  printf '%s' "${work_dir}/payload"
}

build_git_payload() {
  local git_url=$1
  local git_ref=$2
  local work_dir=$3

  git clone --depth 1 "${git_url}" "${work_dir}/repo" >/dev/null 2>&1
  if [[ -n "${git_ref}" ]]; then
    git -C "${work_dir}/repo" fetch --depth 1 origin "${git_ref}" >/dev/null 2>&1
    git -C "${work_dir}/repo" checkout --detach FETCH_HEAD >/dev/null 2>&1
  fi
  bash "${work_dir}/repo/scripts/build-install-payload.sh" --repo "${work_dir}/repo" --output "${work_dir}/payload"
  printf '%s' "${work_dir}/payload"
}

daemon_install() {
  local requested_mode=$1
  local manifest=$2
  local ployz_bin

  ployz_bin="$(user_bin_dir)/ployz"
  if [[ "${requested_mode}" == "host-service" && ${EUID} -ne 0 ]]; then
    sudo "${ployz_bin}" daemon install --mode host-service --install-manifest "${manifest}"
    return
  fi
  "${ployz_bin}" daemon install --mode "${requested_mode}" --install-manifest "${manifest}"
}

probe_json() {
  local manifest current_mode backend installed data_dir config_path socket_path bin_dir
  local os docker_available sudo_available systemd_available chosen_mode

  manifest="$(manifest_path)"
  installed=false
  current_mode=""
  backend=""
  bin_dir="$(user_bin_dir)"
  data_dir="$(default_data_dir)"
  config_path="$(default_config_path)"
  socket_path="$(default_socket_path)"
  os="$(current_os)"
  chosen_mode="$(default_mode)"
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    docker_available=true
  else
    docker_available=false
  fi
  if [[ ${EUID} -eq 0 ]] || { command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; }; then
    sudo_available=true
  else
    sudo_available=false
  fi
  if command -v systemctl >/dev/null 2>&1; then
    systemd_available=true
  else
    systemd_available=false
  fi

  if [[ -f "${manifest}" ]]; then
    installed=true
    # shellcheck disable=SC1090
    source "${manifest}"
    current_mode="${CONFIGURED_MODE:-}"
    backend="${SERVICE_BACKEND:-}"
    bin_dir="${BIN_DIR:-${bin_dir}}"
    data_dir="${DATA_DIR:-${data_dir}}"
    config_path="${CONFIG_PATH:-${config_path}}"
    socket_path="${SOCKET_PATH:-${socket_path}}"
  fi

  printf '{\n'
  printf '  "os": "%s",\n' "$(json_escape "${os}")"
  printf '  "has_docker": %s,\n' "${docker_available}"
  printf '  "has_sudo": %s,\n' "${sudo_available}"
  printf '  "has_systemd": %s,\n' "${systemd_available}"
  printf '  "default_mode": "%s",\n' "$(json_escape "${chosen_mode}")"
  printf '  "installed": %s,\n' "${installed}"
  printf '  "install_manifest": "%s",\n' "$(json_escape "${manifest}")"
  printf '  "bin_dir": "%s",\n' "$(json_escape "${bin_dir}")"
  printf '  "config_path": "%s",\n' "$(json_escape "${config_path}")"
  printf '  "data_dir": "%s",\n' "$(json_escape "${data_dir}")"
  printf '  "socket_path": "%s",\n' "$(json_escape "${socket_path}")"
  printf '  "configured_mode": "%s",\n' "$(json_escape "${current_mode}")"
  printf '  "service_backend": "%s"\n' "$(json_escape "${backend}")"
  printf '}\n'
}

main() {
  local command=${1:-}
  shift || true
  case "${command}" in
    install)
      local mode=""
      local source="release"
      local version="latest"
      local git_url="https://github.com/${PLOYZ_REPO}.git"
      local git_ref=""
      local payload_dir=""
      local no_daemon_install=0
      local work_dir resolved_mode resolved_payload manifest

      while [[ $# -gt 0 ]]; do
        case "$1" in
          --mode)
            mode=${2:-}
            shift 2
            ;;
          --source)
            source=${2:-}
            shift 2
            ;;
          --version)
            version=${2:-}
            shift 2
            ;;
          --git-url)
            git_url=${2:-}
            shift 2
            ;;
          --git-ref)
            git_ref=${2:-}
            shift 2
            ;;
          --payload-dir)
            payload_dir=${2:-}
            shift 2
            ;;
          --no-daemon-install)
            no_daemon_install=1
            shift
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

      resolved_mode=${mode:-$(default_mode)}
      case "${resolved_mode}" in
        docker|host-exec|host-service) ;;
        *)
          printf 'unsupported mode: %s\n' "${resolved_mode}" >&2
          exit 1
          ;;
      esac

      work_dir="$(mktemp -d)"
      trap 'rm -rf "${work_dir}"' EXIT

      case "${source}" in
        release)
          resolved_payload="$(download_release_payload "${version}" "${work_dir}")"
          ;;
        git)
          resolved_payload="$(build_git_payload "${git_url}" "${git_ref}" "${work_dir}")"
          ;;
        payload)
          [[ -n "${payload_dir}" ]] || { printf '--payload-dir is required for --source payload\n' >&2; exit 1; }
          resolved_payload="${payload_dir}"
          ;;
        *)
          printf 'unsupported source: %s\n' "${source}" >&2
          exit 1
          ;;
      esac

      install_payload "${resolved_payload}" "${source}" "${resolved_mode}" "${version}" "${git_url}" "${git_ref}"
      manifest="$(manifest_path)"
      if [[ ${no_daemon_install} -eq 0 ]]; then
        daemon_install "${resolved_mode}" "${manifest}"
      fi
      printf 'install complete\n'
      ;;
    probe)
      if [[ ${1:-} != "--json" ]]; then
        printf 'probe requires --json\n' >&2
        exit 1
      fi
      probe_json
      ;;
    ""|--help|-h)
      usage
      ;;
    *)
      printf 'unknown command: %s\n' "${command}" >&2
      exit 1
      ;;
  esac
}

main "$@"
