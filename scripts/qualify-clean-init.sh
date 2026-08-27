#!/usr/bin/env bash

set -uo pipefail

if [ "$#" -lt 2 ]; then
    echo "usage: $0 VERSION USER@HOST..." >&2
    exit 2
fi

version=$1
shift
ployz_bin=${PLOYZ_BIN:-ployz}
ssh_bin=${SSH_BIN:-ssh}
run=${PLOYZ_QUALIFY_RUN:-$(date -u +%Y%m%d%H%M%S)-$$}
total=$#
failures=0
index=0

for destination in "$@"; do
    host=${destination#*@}
    if [ "$host" = "$destination" ] || [[ "$host" == *:* ]]; then
        echo "qualification destinations must use USER@HOST on SSH port 22: $destination" >&2
        exit 2
    fi
done

for destination in "$@"; do
    index=$((index + 1))
    echo "[$index/$total] initializing $destination"
    if env -u PLOYZ_AUTO_CONFIRM "$ployz_bin" machine init "$destination" \
        --context "qualify-$run-$index" \
        --name "qualify-$index" \
        --version "$version" \
        --storage none \
        --no-dns; then
        echo "[$index/$total] passed"
        continue
    fi

    failures=$((failures + 1))
    echo "[$index/$total] failed; collecting bind diagnostics" >&2
    "$ssh_bin" "$destination" \
        'systemctl status ployz.service --no-pager || true; journalctl -u ployz.service --no-pager -n 200 || true; docker inspect ployz-corrosion || true; docker logs --tail 200 ployz-corrosion || true; ss -H -lntup || true' \
        || true
done

if [ "$failures" -ne 0 ]; then
    echo "$failures/$total clean founder initializations failed" >&2
    exit 1
fi

echo "$total/$total clean founder initializations passed"
