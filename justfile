default:
    @just --list

build:
    go build -o bin/ployz ./cmd/ployz
    go build -o bin/ployzd ./cmd/ployzd

install prefix="/usr/local":
    just build
    install -d "{{prefix}}/bin"
    install -m 0755 bin/ployz "{{prefix}}/bin/ployz"
    install -m 0755 bin/ployzd "{{prefix}}/bin/ployzd"
    just install-corrosion {{prefix}}

install-macos prefix="/usr/local":
    just install {{prefix}}

install-corrosion prefix="/usr/local" repo="getployz/corrosion":
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
      echo "curl or wget is required to download corrosion" >&2
      exit 1
    }

    checksum_file() {
      local file_path="$1"
      if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "${file_path}" | awk '{print $1}'
        return
      fi
      if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "${file_path}" | awk '{print $1}'
        return
      fi
      echo "sha256sum or shasum is required" >&2
      exit 1
    }

    version_file=".corrosion-version"
    if [[ ! -f "${version_file}" ]]; then
      echo "missing ${version_file}" >&2
      exit 1
    fi
    version="$(tr -d '[:space:]' < "${version_file}")"
    if [[ -z "${version}" ]]; then
      echo "empty corrosion version in ${version_file}" >&2
      exit 1
    fi

    install_path="{{prefix}}/bin/corrosion"
    version_stamp="{{prefix}}/bin/.corrosion-release-version"
    if [[ -x "${install_path}" && -f "${version_stamp}" ]]; then
      installed_version="$(tr -d '[:space:]' < "${version_stamp}")"
      if [[ "${installed_version}" == "${version}" ]]; then
        echo "corrosion ${version} already installed at ${install_path}; skipping"
        exit 0
      fi
    fi

    os="$(uname -s)"
    arch="$(uname -m)"
    case "${os}:${arch}" in
      Darwin:arm64)
        asset="corrosion-aarch64-apple-darwin.tar.gz"
        ;;
      Darwin:x86_64)
        asset="corrosion-x86_64-apple-darwin.tar.gz"
        ;;
      Linux:aarch64|Linux:arm64)
        asset="corrosion-aarch64-unknown-linux-gnu.tar.gz"
        ;;
      Linux:x86_64|Linux:amd64)
        asset="corrosion-x86_64-unknown-linux-gnu.tar.gz"
        ;;
      *)
        echo "unsupported platform: ${os}/${arch}" >&2
        exit 1
        ;;
    esac

    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "${tmp_dir}"' EXIT

    base_url="https://github.com/{{repo}}/releases/download/${version}"
    download "${base_url}/checksums.txt" "${tmp_dir}/checksums.txt"
    download "${base_url}/${asset}" "${tmp_dir}/${asset}"

    checksums_path="${tmp_dir}/checksums.txt"
    if [[ ! -f "${checksums_path}" ]]; then
      echo "checksums.txt was not found in release {{repo}}@${version}" >&2
      exit 1
    fi

    expected="$(awk -v name="${asset}" '$2 == name { print $1; exit }' "${checksums_path}")"
    if [[ -z "${expected}" ]]; then
      echo "missing checksum entry for ${asset}" >&2
      exit 1
    fi
    actual="$(checksum_file "${tmp_dir}/${asset}")"
    if [[ "${expected}" != "${actual}" ]]; then
      echo "checksum mismatch for ${asset}: expected ${expected}, got ${actual}" >&2
      exit 1
    fi

    tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}"
    if [[ ! -f "${tmp_dir}/corrosion" ]]; then
      echo "archive ${asset} did not contain corrosion binary at root" >&2
      exit 1
    fi

    install -d "{{prefix}}/bin"
    install -m 0755 "${tmp_dir}/corrosion" "${install_path}"
    printf '%s\n' "${version}" > "${version_stamp}"
    if [[ "${os}" == "Darwin" ]]; then
      xattr -d com.apple.quarantine "${install_path}" 2>/dev/null || true
    fi
    echo "installed corrosion ${install_path} (${version} for ${os}/${arch})"

run *args:
    go run ./cmd/ployz {{args}}

clean:
    rm -rf bin/

test:
    go test ./...

lint:
    go vet ./...
    golangci-lint run ./...

tidy:
    go mod tidy

proto:
    protoc --go_out=. --go_opt=paths=source_relative --go-grpc_out=. --go-grpc_opt=paths=source_relative \
        --proto_path=. internal/daemon/pb/daemon.proto

bootstrap *targets:
    ./scripts/bootstrap-remote.sh {{targets}}

deploy *targets:
    ./scripts/deploy-linux-binary.sh {{targets}}

deploy-linux *targets:
    ./scripts/deploy-linux-binary.sh {{targets}}
