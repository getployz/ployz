#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

fail() {
    echo "qualify-release check failed: $*" >&2
    exit 1
}

bash -n "$ROOT/scripts/qualify-release.sh"
bash -n "$ROOT/scripts/check-layer3-runner.sh"

if grep -qi vultr "$ROOT/scripts/qualify-release.sh" "$ROOT/docs/RELEASE.md"; then
    fail "authority path still names a cloud vendor"
fi

grep -Fq 'qualify-data' "$ROOT/scripts/qualify-release/compose.yaml" || fail "compose fixture has no named volume"
grep -Fq 'x-volumes:' "$ROOT/scripts/qualify-release/compose.yaml" || fail "compose fixture volume is not provisioned"

if "$ROOT/scripts/qualify-release.sh" >/dev/null 2>&1; then
    fail "qualify-release accepted empty hosts"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
SOURCE=$TMP/source
TARGET=$TMP/target
mkdir -p "$TMP/bin" "$SOURCE" "$TARGET"
export LOG=$TMP/calls.log
export SSH_LOG=$TMP/ssh.log
export QUALIFY_STATE=$TMP/current-release
printf 'target\n' >"$QUALIFY_STATE"

cat >"$SOURCE/ployz" <<'CLI'
#!/bin/sh
set -eu
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
        printf '%s context=%s file=%s yes=%s\n' "$action" "$context" "$file" "$yes" >>"$LOG"
        [ "$action" != volume ] || printf 'MACHINE\tVOLUME\tTYPE\tQUOTA\tUSED\tDRIVER\nqualify-1\tqualify-release_qualify-data\tPROVISIONED\t268435456\t4096\tployz\n'
        ;;
    cloud)
        [ "$2" = enroll ] || { echo "unexpected cloud action: $2" >&2; exit 1; }
        token=$3
        shift 3
        name= storage= context= cloud_url= no_dns=no no_ingress=no
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --name) name=$2; shift ;;
                --storage) storage=$2; shift ;;
                --context) context=$2; shift ;;
                --cloud-url) cloud_url=$2; shift ;;
                --no-dns) no_dns=yes ;;
                --no-ingress) no_ingress=yes ;;
                *) echo "unexpected cloud enroll argument: $1" >&2; exit 1 ;;
            esac
            shift
        done
        printf 'cloud-enroll token=%s name=%s storage=%s context=%s cloud_url=%s no_dns=%s no_ingress=%s\n' "$token" "$name" "$storage" "$context" "$cloud_url" "$no_dns" "$no_ingress" >>"$LOG"
        python3 - "$cloud_url" "$token" <<'PY'
import json
import sys
import urllib.request

origin, token = sys.argv[1:]
url = f"{origin}/api/enroll/{token}"
identity = {
    "protocolVersion": 2,
    "name": "qualify-1",
    "requestedStorage": "none",
    "publicKey": "synthetic-public-key",
}
request = urllib.request.Request(url, json.dumps(identity).encode(), {"Content-Type": "application/json"})
with urllib.request.urlopen(request) as response:
    pairing = json.load(response)["pairing"]
callback = {
    "machineId": "11111111111111111111111111111111",
    "pairingCredential": pairing["secret"],
}
request = urllib.request.Request(f"{url}/callback", json.dumps(callback).encode(), {"Content-Type": "application/json"})
with urllib.request.urlopen(request) as response:
    json.load(response)
PY
        : >"$QUALIFY_PAIRED"
        ;;
    machine)
        action=$2
        shift 2
        case "$action" in
            init|add)
                reset=no key= target= context= no_install=no version= storage= name= no_dns=no no_ingress=no
                while [ "$#" -gt 0 ]; do
                    case "$1" in
                        --yes) reset=yes ;;
                        --ssh-key) key=$2; shift ;;
                        --context) context=$2; shift ;;
                        --version) version=$2; shift ;;
                        --storage) storage=$2; shift ;;
                        --name) name=$2; shift ;;
                        --no-install) no_install=yes ;;
                        --no-dns) no_dns=yes ;;
                        --no-ingress) no_ingress=yes ;;
                        root@*) target=$1 ;;
                        *) echo "unexpected machine argument: $1" >&2; exit 1 ;;
                    esac
                    shift
                done
                printf '%s target=%s reset=%s key=%s context=%s no_install=%s version=%s storage=%s name=%s no_dns=%s no_ingress=%s release=%s\n' "$action" "$target" "$reset" "$key" "$context" "$no_install" "$version" "$storage" "$name" "$no_dns" "$no_ingress" "${PLOYZ_RELEASE_DIR:-}" >>"$LOG"
                ;;
            upgrade)
                if [ "${1:-}" = inspect ]; then
                    shift
                    machine=$1
                    shift
                    attempt= context= output=
                    while [ "$#" -gt 0 ]; do
                        case "$1" in
                            --attempt) attempt=$2; shift ;;
                            --context) context=$2; shift ;;
                            --output) output=$2; shift ;;
                            *) echo "unexpected upgrade inspect argument: $1" >&2; exit 1 ;;
                        esac
                        shift
                    done
                    current=$(cat "$QUALIFY_STATE")
                    printf 'upgrade-inspect machine=%s attempt=%s context=%s output=%s current=%s\n' "$machine" "$attempt" "$context" "$output" "$current" >>"$LOG"
                    case "$current" in
                        target) printf '{"outcome":"succeeded","attempt_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","target":"1.2.4","version":"1.2.4"}\n' ;;
                        failure) printf '{"outcome":"failed","attempt_id":"cccccccccccccccccccccccccccccccc","target":"1.2.4","stage":"restarting","error":"restart daemon failed"}\n' ;;
                        *) exit 1 ;;
                    esac
                    exit
                fi
                release=$1
                shift
                machine= context=
                while [ "$#" -gt 0 ]; do
                    case "$1" in
                        --machine) machine=$2; shift ;;
                        --context) context=$2; shift ;;
                        *) echo "unexpected upgrade argument: $1" >&2; exit 1 ;;
                    esac
                    shift
                done
                current=$(cat "$QUALIFY_STATE")
                printf 'upgrade release=%s machine=%s context=%s current=%s\n' "$release" "$machine" "$context" "$current" >>"$LOG"
                case "$current" in
                    corrupt) echo 'daemon archive checksum mismatch' >&2; exit 1 ;;
                    target|failure) sleep 30 ;;
                    *) exit 1 ;;
                esac
                ;;
            *) echo "unexpected machine action: $action" >&2; exit 1 ;;
        esac
        ;;
    *) echo "unexpected command: $*" >&2; exit 1 ;;
esac
CLI
chmod 0755 "$SOURCE/ployz"

cat >"$TARGET/ployz" <<'CLI'
#!/bin/sh
[ "${1:-}" = version ] && printf '1.2.4\n'
CLI
chmod 0755 "$TARGET/ployz"

cat >"$SOURCE/ployzd" <<'DAEMON'
#!/bin/sh
[ "${1:-}" = version ] && printf '1.2.3\n'
DAEMON
cat >"$TARGET/ployzd" <<'DAEMON'
#!/bin/sh
[ "${1:-}" = version ] && printf '1.2.4\n'
DAEMON
printf '#!/bin/sh\nexit 0\n' >"$SOURCE/ployz-uninstall"
cp "$SOURCE/ployz-uninstall" "$TARGET/ployz-uninstall"
chmod 0755 "$SOURCE/ployzd" "$TARGET/ployzd" "$SOURCE/ployz-uninstall" "$TARGET/ployz-uninstall"

for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz; do
    tar -czf "$SOURCE/$archive" -C "$SOURCE" ployz
    tar -czf "$TARGET/$archive" -C "$TARGET" ployz
done
for archive in ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz; do
    tar -czf "$SOURCE/$archive" -C "$SOURCE" ployzd ployz-uninstall
    tar -czf "$TARGET/$archive" -C "$TARGET" ployzd ployz-uninstall
done
(
    cd "$SOURCE"
    sha256sum ployz_*.tar.gz ployzd_*.tar.gz | sort -k2 >checksums.txt
)
(
    cd "$TARGET"
    sha256sum ployz_*.tar.gz ployzd_*.tar.gz | sort -k2 >checksums.txt
)

cat >"$TMP/bin/ssh" <<'SSH'
#!/bin/sh
set -eu
key=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -i) key=$2; shift 2 ;;
        *) break ;;
    esac
done
[ "$key" = /tmp/qualify-key ] || { echo 'remote command lost SSH identity' >&2; exit 1; }
host=$1
shift
command=$*
printf 'ssh host=%s command=%s\n' "$host" "$command" >>"$SSH_LOG"
case "$command" in
    *'uname -m'*) printf 'x86_64\n' ;;
    *'ln -sfn'*'/target'*'/current'*) printf 'target\n' >"$QUALIFY_STATE" ;;
    *'ln -sfn'*'/corrupt'*'/current'*) printf 'corrupt\n' >"$QUALIFY_STATE" ;;
    *'ln -sfn'*'/failure'*'/current'*) printf 'failure\n' >"$QUALIFY_STATE" ;;
    *'upgrade-attempt.json'*)
        case "$(cat "$QUALIFY_STATE")" in
            target) printf '{"requested":"1.2.4","attempt":{"outcome":"succeeded","attempt_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","target":"1.2.4","version":"1.2.4"}}\n' ;;
            corrupt) printf '{"requested":"1.2.4","attempt":{"outcome":"failed","attempt_id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","target":"1.2.4","stage":"verifying","error":"checksum mismatch"}}\n' ;;
            failure) printf '{"requested":"1.2.4","attempt":{"outcome":"failed","attempt_id":"cccccccccccccccccccccccccccccccc","target":"1.2.4","stage":"restarting","error":"restart daemon failed"}}\n' ;;
        esac
        ;;
    *'sudo cat /var/lib/ployz/machine.json'*)
        if [ -f "$QUALIFY_PAIRED" ]; then
            pairing='{"relayUrl":"http://127.0.0.1:1/","secret":"qualification-synthetic-pairing-secret-v1"}'
        else
            pairing=null
        fi
        printf '{"body":{"phase":"participating","machine":{"id":"11111111111111111111111111111111","runtime":{"daemon_version":"1.2.3"}}},"wireguard_private_key":"secret","cloud_pairing":%s,"selected_endpoints":{}}\n' "$pairing"
        ;;
    *'while [ ! -e /var/lib/ployz/qualification/traffic.stop'*)
        printf 'ok\nok\nok\nok\nok\nok\n'
        ;;
    *'curl -fsS --max-time 2'*) printf 'qualify-persistent-data\n' ;;
    *'journalctl -u'*) printf 'intentional qualification activation failure\n' ;;
esac
SSH
chmod 0755 "$TMP/bin/ssh"

cat >"$TMP/bin/scp" <<'SCP'
#!/bin/sh
set -eu
key=
if [ "${1:-}" = -i ]; then
    key=$2
    shift 2
fi
[ "$key" = /tmp/qualify-key ] || { echo 'remote copy lost SSH identity' >&2; exit 1; }
printf 'scp %s\n' "$*" >>"$SSH_LOG"
SCP
chmod 0755 "$TMP/bin/scp"
export QUALIFY_PAIRED=$TMP/paired

for reset in 0 1; do
    : >"$LOG"
    rm -f "$QUALIFY_PAIRED"
    PATH="$TMP/bin:$PATH" PLOYZ_QUALIFY_HOSTS='root@192.0.2.10 root@192.0.2.11' PLOYZ_ARTIFACT_DIR="$SOURCE" \
        PLOYZ_UPGRADE_ARTIFACT_DIR="$TARGET" PLOYZ_QUALIFY_SSH_KEY=/tmp/qualify-key \
        PLOYZ_QUALIFY_CONTEXT=qualify PLOYZ_QUALIFY_RESET="$reset" \
        "$ROOT/scripts/qualify-release.sh" >/dev/null
    expected_reset=no
    [ "$reset" = 0 ] || expected_reset=yes
    grep -Fxq "init target=root@192.0.2.10 reset=$expected_reset key=/tmp/qualify-key context=qualify no_install=no version=1.2.3 storage=zfs name=qualify-1 no_dns=yes no_ingress=yes release=$SOURCE" "$LOG" || fail "init lost its target, release, storage, name, version, reset policy, isolation policy, or SSH identity"
    grep -Fxq "add target=root@192.0.2.11 reset=yes key=/tmp/qualify-key context=qualify no_install=no version=1.2.3 storage=zfs name=qualify-2 no_dns=no no_ingress=no release=$SOURCE" "$LOG" || fail "add lost its target, release, storage, name, version, or SSH identity"
    grep -Eq '^cloud-enroll token=pmet_qualification name=qualify-1 storage=none context=qualify cloud_url=http://127\.0\.0\.1:[0-9]+ no_dns=yes no_ingress=yes$' "$LOG" || fail "Cloud Pairing fixture was not exercised through the source CLI"
    grep -Fxq "deploy context=qualify file=$ROOT/scripts/qualify-release/compose.yaml yes=yes" "$LOG" || fail "persistent-volume fixture was not deployed"
    grep -Fxq 'upgrade release=1.2.4 machine=qualify-1 context=qualify current=target' "$LOG" || fail "target upgrade was not requested"
    grep -Fxq 'upgrade release=1.2.4 machine=qualify-1 context=qualify current=corrupt' "$LOG" || fail "corrupt preflight was not requested"
    grep -Fxq 'upgrade release=1.2.4 machine=qualify-1 context=qualify current=failure' "$LOG" || fail "failed activation was not requested"
    grep -Fxq 'upgrade-inspect machine=qualify-1 attempt=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa context=qualify output=json current=target' "$LOG" || fail "successful attempt was not inspected after reconnect"
    grep -Fxq 'upgrade-inspect machine=qualify-1 attempt=cccccccccccccccccccccccccccccccc context=qualify output=json current=failure' "$LOG" || fail "failed attempt was not inspected after repair"
done
grep -Fq 'ssh host=root@192.0.2.10 command=' "$SSH_LOG" || fail "SSH evidence path was not exercised"
grep -Fq 'docker volume inspect' "$SSH_LOG" || fail "qualification did not verify the Docker Volume driver"
grep -Fq 'zfs list -H -o mountpoint' "$SSH_LOG" || fail "qualification did not verify the ZFS dataset mount"
grep -Fq 'scp ' "$SSH_LOG" || fail "target release was not staged with SCP"

cat >"$TMP/bin/cargo" <<'CARGO'
#!/bin/sh
case " $* " in
    *' --no-run '*' --ignored '*|*' --ignored '*' --no-run '*) echo 'informing tests must execute' >&2; exit 1 ;;
    *' --no-run '*)
        [ ! -s "$LOG" ] || exit 1
        printf 'compile\n' >>"$LOG"
        exit 0 ;;
    *' --no-fail-fast '*)
        case " $* " in
            *' --ignored '*|*' --include-ignored '*) printf '%s\n' "$*" >>"$LOG" ;;
            *) echo 'informing tests must be selected' >&2; exit 1 ;;
        esac ;;
    *) echo 'test invocation must collect all binary failures' >&2; exit 1 ;;
esac
case " $* " in
    *' --test build_layer3 '*) exit "${CARGO_SUITE_EXIT:-0}" ;;
esac
CARGO
chmod 0755 "$TMP/bin/cargo"
: >"$LOG"
PATH="$TMP/bin:$PATH" PLOYZ_LAYER3_LOG_DIR="$TMP/layer3-logs" GITHUB_STEP_SUMMARY="$TMP/layer3-summary.md" \
    "$ROOT/scripts/run-layer3-tests.sh" >/dev/null
grep -Eq -- '--(include-)?ignored' "$LOG" || fail "layer3 runner did not execute tests"

for status in 1 124; do
    : >"$LOG"
    if PATH="$TMP/bin:$PATH" PLOYZ_LAYER3_LOG_DIR="$TMP/layer3-logs" GITHUB_STEP_SUMMARY="$TMP/layer3-summary.md" CARGO_SUITE_EXIT="$status" \
        "$ROOT/scripts/run-layer3-tests.sh" >/dev/null; then
        fail "layer3 runner hid suite failure $status"
    fi
    [ "$(wc -l < "$LOG")" -eq 13 ] || fail "layer3 runner skipped or retried suites after failure"
    [ "$(grep -c -- '--test build_layer3 ' "$LOG")" -eq 1 ] || fail "failed suite was rerun"
    grep -Fq 'deploy_execution_preserves_partial_effects' "$LOG" || fail "last suite did not run after failure"
    grep -Fq "failed (exit $status)" "$TMP/layer3-logs/results.md" || fail "suite failure was not reported"
    grep -Fq "failed (exit $status)" "$TMP/layer3-summary.md" || fail "suite failure was missing from the job summary"
done

output=$(
    PLOYZ_QUALIFY_HOSTS='root@192.0.2.10 root@192.0.2.11' PLOYZ_ARTIFACT_DIR="$SOURCE" \
        PLOYZ_UPGRADE_ARTIFACT_DIR="$TARGET" PLOYZ_QUALIFY_DRY_RUN=1 \
        PLOYZ_QUALIFY_RESET=0 "$ROOT/scripts/qualify-release.sh"
)
printf '%s\n' "$output" | grep -Fq 'qualify dry-run' || fail "dry-run did not print the plan"
printf '%s\n' "$output" | grep -Fq 'normal ZFS machine init/add' || fail "dry-run omitted normal storage setup"
printf '%s\n' "$output" | grep -Fq 'client disconnect and reconnect' || fail "dry-run omitted connection loss"
printf '%s\n' "$output" | grep -Fq 'failed activation' || fail "dry-run omitted failure evidence"
printf '%s\n' "$output" | grep -Fq 'reset: no' || fail "dry-run omitted the default no-reset policy"

"$ROOT/scripts/check-layer3-runner.sh"
echo "qualify-release contracts passed"
