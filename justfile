default:
    @just --list

build:
    cargo build

build-release:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" == "Linux" ]]; then
      ./scripts/install-ebpf-bytecode.sh
      cargo build --release -p ployzd --features ebpf-native --bins
      cargo build --release -p ployzctl -p ployz-gateway -p ployz-dns
      exit 0
    fi
    cargo build --release -p ployzd --bins -p ployzctl -p ployz-gateway -p ployz-dns

test:
    cargo test --workspace --exclude ployzd --exclude ployz-runtime-backends

format-v2:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -f mix.exs ]]; then
      echo "mix.exs not present yet; skipping v2 format"
      exit 0
    fi
    if [[ ! -f .formatter.exs ]]; then
      echo ".formatter.exs not present yet; skipping v2 format"
      exit 0
    fi
    mix format --check-formatted

test-v2:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -f mix.exs ]]; then
      echo "mix.exs not present yet; skipping v2 tests"
      exit 0
    fi
    mix deps.get
    mix test --exclude docker

test-v2-e2e:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -f mix.exs ]]; then
      echo "mix.exs not present yet; skipping v2 Docker e2e tests"
      exit 0
    fi
    cargo build -p ployz-substrate-helper
    mix deps.get
    mix test --only docker

test-all:
    cargo test
    just verify-deploy-types

deploy-types:
    bash ./scripts/generate-deploy-types.sh

verify-deploy-types:
    bash ./scripts/verify-deploy-types.sh

bootstrap-linux *args:
    ./scripts/bootstrap-linux.sh {{args}}

e2e *args:
    cargo run -p ployz-e2e -- {{args}}

built-in-images-dev output="target/ployz-dev/built-in-images.toml":
    bash ./scripts/build-dev-built-in-images.sh "{{output}}"

ployzd *args:
    bash ./scripts/run-ployzd-dev.sh {{args}}

install prefix="/usr/local":
    just build-release
    install -d "{{prefix}}/bin"
    install -m 0755 ployz.sh "{{prefix}}/bin/ployz.sh"
    install -m 0755 target/release/ployzctl "{{prefix}}/bin/ployzctl"
    install -m 0755 target/release/ployzd "{{prefix}}/bin/ployzd"
    install -m 0755 target/release/ployz-gateway "{{prefix}}/bin/ployz-gateway"
    install -m 0755 target/release/ployz-dns "{{prefix}}/bin/ployz-dns"
    just install-nats-server {{prefix}}

install-nats-server prefix="/usr/local" repo="nats-io/nats-server":
    #!/usr/bin/env bash
    set -euo pipefail

    download() {
      local url="$1"
      local dest="$2"
      if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "${dest}" "${url}"
        return
      fi
      if command -v wget >/dev/null 2>&1; then
        wget -qO "${dest}" "${url}"
        return
      fi
      echo "curl or wget is required to download nats-server" >&2
      exit 1
    }

    version_file=".nats-version"
    if [[ ! -f "${version_file}" ]]; then
      echo "missing ${version_file}" >&2
      exit 1
    fi
    version="$(tr -d '[:space:]' < "${version_file}")"
    if [[ -z "${version}" ]]; then
      echo "empty nats-server version in ${version_file}" >&2
      exit 1
    fi

    install_path="{{prefix}}/bin/nats-server"
    version_stamp="{{prefix}}/bin/.nats-server-release-version"
    if [[ -x "${install_path}" && -f "${version_stamp}" ]]; then
      installed_version="$(tr -d '[:space:]' < "${version_stamp}")"
      if [[ "${installed_version}" == "${version}" ]]; then
        echo "nats-server ${version} already installed at ${install_path}; skipping"
        exit 0
      fi
    fi

    os="$(uname -s)"
    arch="$(uname -m)"
    case "${os}" in
      Darwin) asset_os="darwin" ;;
      Linux) asset_os="linux" ;;
      *)
        echo "unsupported platform: ${os}/${arch}" >&2
        exit 1
        ;;
    esac
    case "${arch}" in
      x86_64|amd64) asset_arch="amd64" ;;
      aarch64|arm64) asset_arch="arm64" ;;
      *)
        echo "unsupported platform: ${os}/${arch}" >&2
        exit 1
        ;;
    esac

    asset="nats-server-${version}-${asset_os}-${asset_arch}.tar.gz"
    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "${tmp_dir}"' EXIT

    url="https://github.com/{{repo}}/releases/download/${version}/${asset}"
    download "${url}" "${tmp_dir}/${asset}"
    tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}"
    binary="$(find "${tmp_dir}" -type f -name nats-server -perm -111 | head -n 1)"
    if [[ -z "${binary}" ]]; then
      echo "archive ${asset} did not contain executable nats-server" >&2
      exit 1
    fi

    install -d "{{prefix}}/bin"
    install -m 0755 "${binary}" "${install_path}"
    printf '%s\n' "${version}" > "${version_stamp}"
    if [[ "${os}" == "Darwin" ]]; then
      xattr -d com.apple.quarantine "${install_path}" 2>/dev/null || true
    fi
    echo "installed nats-server ${install_path} (${version} for ${os}/${arch})"

install-ebpf repo="getployz/ployz":
    ./scripts/install-ebpf-bytecode.sh {{repo}}

deploy *targets:
    just install-ebpf
    ./scripts/deploy-linux-binary.sh {{targets}}
