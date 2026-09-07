#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

fail() {
    echo "qualify-release check failed: $*" >&2
    exit 1
}

bash -n "$ROOT/scripts/qualify-release.sh"
bash -n "$ROOT/scripts/check-layer3-runner.sh"
bash -n "$ROOT/scripts/check-product-paths.sh"

if grep -qi vultr "$ROOT/scripts/qualify-release.sh" "$ROOT/docs/RELEASE.md"; then
    fail "authority path still names a cloud vendor"
fi

grep -Fq 'qualify-data' "$ROOT/scripts/qualify-release/compose.yaml" || fail "compose fixture has no named volume"

if "$ROOT/scripts/qualify-release.sh" >/dev/null 2>&1; then
    fail "qualify-release accepted empty hosts"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin" "$TMP/installed"
export LOG=$TMP/calls.log
cat > "$TMP/ployz" <<'CLI'
#!/bin/sh
case "$1" in
    version) printf '1.2.3\n' ;;
    deploy|volume)
        action=$1
        shift
        if [ "$action" = volume ]; then
            [ "${1:-}" = ls ] || exit 1
            shift
        fi
        context= file= yes=no
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --context) context=$2; shift ;;
                -f) file=$2; shift ;;
                --yes) yes=yes ;;
                *) echo "unexpected $action argument: $1" >&2; exit 1 ;;
            esac
            shift
        done
        printf '%s context=%s file=%s yes=%s\n' "$action" "$context" "$file" "$yes" >> "$LOG"
        [ "$action" != volume ] || printf 'qualify-data\n'
        ;;
    machine)
        action=$2
        case "$action" in init|add) ;; *) exit 1 ;; esac
        shift 2
        reset=no key= target= context= no_install=no
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --yes) reset=yes ;;
                --ssh-key) key=$2; shift ;;
                --context) context=$2; shift ;;
                --no-install) no_install=yes ;;
                root@*) target=$1 ;;
                *) echo "unexpected machine argument: $1" >&2; exit 1 ;;
            esac
            shift
        done
        printf '%s target=%s reset=%s key=%s context=%s no_install=%s\n' "$action" "$target" "$reset" "$key" "$context" "$no_install" >> "$LOG"
        ;;
    *) echo "unexpected command: $*" >&2; exit 1 ;;
esac
CLI
chmod 0755 "$TMP/ployz"
for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz; do
    tar -czf "$TMP/$archive" -C "$TMP" ployz
done

# Same-version local archives must still replace the installed payload.
for payload in old new; do
    printf '#!/bin/sh\nif [ "$1" = version ]; then echo 1.2.3; else echo %s; fi\n' "$payload" > "$TMP/$payload"
    chmod 0755 "$TMP/$payload"
done
cp "$TMP/old" "$TMP/installed/ployzd"
cp "$TMP/new" "$TMP/ployzd"
cp "$TMP/new" "$TMP/ployz-uninstall"
for archive in ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz; do
    tar -czf "$TMP/$archive" -C "$TMP" ployzd ployz-uninstall
done
(
    export PLOYZ_INSTALL_TEST_ONLY=true INSTALL_BIN_DIR="$TMP/installed" PLOYZ_RELEASE_DIR="$TMP" PLOYZ_VERSION=1.2.3
    source "$ROOT/scripts/install.sh"
    install_binaries
)
[ "$("$TMP/installed/ployzd")" = new ] || fail "same-version local archive was not installed"

export SSH_LOG=$TMP/ssh.log
cat > "$TMP/bin/ssh" <<'SSH'
#!/bin/sh
set -eu
key=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -i) key=$2; shift 2 ;;
        -o) shift 2 ;;
        *) break ;;
    esac
done
[ "$key" = /tmp/qualify-key ] || { echo 'remote command lost SSH identity' >&2; exit 1; }
printf '%s identity=%s\n' "${0##*/}" "$key" >> "$SSH_LOG"
case "$*" in *'uname -m') echo x86_64 ;; esac
SSH
chmod 0755 "$TMP/bin/ssh"
ln -s ssh "$TMP/bin/scp"
for reset in 0 1; do
    : > "$LOG"
    PATH="$TMP/bin:$PATH" PLOYZ_QUALIFY_HOSTS='root@192.0.2.10 root@192.0.2.11' PLOYZ_ARTIFACT_DIR="$TMP" \
        PLOYZ_QUALIFY_SSH_OPTS='-i /tmp/qualify-key' PLOYZ_QUALIFY_CONTEXT=qualify PLOYZ_QUALIFY_RESET="$reset" \
        "$ROOT/scripts/qualify-release.sh" >/dev/null
    expected_reset=no
    [ "$reset" = 0 ] || expected_reset=yes
    grep -Fxq "init target=root@192.0.2.10 reset=$expected_reset key=/tmp/qualify-key context=qualify no_install=yes" "$LOG" || fail "init lost its target, reset policy, or SSH identity"
    grep -Fxq 'add target=root@192.0.2.11 reset=yes key=/tmp/qualify-key context=qualify no_install=yes' "$LOG" || fail "add lost its target or SSH identity"
    grep -Fxq "deploy context=qualify file=$ROOT/scripts/qualify-release/compose.yaml yes=yes" "$LOG" || fail "named-volume fixture was not deployed"
    grep -Fxq 'volume context=qualify file= yes=no' "$LOG" || fail "volume ls did not query the qualification context"
done
for command in ssh scp; do
    grep -Fxq "$command identity=/tmp/qualify-key" "$SSH_LOG" || fail "$command was not exercised"
done

cat > "$TMP/bin/cargo" <<'CARGO'
#!/bin/sh
case " $* " in
    *' --no-run '*' --ignored '*|*' --ignored '*' --no-run '*) echo 'informing tests must execute' >&2; exit 1 ;;
    *' --no-run '*)
        [ ! -s "$LOG" ] || exit 1
        printf 'compile\n' >> "$LOG"
        exit 0 ;;
    *' --no-fail-fast '*)
        case " $* " in
            *' --ignored '*|*' --include-ignored '*) printf '%s\n' "$*" >> "$LOG" ;;
            *) echo 'informing tests must be selected' >&2; exit 1 ;;
        esac ;;
    *) echo 'test invocation must collect all binary failures' >&2; exit 1 ;;
esac
CARGO
chmod 0755 "$TMP/bin/cargo"
: > "$LOG"
PATH="$TMP/bin:$PATH" "$ROOT/scripts/run-layer3-tests.sh"
grep -Eq -- '--(include-)?ignored' "$LOG" || fail "layer3 runner did not execute tests"

output=$(
    PLOYZ_QUALIFY_HOSTS='root@192.0.2.10 root@192.0.2.11' PLOYZ_ARTIFACT_DIR="$TMP" \
        PLOYZ_QUALIFY_DRY_RUN=1 PLOYZ_QUALIFY_RESET=0 "$ROOT/scripts/qualify-release.sh"
)
printf '%s\n' "$output" | grep -Fq 'qualify dry-run' || fail "dry-run did not print the plan"
printf '%s\n' "$output" | grep -Fq 'named volume' || fail "dry-run omitted the named-volume step"
printf '%s\n' "$output" | grep -Fq 'reset: no' || fail "dry-run omitted the default no-reset policy"
printf '%s\n' "$output" | grep -Fq 'always replace' || fail "dry-run omitted forced daemon replace"

"$ROOT/scripts/check-layer3-runner.sh"
"$ROOT/scripts/check-product-paths.sh"
echo "qualify-release contracts passed"
