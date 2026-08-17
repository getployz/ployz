#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
out_dir=${PLOYZ_SH_SITE_DIR:-"$ROOT/dist/ployz-sh-site"}
channels_dir=${PLOYZ_SH_CHANNELS_DIR:-}

rm -rf "$out_dir"
mkdir -p "$out_dir"

install -m 0644 "$ROOT/install.sh" "$out_dir/index.html"
install -m 0644 "$ROOT/install.sh" "$out_dir/install.sh"
install -m 0644 "$ROOT/site/_headers" "$out_dir/_headers"

if [ -n "$channels_dir" ]; then
    for name in stable beta; do
        if [ -f "$channels_dir/$name" ]; then
            install -m 0644 "$channels_dir/$name" "$out_dir/$name"
        fi
    done
fi

echo "ployz.sh site staged in $out_dir"
