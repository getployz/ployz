#!/usr/bin/env bash

set -euo pipefail

repoint_homebrew_tap() {
    local tap=$1 formula=$2
    mkdir -p "$tap/Formula"
    cp "$formula" "$tap/Formula/ployz.rb"
    rm -f "$tap/Casks/ployz.rb"
    rmdir "$tap/Casks" 2>/dev/null || true
    cat > "$tap/README.md" <<'EOF'
# homebrew-ployz

Homebrew tap for the Ployz CLI reconstructed at https://github.com/getployz/ployz2.

```
brew install getployz/ployz/ployz
```

This package replaces the older `getployz/ployz` implementation as a **clean break** with manual transition, not an in-place compatibility promise. Existing configuration and cluster state from that product are not migrated.
EOF
}

if [ "${PLOYZ_HOMEBREW_TEST_ONLY:-false}" != true ]; then
    tap=${1:-}
    formula=${2:-}
    [ -n "$tap" ] && [ -n "$formula" ] || {
        echo "usage: $0 <tap-checkout> <formula>" >&2
        exit 1
    }
    repoint_homebrew_tap "$tap" "$formula"
fi
