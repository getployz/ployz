#!/usr/bin/env bash
set -euo pipefail

PLOYZ_REPO="${PLOYZ_REPO:-getployz/ployz}"

# Set PLOYZ_QUIET=1 to suppress progress output (useful for CI/e2e).
# Warnings and errors always print regardless.
PLOYZ_QUIET="${PLOYZ_QUIET:-0}"
PLOYZ_OUTPUT="human"
PLOYZ_JSON_FD=1
PLOYZ_JSON_CURRENT_STEP=""
PLOYZ_JSON_FINISHED=0

# --- Output helpers ---
# All progress goes to stderr so stdout stays clean (important for probe --json).

step() {
  if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
    json_log "${PLOYZ_JSON_CURRENT_STEP}" "$1"
  else
    [[ "${PLOYZ_QUIET}" == "1" ]] || printf '==> %s\n' "$1" >&2
  fi
}
info() {
  if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
    json_log "${PLOYZ_JSON_CURRENT_STEP}" "$1"
  else
    [[ "${PLOYZ_QUIET}" == "1" ]] || printf '    %s\n' "$1" >&2
  fi
}
warn() {
  if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
    json_warning "${PLOYZ_JSON_CURRENT_STEP}" "$1"
  else
    printf 'warning: %s\n' "$1" >&2
  fi
}
die()  {
  if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
    json_fail "${PLOYZ_JSON_CURRENT_STEP}" "$1"
  else
    printf 'error: %s\n' "$1" >&2
  fi
  exit 1
}

# --- Usage ---

usage() {
  cat <<'EOF'
Usage:
  ployz.sh install [options]
  ployz.sh probe --json

Options:
  --runtime TARGET       docker or host
  --service-mode MODE    user or system
  --source SOURCE        release, git, or payload
  --version VERSION      Release version or "latest"
  --git-url URL          Git repository URL for --source git
  --git-ref REF          Git ref for --source git
  --payload-dir PATH     Payload directory for --source payload
  --no-daemon-install    Skip `ployz daemon install`
  --json                 Emit newline-delimited JSON progress for provisioning UIs
EOF
}

# --- String escaping ---

# Wraps a value in single quotes for safe shell eval.
# Embedded single quotes become: '\'' (end quote, escaped quote, resume quote).
# This is the POSIX-standard trick for single-quote escaping.
# The resulting format is parsed by install.rs:parse_shell_value() in the daemon.
shell_quote() {
  printf "'%s'" "${1//\'/\'\"\'\"\'}"
}

# Escapes a string for embedding inside a JSON "double-quoted" value.
# Handles: backslashes, double quotes, newlines, carriage returns, tabs.
# No jq dependency required.
json_escape() {
  local value=${1//\\/\\\\}
  value=${value//\"/\\\"}
  value=${value//$'\n'/\\n}
  value=${value//$'\r'/\\r}
  value=${value//$'\t'/\\t}
  printf '%s' "${value}"
}

json_string() {
  printf '"%s"' "$(json_escape "$1")"
}

json_emit() {
  if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
    printf '%s\n' "$1" >&"${PLOYZ_JSON_FD}"
  fi
}

json_start() {
  local mode=$1
  local title="Setting up Ployz"
  local subtitle="This will install or refresh Ployz so it is ready to use."

  json_emit "$(printf '{"type":"start","mode":%s,"title":%s,"subtitle":%s,"steps":[{"id":"system","label":"Checking this computer","description":"Making sure Ployz can run here."},{"id":"settings","label":"Choosing the best setup","description":"Selecting the right runtime, service mode, and source."},{"id":"package","label":"Getting Ployz","description":"Downloading or preparing the install package."},{"id":"install","label":"Installing Ployz","description":"Putting the CLI and background service files in place."},{"id":"service","label":"Starting the background service","description":"Registering the daemon so Ployz can manage apps."},{"id":"ready","label":"Finishing setup","description":"Checking that everything is ready to use."}]}' "$(json_string "${mode}")" "$(json_string "${title}")" "$(json_string "${subtitle}")")"
}

json_step() {
  local id=$1
  local status=$2
  local label=$3
  local detail=${4:-}
  local data=${5:-}
  local frame

  frame="$(printf '{"type":"step","id":%s,"status":%s,"label":%s' "$(json_string "${id}")" "$(json_string "${status}")" "$(json_string "${label}")")"
  if [[ -n "${detail}" ]]; then
    frame="${frame},\"detail\":$(json_string "${detail}")"
  fi
  if [[ -n "${data}" ]]; then
    frame="${frame},\"data\":${data}"
  fi
  frame="${frame}}"
  json_emit "${frame}"
}

json_log() {
  local id=$1
  local message=$2
  local frame

  frame='{"type":"log"'
  if [[ -n "${id}" ]]; then
    frame="${frame},\"id\":$(json_string "${id}")"
  fi
  frame="${frame},\"message\":$(json_string "${message}")}"
  json_emit "${frame}"
}

json_warning() {
  local id=$1
  local message=$2
  local frame

  frame='{"type":"warning"'
  if [[ -n "${id}" ]]; then
    frame="${frame},\"id\":$(json_string "${id}")"
  fi
  frame="${frame},\"message\":$(json_string "${message}")}"
  json_emit "${frame}"
}

json_error() {
  local id=$1
  local message=$2
  local frame

  frame='{"type":"error"'
  if [[ -n "${id}" ]]; then
    frame="${frame},\"id\":$(json_string "${id}")"
  fi
  frame="${frame},\"message\":$(json_string "${message}")}"
  json_emit "${frame}"
}

json_finish() {
  local status=$1
  local title=$2
  local subtitle=${3:-}
  local data=${4:-}
  local frame

  PLOYZ_JSON_FINISHED=1
  frame="$(printf '{"type":"finish","status":%s,"title":%s' "$(json_string "${status}")" "$(json_string "${title}")")"
  if [[ -n "${subtitle}" ]]; then
    frame="${frame},\"subtitle\":$(json_string "${subtitle}")"
  fi
  if [[ -n "${data}" ]]; then
    frame="${frame},\"data\":${data}"
  fi
  frame="${frame}}"
  json_emit "${frame}"
}

json_fail() {
  local id=$1
  local message=$2

  if [[ "${PLOYZ_JSON_FINISHED}" == "1" ]]; then
    return
  fi
  if [[ -n "${id}" ]]; then
    json_step "${id}" "failed" "Setup could not continue"
  fi
  json_error "${id}" "${message}"
  json_finish "failed" "Setup could not finish" "${message}"
}

json_unexpected_error() {
  local exit_code=$?
  local command=${BASH_COMMAND:-}
  if [[ "${PLOYZ_OUTPUT}" == "json" && "${PLOYZ_JSON_FINISHED}" != "1" ]]; then
    json_fail "${PLOYZ_JSON_CURRENT_STEP}" "A setup command failed (${exit_code}): ${command}"
  fi
  exit "${exit_code}"
}

json_set_step() {
  PLOYZ_JSON_CURRENT_STEP=$1
}

run_logged() {
  local id=$1
  shift

  if [[ "${PLOYZ_OUTPUT}" != "json" ]]; then
    "$@"
    return
  fi

  set +e
  "$@" 2>&1 | while IFS= read -r line; do
    json_log "${id}" "${line}"
  done
  local status=${PIPESTATUS[0]}
  set -e
  return "${status}"
}

json_install_summary_data() {
  local source=$1
  local version=$2
  local runtime_target=$3
  local service_mode=$4
  local daemon_install=$5
  printf '{"installed":%s,"os":%s,"arch":%s,"runtime_target":%s,"service_mode":%s,"source":%s,"version":%s,"daemon_install":%s,"bin_dir":%s,"install_manifest":%s,"config_path":%s,"data_dir":%s,"socket_path":%s}' \
    "${installed:-false}" \
    "$(json_string "$(current_os)")" \
    "$(json_string "$(current_arch)")" \
    "$(json_string "${runtime_target}")" \
    "$(json_string "${service_mode}")" \
    "$(json_string "${source}")" \
    "$(json_string "${version}")" \
    "${daemon_install}" \
    "$(json_string "$(user_bin_dir)")" \
    "$(json_string "$(manifest_path)")" \
    "$(json_string "$(default_config_path)")" \
    "$(json_string "$(default_data_dir)")" \
    "$(json_string "$(default_socket_path)")"
}

# --- Platform detection ---

# Returns: linux, darwin, or other
current_os() {
  case "$(uname -s)" in
    Linux) printf 'linux' ;;
    Darwin) printf 'darwin' ;;
    *) printf 'other' ;;
  esac
}

# Returns: x86_64, aarch64, or raw uname -m output
current_arch() {
  case "$(uname -m)" in
    x86_64|amd64) printf 'x86_64' ;;
    aarch64|arm64) printf 'aarch64' ;;
    *) printf '%s' "$(uname -m)" ;;
  esac
}

release_os() {
  case "${1}" in
    Linux|linux) printf 'linux' ;;
    Darwin|darwin) printf 'darwin' ;;
    *) printf '%s' "${1}" ;;
  esac
}

release_arch() {
  case "${1}" in
    x86_64|amd64) printf 'x86_64' ;;
    aarch64|arm64) printf 'aarch64' ;;
    *) printf '%s' "${1}" ;;
  esac
}

payload_asset_name() {
  local os=$1
  local arch=$2
  printf 'ployz-payload-%s-%s.tar.gz' "${os}" "${arch}"
}

payload_asset_name_for_host() {
  payload_asset_name "$(current_os)" "$(current_arch)"
}

payload_asset_name_for_target_platform() {
  local target_platform=$1
  local os raw_arch

  os=${target_platform%%/*}
  raw_arch=${target_platform#*/}
  [[ -n "${os}" && -n "${raw_arch}" && "${os}" != "${target_platform}" ]] || {
    die "invalid target platform '${target_platform}'"
  }

  payload_asset_name "$(release_os "${os}")" "$(release_arch "${raw_arch}")"
}

# macOS defaults to docker (runs in Docker Desktop VM), Linux defaults to host
default_runtime() {
  case "$(current_os)" in
    darwin) printf 'docker' ;;
    *)      printf 'host' ;;
  esac
}

# system mode requires systemctl + root/sudo; otherwise user mode
default_service_mode() {
  case "$(current_os)" in
    darwin)
      printf 'user'
      ;;
    linux)
      if command -v systemctl >/dev/null 2>&1 && { [[ ${EUID} -eq 0 ]] || sudo -n true >/dev/null 2>&1; }; then
        printf 'system'
      else
        printf 'user'
      fi
      ;;
    *)
      printf 'user'
      ;;
  esac
}

# --- Path resolution ---
# These functions define where ployz files live on each platform.
# The paths follow XDG conventions on Linux and standard macOS locations.

# Where binaries are installed (ployz, ployzd, etc.)
user_bin_dir() {
  printf '%s/.local/bin' "${HOME}"
}

# Persistent data directory (state, databases, install metadata)
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

# TOML configuration file
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

# Unix domain socket for CLI <-> daemon communication
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

# --- Derived paths ---

manifest_path() {
  printf '%s/install/manifest.env' "$(default_data_dir)"
}

assets_dir() {
  printf '%s/install/assets' "$(default_data_dir)"
}

# --- Download helper ---

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
  die "curl or wget is required to download ${url}"
}

# --- Docker requirement ---

docker_ready() {
  command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1
}

docker_ready_as_root() {
  command -v docker >/dev/null 2>&1 && run_as_root docker info >/dev/null 2>&1
}

run_as_root() {
  if [[ ${EUID} -eq 0 ]]; then
    "$@"
    return
  fi

  command -v sudo >/dev/null 2>&1 || die "root or sudo is required to install Docker"
  sudo "$@"
}

start_docker_linux() {
  if command -v systemctl >/dev/null 2>&1; then
    run_as_root systemctl enable --now docker >/dev/null 2>&1 || true
    return
  fi

  if command -v service >/dev/null 2>&1; then
    run_as_root service docker start >/dev/null 2>&1 || true
  fi
}

install_docker_linux() {
  local install_script

  install_script="$(mktemp)"
  download_file "https://get.docker.com" "${install_script}"
  if ! run_as_root sh "${install_script}"; then
    rm -f "${install_script}"
    die "Docker installation failed"
  fi
  rm -f "${install_script}"
}

ensure_docker() {
  local os

  os="$(current_os)"
  if docker_ready || { [[ "${os}" == "linux" ]] && docker_ready_as_root; }; then
    return
  fi

  case "${os}" in
    linux)
      if command -v docker >/dev/null 2>&1; then
        step "Starting Docker"
        start_docker_linux
      else
        step "Installing Docker"
        install_docker_linux
        start_docker_linux
      fi
      ;;
    darwin)
      die "Docker is required. Install and start OrbStack, then re-run this installer: https://orbstack.dev"
      ;;
    *)
      die "Docker is required, but automatic Docker installation is only supported on Linux"
      ;;
  esac

  if [[ "${os}" == "linux" ]]; then
    docker_ready || docker_ready_as_root || die "Docker is installed, but the Docker daemon is not reachable"
    return
  fi

  docker_ready || die "Docker is installed, but the Docker daemon is not reachable"
}

# --- Payload validation ---

required_payload_file() {
  local base=$1
  local path=$2
  [[ -e "${base}/${path}" ]] || die "Payload is missing required file: ${base}/${path}"
}

# --- Install manifest ---

# Writes a shell-sourceable KEY='value' file that records where every component
# was installed and how. This file is read by:
#   - probe_json() in this script (via `source`)
#   - install.rs:InstallManifest::load_from_path() in the Rust daemon
#
# The format MUST remain KEY=<single-quoted-value>, one per line.
# See shell_quote() for the quoting scheme.
write_manifest() {
  local path=$1
  local source_kind=$2
  local runtime_target=$3
  local source_version=$4
  local source_git_url=$5
  local source_git_ref=$6
  local bin_dir=$7
  local assets_dir_path=$8
  local config_path=$9
  local data_dir=${10}
  local socket_path=${11}
  local service_mode=${12}
  local payload_asset=${13}
  local installed_at=${14}
  local updated_at=${15}

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
RUNTIME_TARGET=$(shell_quote "${runtime_target}")
SERVICE_MODE=$(shell_quote "${service_mode}")
SERVICE_BACKEND=$(shell_quote "")
INSTALLED_VERSION=$(shell_quote "${source_version}")
PAYLOAD_ASSET=$(shell_quote "${payload_asset}")
INSTALLED_AT=$(shell_quote "${installed_at}")
UPDATED_AT=$(shell_quote "${updated_at}")
EOF
}

# --- Payload installation ---

install_payload() {
  local payload_dir=$1
  local source_kind=$2
  local runtime_target=$3
  local source_version=$4
  local source_git_url=$5
  local source_git_ref=$6
  local service_mode=$7
  local payload_asset=$8
  local installed_at=$9
  local updated_at=${10}
  local bin_dir manifest assets_path

  step "Validating payload contents"
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

  step "Installing binaries to ${bin_dir}"
  install -d "${bin_dir}" "${assets_path}"
  info "ployz.sh      -> ${bin_dir}/ployz.sh"
  install -m 0755 "${payload_dir}/ployz.sh" "${bin_dir}/ployz.sh"
  info "ployz         -> ${bin_dir}/ployz"
  install -m 0755 "${payload_dir}/bin/ployz" "${bin_dir}/ployz"
  info "ployzd        -> ${bin_dir}/ployzd"
  install -m 0755 "${payload_dir}/bin/ployzd" "${bin_dir}/ployzd"
  info "ployz-gateway -> ${bin_dir}/ployz-gateway"
  install -m 0755 "${payload_dir}/bin/ployz-gateway" "${bin_dir}/ployz-gateway"
  info "ployz-dns     -> ${bin_dir}/ployz-dns"
  install -m 0755 "${payload_dir}/bin/ployz-dns" "${bin_dir}/ployz-dns"
  info "corrosion     -> ${bin_dir}/corrosion"
  install -m 0755 "${payload_dir}/bin/corrosion" "${bin_dir}/corrosion"

  step "Installing assets to ${assets_path}"
  install -d "${assets_path}/systemd"
  info "ployzd.service -> ${assets_path}/systemd/ployzd.service"
  install -m 0644 "${payload_dir}/assets/systemd/ployzd.service" "${assets_path}/systemd/ployzd.service"

  step "Writing install manifest to ${manifest}"
  write_manifest \
    "${manifest}" \
    "${source_kind}" \
    "${runtime_target}" \
    "${source_version}" \
    "${source_git_url}" \
    "${source_git_ref}" \
    "${bin_dir}" \
    "${assets_path}" \
    "$(default_config_path)" \
    "$(default_data_dir)" \
    "$(default_socket_path)" \
    "${service_mode}" \
    "${payload_asset}" \
    "${installed_at}" \
    "${updated_at}"
}

# --- Source acquisition ---

download_release_payload() {
  local version=$1
  local work_dir=$2
  local asset url
  asset="$(payload_asset_name_for_host)"
  if [[ "${version}" == "latest" ]]; then
    url="https://github.com/${PLOYZ_REPO}/releases/latest/download/${asset}"
  else
    url="https://github.com/${PLOYZ_REPO}/releases/download/${version}/${asset}"
  fi

  step "Downloading release payload (version: ${version})"
  info "${url}"
  mkdir -p "${work_dir}/payload"
  run_logged "package" download_file "${url}" "${work_dir}/payload.tgz"
  step "Extracting payload"
  run_logged "package" tar -xzf "${work_dir}/payload.tgz" -C "${work_dir}/payload"
  printf '%s' "${work_dir}/payload"
}

build_git_payload() {
  local git_url=$1
  local git_ref=$2
  local work_dir=$3

  step "Cloning repository for source build"
  info "URL: ${git_url}, ref: ${git_ref:-HEAD}"
  if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
    run_logged "package" git clone --depth 1 "${git_url}" "${work_dir}/repo"
  else
    git clone --depth 1 "${git_url}" "${work_dir}/repo" >/dev/null 2>&1
  fi
  if [[ -n "${git_ref}" ]]; then
    if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
      run_logged "package" git -C "${work_dir}/repo" fetch --depth 1 origin "${git_ref}"
      run_logged "package" git -C "${work_dir}/repo" checkout --detach FETCH_HEAD
    else
      git -C "${work_dir}/repo" fetch --depth 1 origin "${git_ref}" >/dev/null 2>&1
      git -C "${work_dir}/repo" checkout --detach FETCH_HEAD >/dev/null 2>&1
    fi
  fi

  step "Building payload from source (this may take several minutes)"
  run_logged "package" bash "${work_dir}/repo/scripts/build-install-payload.sh" --repo "${work_dir}/repo" --output "${work_dir}/payload"
  printf '%s' "${work_dir}/payload"
}

# --- Daemon service registration ---

daemon_install() {
  local runtime_target=$1
  local manifest=$2
  local service_mode=$3
  local ployz_bin

  ployz_bin="$(user_bin_dir)/ployz"

  step "Registering daemon service (runtime: ${runtime_target}, mode: ${service_mode})"
  if [[ "${runtime_target}" == "host" && "${service_mode}" == "system" && ${EUID} -ne 0 ]]; then
    warn "System-mode daemon install requires root privileges"
    info "Running: sudo ${ployz_bin} daemon install --runtime host --service-mode system --install-manifest ${manifest}"
    run_logged "service" sudo "${ployz_bin}" daemon install --runtime host --service-mode system --install-manifest "${manifest}"
    return
  fi

  info "Running: ${ployz_bin} daemon install --runtime ${runtime_target} --service-mode ${service_mode} --install-manifest ${manifest}"
  run_logged "service" "${ployz_bin}" daemon install \
    --runtime "${runtime_target}" \
    --service-mode "${service_mode}" \
    --install-manifest "${manifest}"
}

# --- Probe ---

probe_json() {
  local manifest current_runtime current_service_mode backend installed data_dir config_path socket_path bin_dir
  local source_kind source_version installed_version payload_asset installed_at updated_at
  local os docker_available sudo_available systemd_available chosen_runtime chosen_service_mode

  manifest="$(manifest_path)"
  installed=false
  current_runtime=""
  current_service_mode=""
  backend=""
  source_kind=""
  source_version=""
  installed_version=""
  payload_asset=""
  installed_at=""
  updated_at=""
  bin_dir="$(user_bin_dir)"
  data_dir="$(default_data_dir)"
  config_path="$(default_config_path)"
  socket_path="$(default_socket_path)"
  os="$(current_os)"
  chosen_runtime="$(default_runtime)"
  chosen_service_mode="$(default_service_mode)"
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
    # Load the install manifest to report installed state.
    # This `source`s a shell file — which means it executes code. The manifest
    # is written by this script's own write_manifest(), so it is trusted as long
    # as the data directory has not been tampered with.
    # shellcheck disable=SC1090
    source "${manifest}"
    current_runtime="${RUNTIME_TARGET:-}"
    current_service_mode="${SERVICE_MODE:-}"
    backend="${SERVICE_BACKEND:-}"
    source_kind="${SOURCE_KIND:-}"
    source_version="${SOURCE_VERSION:-}"
    installed_version="${INSTALLED_VERSION:-${source_version}}"
    payload_asset="${PAYLOAD_ASSET:-}"
    installed_at="${INSTALLED_AT:-}"
    updated_at="${UPDATED_AT:-}"
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
  printf '  "default_runtime": "%s",\n' "$(json_escape "${chosen_runtime}")"
  printf '  "default_service_mode": "%s",\n' "$(json_escape "${chosen_service_mode}")"
  printf '  "installed": %s,\n' "${installed}"
  printf '  "install_manifest": "%s",\n' "$(json_escape "${manifest}")"
  printf '  "bin_dir": "%s",\n' "$(json_escape "${bin_dir}")"
  printf '  "config_path": "%s",\n' "$(json_escape "${config_path}")"
  printf '  "data_dir": "%s",\n' "$(json_escape "${data_dir}")"
  printf '  "socket_path": "%s",\n' "$(json_escape "${socket_path}")"
  printf '  "runtime_target": "%s",\n' "$(json_escape "${current_runtime}")"
  printf '  "service_mode": "%s",\n' "$(json_escape "${current_service_mode}")"
  printf '  "service_backend": "%s",\n' "$(json_escape "${backend}")"
  printf '  "source_kind": "%s",\n' "$(json_escape "${source_kind}")"
  printf '  "source_version": "%s",\n' "$(json_escape "${source_version}")"
  printf '  "installed_version": "%s",\n' "$(json_escape "${installed_version}")"
  printf '  "payload_asset": "%s",\n' "$(json_escape "${payload_asset}")"
  printf '  "installed_at": "%s",\n' "$(json_escape "${installed_at}")"
  printf '  "updated_at": "%s"\n' "$(json_escape "${updated_at}")"
  printf '}\n'
}

internal_payload_asset_name() {
  local target_platform=""

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --target-platform)
        target_platform=${2:-}
        shift 2
        ;;
      --help|-h)
        die "usage: ployz.sh internal payload-asset-name [--target-platform OS/ARCH]"
        ;;
      *)
        die "unknown internal payload-asset-name argument: $1"
        ;;
    esac
  done

  if [[ -n "${target_platform}" ]]; then
    payload_asset_name_for_target_platform "${target_platform}"
    printf '\n'
    return
  fi

  payload_asset_name_for_host
  printf '\n'
}

internal_main() {
  local command=${1:-}
  shift || true

  case "${command}" in
    payload-asset-name)
      internal_payload_asset_name "$@"
      ;;
    *)
      die "Unknown internal command: ${command}"
      ;;
  esac
}

# --- Main ---

main() {
  local command=${1:-}
  shift || true
  case "${command}" in
    internal)
      internal_main "$@"
      ;;
    install)
      local runtime=""
      local service_mode=""
      local source="release"
      local version="latest"
      local git_url="https://github.com/${PLOYZ_REPO}.git"
      local git_ref=""
      local payload_dir=""
      local no_daemon_install=0
      local json=0
      local work_dir resolved_runtime resolved_service_mode resolved_payload manifest
      local installed=false install_mode="provision" existing_runtime="" existing_service_mode=""
      local existing_installed_at="" installed_at updated_at payload_asset daemon_install_json

      for arg in "$@"; do
        if [[ "${arg}" == "--json" ]]; then
          json=1
        fi
      done
      if [[ ${json} -eq 1 ]]; then
        PLOYZ_OUTPUT="json"
        PLOYZ_JSON_FD=3
        exec 3>&1
        trap 'json_unexpected_error' ERR
      fi

      while [[ $# -gt 0 ]]; do
        case "$1" in
          --json)
            shift
            ;;
          --runtime)
            runtime=${2:-}
            shift 2
            ;;
          --service-mode)
            service_mode=${2:-}
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
            die "Unknown argument: $1"
            ;;
        esac
      done

      manifest="$(manifest_path)"
      if [[ -f "${manifest}" ]]; then
        installed=true
        # shellcheck disable=SC1090
        source "${manifest}"
        existing_runtime="${RUNTIME_TARGET:-}"
        existing_service_mode="${SERVICE_MODE:-}"
        existing_installed_at="${INSTALLED_AT:-}"
      fi

      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        json_start "${install_mode}"
      fi

      json_set_step "system"
      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        json_step "system" "running" "Checking this computer"
      fi

      resolved_runtime=${runtime:-${existing_runtime:-$(default_runtime)}}
      resolved_service_mode=${service_mode:-${existing_service_mode:-$(default_service_mode)}}

      # Validate runtime
      case "${resolved_runtime}" in
        docker|host) ;;
        *) die "Unsupported runtime: ${resolved_runtime}" ;;
      esac

      # Validate service mode
      case "${resolved_service_mode}" in
        user|system) ;;
        *) die "Unsupported service mode: ${resolved_service_mode}" ;;
      esac

      # Docker runtime only works with user-mode services
      if [[ "${resolved_runtime}" == "docker" && "${resolved_service_mode}" != "user" ]]; then
        die "Docker runtime only supports --service-mode user"
      fi

      # System-mode services require systemd, which is Linux-only
      if [[ "${resolved_service_mode}" == "system" && "$(current_os)" != "linux" ]]; then
        die "--service-mode system is only supported on Linux"
      fi

      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        if [[ "${installed}" == "true" ]]; then
          json_step "system" "done" "Found Ployz on this computer" "Setup will refresh the existing installation." "$(printf '{"installed":true,"os":%s,"arch":%s}' "$(json_string "$(current_os)")" "$(json_string "$(current_arch)")")"
        else
          json_step "system" "done" "This computer is supported" "$(printf '%s %s detected' "$(current_os)" "$(current_arch)")" "$(printf '{"installed":false,"os":%s,"arch":%s}' "$(json_string "$(current_os)")" "$(json_string "$(current_arch)")")"
        fi
      fi

      json_set_step "settings"
      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        json_step "settings" "running" "Choosing the best setup"
      fi

      payload_asset="$(payload_asset_name_for_host)"
      daemon_install_json=true
      if [[ ${no_daemon_install} -eq 1 ]]; then
        daemon_install_json=false
      fi

      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        if [[ "${resolved_runtime}" == "docker" && "$(current_os)" == "darwin" ]]; then
          json_step "settings" "done" "Using the recommended setup" "Docker runtime, user service" "$(json_install_summary_data "${source}" "${version}" "${resolved_runtime}" "${resolved_service_mode}" "${daemon_install_json}")"
        else
          json_step "settings" "done" "Install settings are ready" "Runtime: ${resolved_runtime}, service mode: ${resolved_service_mode}" "$(json_install_summary_data "${source}" "${version}" "${resolved_runtime}" "${resolved_service_mode}" "${daemon_install_json}")"
        fi
      fi

      step "Installing ployz"
      info "OS: $(current_os), Arch: $(current_arch)"
      info "Runtime: ${resolved_runtime}, Service mode: ${resolved_service_mode}"
      info "Source: ${source}$([ "${source}" = "release" ] && printf ", version: ${version}" || true)"

      ensure_docker

      work_dir="$(mktemp -d)"
      trap "rm -rf -- \"${work_dir}\"" EXIT
      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        trap 'json_unexpected_error' ERR
      fi

      json_set_step "package"
      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        case "${source}" in
          release)
            json_step "package" "running" "Getting Ployz" "Release: ${version}"
            ;;
          git)
            json_step "package" "running" "Building Ployz from source" "Ref: ${git_ref:-HEAD}"
            ;;
          payload)
            json_step "package" "running" "Preparing the provided package"
            ;;
        esac
      fi
      case "${source}" in
        release)
          resolved_payload="$(download_release_payload "${version}" "${work_dir}")"
          ;;
        git)
          resolved_payload="$(build_git_payload "${git_url}" "${git_ref}" "${work_dir}")"
          ;;
        payload)
          [[ -n "${payload_dir}" ]] || die "--payload-dir is required for --source payload"
          resolved_payload="${payload_dir}"
          ;;
        *)
          die "Unsupported source: ${source}"
          ;;
      esac
      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        json_step "package" "done" "Ployz package is ready" "" "$(printf '{"payload_dir":%s,"payload_asset":%s}' "$(json_string "${resolved_payload}")" "$(json_string "${payload_asset}")")"
      fi

      json_set_step "install"
      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        json_step "install" "running" "Putting Ployz in place"
      fi
      updated_at="$(date +%s)"
      installed_at="${existing_installed_at:-${updated_at}}"
      install_payload \
        "${resolved_payload}" \
        "${source}" \
        "${resolved_runtime}" \
        "${version}" \
        "${git_url}" \
        "${git_ref}" \
        "${resolved_service_mode}" \
        "${payload_asset}" \
        "${installed_at}" \
        "${updated_at}"
      installed=true
      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        json_step "install" "done" "Ployz files are ready" "" "$(json_install_summary_data "${source}" "${version}" "${resolved_runtime}" "${resolved_service_mode}" "${daemon_install_json}")"
      fi

      manifest="$(manifest_path)"
      json_set_step "service"
      if [[ ${no_daemon_install} -eq 0 ]]; then
        if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
          json_step "service" "running" "Starting the Ployz service"
        fi
        daemon_install "${resolved_runtime}" "${manifest}" "${resolved_service_mode}"
        if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
          json_step "service" "done" "Ployz service is ready"
        fi
      else
        if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
          json_step "service" "skipped" "Background service setup was skipped" "You can start it later from the CLI."
        fi
      fi

      json_set_step "ready"
      if [[ "${PLOYZ_OUTPUT}" == "json" ]]; then
        json_step "ready" "done" "Setup complete"
        json_finish "ok" "Ployz is ready" "You can now deploy and manage apps from this computer." "$(json_install_summary_data "${source}" "${version}" "${resolved_runtime}" "${resolved_service_mode}" "${daemon_install_json}")"
      fi

      if [[ "${PLOYZ_OUTPUT}" != "json" ]]; then
        step "Installation complete"
        info ""
        info "Binaries:  $(user_bin_dir)/"
        info "Assets:    $(assets_dir)/"
        info "Manifest:  $(manifest_path)"
        info "Config:    $(default_config_path)"
        info "Data:      $(default_data_dir)"
        info "Socket:    $(default_socket_path)"
        info ""
        info "Run 'ployz status' to check the daemon."
      fi
      ;;
    probe)
      if [[ ${1:-} != "--json" ]]; then
        die "probe requires --json"
      fi
      probe_json
      ;;
    ""|--help|-h)
      usage
      ;;
    *)
      die "Unknown command: ${command}"
      ;;
  esac
}

main "$@"
