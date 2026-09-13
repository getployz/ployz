#!/usr/bin/env bash

set -euo pipefail

release_artifacts_needed() {
    local event=$1
    shift
    if [ "$event" != pull_request ]; then
        echo true
        return
    fi
    local file
    for file in "$@"; do
        case $file in
            core/.goreleaser.yaml | .github/workflows/release.yml | .github/workflows/release-contracts.yml | core/scripts/verify-release.sh | core/scripts/pack-release.sh | core/scripts/homebrew-formula.sh | core/scripts/release-artifacts-needed.sh)
                echo true
                return
                ;;
        esac
    done
    echo false
}

event=${1:-}
[ -n "$event" ] || {
    echo "usage: $0 <event> [file...]" >&2
    exit 1
}
shift
release_artifacts_needed "$event" "$@"
