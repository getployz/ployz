#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${ENV_FILE:-$ROOT_DIR/.env.targets}"

if [[ -f "$ENV_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$ENV_FILE"
fi

SSH_PORT="${SSH_PORT:-22}"
TARGETS_RAW="${TARGETS:-}"

if [[ $# -gt 0 ]]; then
  TARGETS_RAW="$*"
fi

if [[ -z "$TARGETS_RAW" ]]; then
  echo "No targets set. Add TARGETS to $ENV_FILE or pass hosts as args." >&2
  exit 1
fi

read -r -a TARGET_LIST <<<"$TARGETS_RAW"

for target in "${TARGET_LIST[@]}"; do
  [[ -z "$target" ]] && continue
  echo "==> Bootstrapping $target"

  ssh -p "$SSH_PORT" "$target" 'bash -s' <<'EOF'
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "Remote host must be Linux" >&2
  exit 1
fi

if [[ ! -r /etc/os-release ]]; then
  echo "Cannot detect distro: /etc/os-release missing" >&2
  exit 1
fi

# shellcheck disable=SC1091
source /etc/os-release
OS_ID="${ID:-unknown}"

if [[ "$OS_ID" == "manjaro" || "$OS_ID" == "manjaro-arm" || "$OS_ID" == "endeavouros" || "$OS_ID" == "cachyos" ]]; then
  OS_ID="arch"
fi
if [[ "$OS_ID" == "pop" || "$OS_ID" == "linuxmint" || "$OS_ID" == "zorin" ]]; then
  OS_ID="ubuntu"
fi
if [[ "$OS_ID" == "fedora-asahi-remix" ]]; then
  OS_ID="fedora"
fi

if [[ "$(id -u)" -ne 0 ]]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "Remote user is not root and sudo is unavailable" >&2
    exit 1
  fi
  SUDO="sudo"
else
  SUDO=""
fi

start_docker() {
  if command -v systemctl >/dev/null 2>&1; then
    $SUDO systemctl enable --now docker
    $SUDO systemctl is-active --quiet docker
    return
  fi
  if command -v service >/dev/null 2>&1; then
    $SUDO service docker start
    return
  fi
  if command -v rc-service >/dev/null 2>&1; then
    $SUDO rc-update add docker default >/dev/null 2>&1 || true
    $SUDO rc-service docker start
    return
  fi
  echo "No known service manager found to start Docker" >&2
  exit 1
}

install_docker() {
  case "$OS_ID" in
    ubuntu|debian|raspbian)
      export DEBIAN_FRONTEND=noninteractive
      $SUDO rm -f /etc/apt/sources.list.d/docker.list /etc/apt/sources.list.d/docker.sources
      $SUDO apt-get update -y
      $SUDO apt-get install -y ca-certificates curl gnupg lsb-release
      $SUDO install -m 0755 -d /etc/apt/keyrings
      $SUDO rm -f /etc/apt/keyrings/docker.gpg /etc/apt/keyrings/docker.asc
      curl -fsSL "https://download.docker.com/linux/$OS_ID/gpg" | $SUDO gpg --dearmor -o /etc/apt/keyrings/docker.gpg.tmp
      $SUDO mv /etc/apt/keyrings/docker.gpg.tmp /etc/apt/keyrings/docker.gpg
      $SUDO chmod a+r /etc/apt/keyrings/docker.gpg
      CODENAME="${VERSION_CODENAME:-}"
      if [[ -z "$CODENAME" ]] && command -v lsb_release >/dev/null 2>&1; then
        CODENAME="$(lsb_release -cs)"
      fi
      if [[ -z "$CODENAME" ]]; then
        echo "Cannot determine distro codename for Docker repo" >&2
        exit 1
      fi
      echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/$OS_ID $CODENAME stable" | $SUDO tee /etc/apt/sources.list.d/docker.list >/dev/null
      if ! $SUDO apt-get update -y; then
        echo "Docker apt repo failed; falling back to get.docker.com installer"
        curl -fsSL https://get.docker.com | $SUDO sh
      else
        $SUDO apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
      fi
      ;;
    arch|archarm)
      $SUDO pacman -Sy --noconfirm docker docker-compose
      ;;
    alpine)
      $SUDO apk add --no-cache docker docker-cli-compose
      ;;
    fedora|rhel|centos|rocky|almalinux|ol|amzn)
      if command -v dnf >/dev/null 2>&1; then
        $SUDO dnf -y install dnf-plugins-core || true
        if [[ "$OS_ID" == "fedora" ]]; then
          $SUDO dnf config-manager --add-repo https://download.docker.com/linux/fedora/docker-ce.repo || true
        else
          $SUDO dnf config-manager --add-repo https://download.docker.com/linux/centos/docker-ce.repo || true
        fi
        $SUDO dnf -y install docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin || $SUDO dnf -y install docker
      elif command -v yum >/dev/null 2>&1; then
        $SUDO yum -y install yum-utils || true
        $SUDO yum-config-manager --add-repo https://download.docker.com/linux/centos/docker-ce.repo || true
        $SUDO yum -y install docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin || $SUDO yum -y install docker
      else
        echo "No supported package manager found for $OS_ID" >&2
        exit 1
      fi
      ;;
    opensuse-leap|opensuse-tumbleweed|sles)
      $SUDO zypper --non-interactive refresh
      $SUDO zypper --non-interactive install docker
      ;;
    *)
      echo "Unsupported Linux distro for automatic Docker install: $OS_ID" >&2
      exit 1
      ;;
  esac
}

install_network_tools() {
  case "$OS_ID" in
    ubuntu|debian|raspbian)
      export DEBIAN_FRONTEND=noninteractive
      if ! $SUDO apt-get update -y; then
        # Broken third-party repos (often docker.list key issues) can block apt update.
        $SUDO rm -f /etc/apt/sources.list.d/docker.list /etc/apt/sources.list.d/docker.sources
        $SUDO apt-get update -y
      fi
      $SUDO apt-get install -y iproute2 iptables wireguard-tools
      ;;
    arch|archarm)
      $SUDO pacman -Sy --noconfirm iproute2 iptables wireguard-tools
      ;;
    alpine)
      $SUDO apk add --no-cache iproute2 iptables wireguard-tools
      ;;
    fedora|rhel|centos|rocky|almalinux|ol|amzn)
      if command -v dnf >/dev/null 2>&1; then
        $SUDO dnf -y install iproute iptables wireguard-tools
      elif command -v yum >/dev/null 2>&1; then
        $SUDO yum -y install iproute iptables wireguard-tools
      else
        echo "No supported package manager found for $OS_ID" >&2
        exit 1
      fi
      ;;
    opensuse-leap|opensuse-tumbleweed|sles)
      $SUDO zypper --non-interactive refresh
      $SUDO zypper --non-interactive install iproute2 iptables wireguard-tools
      ;;
    *)
      echo "Unsupported Linux distro for network tool install: $OS_ID" >&2
      exit 1
      ;;
  esac
}

ensure_network_tools() {
  missing=0
  for c in ip iptables wg; do
    if ! command -v "$c" >/dev/null 2>&1; then
      missing=1
      break
    fi
  done
  if [[ "$missing" -eq 0 ]]; then
    return
  fi

  echo "Installing networking tools (ip, iptables, wg)..."
  install_network_tools

  for c in ip iptables wg; do
    if ! command -v "$c" >/dev/null 2>&1; then
      echo "Failed to install required networking tool: $c" >&2
      exit 1
    fi
  done
}

if command -v docker >/dev/null 2>&1; then
  echo "Docker already installed"
else
  install_docker
fi

start_docker
docker info >/dev/null
ensure_network_tools
echo "Docker ready"
EOF
done

echo "Bootstrap complete"
