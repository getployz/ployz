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
            .goreleaser.yaml | .github/workflows/release.yml | .github/workflows/release-contracts.yml | scripts/verify-release.sh | scripts/pack-release.sh | scripts/homebrew-formula.sh | scripts/release-artifacts-needed.sh)
                echo true
                return
                ;;
        esac
    done
    echo false
}

if [ "${PLOYZ_RELEASE_ARTIFACTS_TEST_ONLY:-false}" != true ]; then
    event=${1:-}
    [ -n "$event" ] || {
        echo "usage: $0 <event> [file...]" >&2
        exit 1
    }
    shift
    release_artifacts_needed "$event" "$@"
fi
