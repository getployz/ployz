#!/usr/bin/env bash

set -euo pipefail

newest_run_id_named_except() {
    local name=$1 except=$2
    jq -r --arg name "$name" --arg except "$except" '
        ($except | split("\n") | map(select(length > 0) | tostring)) as $skip
        | [.[] | select(.displayTitle == $name) | select((.databaseId | tostring) as $id | ($skip | index($id) | not))]
        | .[0].databaseId // empty
    '
}

if [ "${PLOYZ_BOUNCE_RELEASE_TEST_ONLY:-false}" != true ]; then
    workflow=${1:-}
    wanted=${2:-}
    ref=${3:-}
    [ -n "$workflow" ] && [ -n "$wanted" ] && [ -n "$ref" ] || {
        echo "usage: $0 <workflow> <run-name> <ref> [-f key=value ...]" >&2
        exit 1
    }
    shift 3
    existing=$(gh run list --workflow="$workflow" --event=workflow_dispatch --limit 20 --json databaseId --jq '.[].databaseId')
    gh workflow run "$workflow" --ref "$ref" "$@"
    id=
    for _ in $(seq 1 30); do
        id=$(gh run list --workflow="$workflow" --event=workflow_dispatch --limit 20 --json databaseId,displayTitle | newest_run_id_named_except "$wanted" "$existing")
        if [ -n "$id" ]; then
            gh run watch "$id" --exit-status
            exit $?
        fi
        sleep 2
    done
    echo "timed out waiting for $wanted on $ref" >&2
    exit 1
fi
