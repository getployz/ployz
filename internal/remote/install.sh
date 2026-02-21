#!/bin/sh
# Usage: PLOYZ_VERSION=<version> sh install.sh
#   Installs ployz binaries, Docker, and systemd services on a Linux host.
#   For dev builds (PLOYZ_VERSION=dev), binaries must be pre-deployed via: just deploy user@host
set -eu

PLOYZ_VERSION="${PLOYZ_VERSION:-__PLOYZ_VERSION__}"
GITHUB_REPO="getployz/ployz"

# --- helpers ---

info()  { echo "[ployz] $*"; }
fatal() { echo "[ployz] ERROR: $*" >&2; exit 1; }

has_cmd() { command -v "$1" >/dev/null 2>&1; }

download() {
    url=$1; dest=$2
    if has_cmd curl; then
        curl -fsSL -o "$dest" "$url"
    elif has_cmd wget; then
        wget -qO "$dest" "$url"
    else
        fatal "curl or wget is required"
    fi
}

checksum() {
    if has_cmd sha256sum; then
        sha256sum "$1" | awk '{print $1}'
    elif has_cmd shasum; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        fatal "sha256sum or shasum is required"
    fi
}

install_docker() {
    if has_cmd docker; then
        info "docker is already installed"
        return
    fi

    info "installing docker"

    if [ "$PLATFORM" != "linux" ]; then
        fatal "docker must be installed manually on macOS — https://orbstack.dev"
    fi

    if has_cmd curl; then
        curl -fsSL https://get.docker.com | sh
    elif has_cmd wget; then
        wget -qO- https://get.docker.com | sh
    else
        fatal "curl or wget is required to install docker"
    fi

    if ! has_cmd docker; then
        fatal "docker installation failed — install manually: https://docs.docker.com/engine/install/"
    fi

    info "docker installed"
}

start_docker() {
    if docker info >/dev/null 2>&1; then return; fi

    info "starting docker"

    if has_cmd systemctl; then
        systemctl enable --now docker >/dev/null 2>&1
    elif has_cmd service; then
        service docker start >/dev/null 2>&1
    else
        fatal "could not start docker — no service manager found"
    fi

    for _ in $(seq 1 30); do
        if docker info >/dev/null 2>&1; then return; fi
        sleep 1
    done
    fatal "docker daemon did not start within 30s"
}

enable_services() {
    systemctl enable --now ployzd.service ployz-runtime.service >/dev/null 2>&1 || true
}

wait_for_ployzd() {
    info "waiting for ployzd"
    for _ in $(seq 1 30); do
        if [ -S /var/run/ployzd.sock ]; then return; fi
        sleep 1
    done
    fatal "ployzd did not become ready at /var/run/ployzd.sock within 30s"
}

# --- root check ---

if [ "$(id -u)" -ne 0 ]; then
    fatal "must be run as root"
fi

# --- platform ---

OS=$(uname -s)
case "$OS" in
    Linux)  PLATFORM="linux" ;;
    Darwin) PLATFORM="darwin" ;;
    *) fatal "unsupported OS: $OS" ;;
esac

# --- architecture ---

UNAME_M=$(uname -m)
case "$UNAME_M" in
    x86_64|amd64)  ARCH="amd64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *) fatal "unsupported architecture: $UNAME_M" ;;
esac

# --- version ---

if echo "$PLOYZ_VERSION" | grep -q '^__.*__$' || [ -z "$PLOYZ_VERSION" ]; then
    fatal "PLOYZ_VERSION is not set and no default was baked in"
fi

# strip leading v if present
PLOYZ_VERSION=$(echo "$PLOYZ_VERSION" | sed 's/^v//')

# --- dev build: require pre-deployed binaries ---

if [ "$PLOYZ_VERSION" = "dev" ]; then
    MISSING=""
    for bin in ployz ployzd ployz-runtime; do
        if ! [ -x "/usr/local/bin/$bin" ]; then
            MISSING="$MISSING $bin"
        fi
    done
    if [ -n "$MISSING" ]; then
        fatal "dev build requires pre-deployed binaries — missing:${MISSING}
  deploy them first:  just deploy user@host"
    fi
    info "dev build: using pre-deployed binaries in /usr/local/bin"

    install_docker
    start_docker

    if [ "$PLATFORM" = "linux" ]; then
        enable_services
        wait_for_ployzd
        info "ployz dev installed and running"
    else
        info "ployz dev installed"
    fi
    exit 0
fi

info "installing ployz $PLOYZ_VERSION ($PLATFORM/$ARCH)"

# --- idempotency: skip if correct version already installed ---

if has_cmd ployzd; then
    INSTALLED=$(ployzd --version 2>/dev/null || true)
    if [ "$INSTALLED" = "$PLOYZ_VERSION" ] || [ "$INSTALLED" = "v$PLOYZ_VERSION" ]; then
        info "ployz $PLOYZ_VERSION is already installed"
        if [ "$PLATFORM" = "linux" ]; then
            enable_services
            wait_for_ployzd
        fi
        exit 0
    fi
fi

# --- download checksums ---

BASE_URL="https://github.com/${GITHUB_REPO}/releases/download/v${PLOYZ_VERSION}"
CHECKSUMS_PATH="/tmp/ployz_checksums.txt"

info "downloading checksums"
download "${BASE_URL}/ployz_${PLOYZ_VERSION}_checksums.txt" "$CHECKSUMS_PATH"

# --- verify downloaded file against checksums ---

verify_checksum() {
    file=$1; name=$2
    EXPECTED=$(grep "$name" "$CHECKSUMS_PATH" | awk '{print $1}')
    if [ -z "$EXPECTED" ]; then
        fatal "${name} not found in checksums.txt"
    fi
    ACTUAL=$(checksum "$file")
    if [ "$ACTUAL" != "$EXPECTED" ]; then
        fatal "checksum mismatch: expected ${EXPECTED}, got ${ACTUAL}"
    fi
    info "checksum verified"
}

# --- platform-specific install ---

if [ "$PLATFORM" = "linux" ]; then
    if has_cmd apt-get; then
        PKG_MGR="apt"
        PKG_EXT="deb"
    elif has_cmd dnf; then
        PKG_MGR="dnf"
        PKG_EXT="rpm"
    elif has_cmd yum; then
        PKG_MGR="yum"
        PKG_EXT="rpm"
    else
        fatal "no supported package manager found (need apt-get, dnf, or yum)"
    fi

    PKG_FILE="ployz_${PLOYZ_VERSION}_${ARCH}.${PKG_EXT}"
    PKG_PATH="/tmp/${PKG_FILE}"

    info "downloading ${PKG_FILE}"
    download "${BASE_URL}/${PKG_FILE}" "$PKG_PATH"
    verify_checksum "$PKG_PATH" "$PKG_FILE"

    info "updating package index"
    case "$PKG_MGR" in
        apt) apt-get update -qq ;;
        dnf) dnf makecache -q ;;
        yum) yum makecache -q ;;
    esac

    info "installing via ${PKG_MGR}"
    case "$PKG_MGR" in
        apt) apt-get install -y "$PKG_PATH" ;;
        dnf) dnf install -y "$PKG_PATH" ;;
        yum) yum install -y "$PKG_PATH" ;;
    esac

    rm -f "$PKG_PATH"
else
    ARCHIVE_FILE="ployz_${PLOYZ_VERSION}_${PLATFORM}_${ARCH}.tar.gz"
    ARCHIVE_PATH="/tmp/${ARCHIVE_FILE}"

    info "downloading ${ARCHIVE_FILE}"
    download "${BASE_URL}/${ARCHIVE_FILE}" "$ARCHIVE_PATH"
    verify_checksum "$ARCHIVE_PATH" "$ARCHIVE_FILE"

    EXTRACT_DIR="/tmp/ployz_extract_$$"
    mkdir -p "$EXTRACT_DIR"
    tar -xzf "$ARCHIVE_PATH" -C "$EXTRACT_DIR"

    for bin in ployz ployzd ployz-runtime; do
        found=$(find "$EXTRACT_DIR" -name "$bin" -type f | head -1)
        if [ -z "$found" ]; then
            fatal "binary $bin not found in archive"
        fi
        install -m 755 "$found" /usr/local/bin/"$bin"
    done

    rm -rf "$EXTRACT_DIR" "$ARCHIVE_PATH"
fi

rm -f "$CHECKSUMS_PATH"

# --- docker ---

install_docker
start_docker

# --- start services ---

if [ "$PLATFORM" = "linux" ]; then
    wait_for_ployzd
    info "ployz $PLOYZ_VERSION installed and running"
else
    info "ployz $PLOYZ_VERSION installed"
fi
