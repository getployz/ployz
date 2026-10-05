#!/usr/bin/env bash
# Re-records the four prototype GIFs. All four record into a staging dir first;
# the tracked GIFs are replaced only after every recording succeeded.
set -euo pipefail
shopt -s inherit_errexit
MEDIA=$(cd -- "$(dirname -- "$0")" && pwd) || exit 1
STAGE=$(mktemp -d "$MEDIA/.make.XXXXXX") || exit 1
readonly MEDIA STAGE
trap 'rm -rf "$STAGE"' EXIT
cargo build --release --quiet --manifest-path "$MEDIA/../prototype/Cargo.toml"
export PATH="$MEDIA/../prototype/target/release:$PATH"
prompt() { printf '\033[1;32m$\033[0m %s\r\n' "$1"; sleep 0.6; }
export -f prompt

# rec NAME ROWS SCRIPT [EXIT]   record.py writes NAME.gif only when SCRIPT exits EXIT (default 0)
rec() {
  local name=$1 rows=$2 script=$3 expect=${4:-0}
  "$MEDIA/../record.py" --cols 80 --rows "$rows" --expect-exit "$expect" --gif "$STAGE/$name.gif" -- "ployz() { ployz-ui-prototype \"\$@\"; }; $script"
}
rec success 12 'prompt "ployz deploy"; ployz deploy'
rec fail 20 'prompt "ployz deploy"; ployz deploy --fail' 1
rec plain 24 'prompt "ployz deploy 2>&1 | cat     # piped or CI: same events, plain lines"; ployz deploy 2>&1 | cat'
rec ls 26 'for c in "" " | cat" " --json"; do prompt "ployz service ls$c"; eval "ployz ls$c"; echo; done'
mv -f -- "$STAGE"/*.gif "$MEDIA"/
