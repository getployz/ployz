#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
LOG=$TMP/calls.log
export LOG
FAKE_BIN=$TMP/fake-bin
export FAKE_BIN
mkdir -p "$FAKE_BIN"
for command in systemctl journalctl docker ss; do
    cat > "$FAKE_BIN/$command" <<EOF
#!/bin/sh
printf '%s %s\\n' '$command' "\$*" >> "\$LOG"
EOF
    chmod 0755 "$FAKE_BIN/$command"
done

cat > "$TMP/ployz" <<'EOF'
#!/bin/sh
printf 'ployz' >> "$LOG"
printf ' <%s>' "$@" >> "$LOG"
printf '\n' >> "$LOG"
[ "${PLOYZ_AUTO_CONFIRM+x}" != x ] || exit 99
[ "$3" != root@two ]
EOF
cat > "$TMP/ssh" <<'EOF'
#!/bin/sh
printf 'ssh <%s>\n' "$1" >> "$LOG"
PATH="$FAKE_BIN:$PATH" sh -c "$2"
EOF
chmod 0755 "$TMP/ployz" "$TMP/ssh"

unsupported_status=0
PLOYZ_BIN="$TMP/ployz" SSH_BIN="$TMP/ssh" PLOYZ_QUALIFY_RUN=run \
    bash "$ROOT/scripts/qualify-clean-init.sh" 0.1.2-beta.23 \
        root@one:2222 >"$TMP/unsupported-output" 2>&1 || unsupported_status=$?
if [ "$unsupported_status" -ne 2 ]; then
    echo "qualification accepted a non-default SSH port" >&2
    exit 1
fi
if [ -s "$LOG" ]; then
    echo "qualification invoked a command for an unsupported destination" >&2
    exit 1
fi

status=0
PLOYZ_AUTO_CONFIRM=1 PLOYZ_BIN="$TMP/ployz" SSH_BIN="$TMP/ssh" PLOYZ_QUALIFY_RUN=run \
    bash "$ROOT/scripts/qualify-clean-init.sh" 0.1.2-beta.23 \
        root@one root@two root@three >"$TMP/output" 2>&1 || status=$?
if [ "$status" -ne 1 ]; then
    echo "qualification exited $status, expected 1" >&2
    exit 1
fi

grep -Fxq 'ployz <machine> <init> <root@one> <--context> <qualify-run-1> <--name> <qualify-1> <--version> <0.1.2-beta.23> <--storage> <none> <--no-dns>' "$LOG"
grep -Fxq 'ployz <machine> <init> <root@two> <--context> <qualify-run-2> <--name> <qualify-2> <--version> <0.1.2-beta.23> <--storage> <none> <--no-dns>' "$LOG"
grep -Fxq 'ployz <machine> <init> <root@three> <--context> <qualify-run-3> <--name> <qualify-3> <--version> <0.1.2-beta.23> <--storage> <none> <--no-dns>' "$LOG"
grep -Fxq 'ssh <root@two>' "$LOG"
for evidence in \
    'systemctl status ployz.service --no-pager' \
    'journalctl -u ployz.service --no-pager -n 200' \
    'docker inspect ployz-corrosion' \
    'docker logs --tail 200 ployz-corrosion' \
    'ss -H -lntup'; do
    grep -Fq "$evidence" "$LOG" || {
        echo "diagnostics omitted: $evidence" >&2
        exit 1
    }
done
grep -Fq '1/3 clean founder initializations failed' "$TMP/output"

echo "clean-init qualification interface passed"
