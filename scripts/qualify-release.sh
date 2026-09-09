#!/usr/bin/env bash
# Qualify two draft musl releases against real Linux Machines.
# Pass SSH targets. This script does not pick a cloud vendor.
#
#   PLOYZ_QUALIFY_HOSTS='root@192.0.2.10 root@192.0.2.11' \
#   PLOYZ_ARTIFACT_DIR=/path/to/source/dist \
#   PLOYZ_UPGRADE_ARTIFACT_DIR=/path/to/target/dist \
#   scripts/qualify-release.sh
#
# Optional: PLOYZ_QUALIFY_DRY_RUN=1, PLOYZ_QUALIFY_SSH_KEY,
# PLOYZ_QUALIFY_CONTEXT, PLOYZ_QUALIFY_RESET=1.
# Hosts must be uninitialized unless PLOYZ_QUALIFY_RESET=1. Reset destroys
# managed containers on that Machine.

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
COMPOSE_DIR=$ROOT/scripts/qualify-release
HOSTS=${PLOYZ_QUALIFY_HOSTS:-}
ARTIFACT_DIR=${PLOYZ_ARTIFACT_DIR:-}
UPGRADE_ARTIFACT_DIR=${PLOYZ_UPGRADE_ARTIFACT_DIR:-}
DRY_RUN=${PLOYZ_QUALIFY_DRY_RUN:-0}
CONTEXT=${PLOYZ_QUALIFY_CONTEXT:-qualify}
RESET=${PLOYZ_QUALIFY_RESET:-0}
SSH_KEY=${PLOYZ_QUALIFY_SSH_KEY:-}
CONFIG_DIR=
TRAFFIC_PID=
ENROLL_FIXTURE_PID=
ENROLL_PORT=
TRAFFIC_STOP=/var/lib/ployz/qualification/traffic.stop
REMOTE_RELEASE_ROOT=/var/lib/ployz/qualification/releases
APP_URL=http://127.0.0.1:18082/identity
APP_VALUE=qualify-persistent-data
APP_VOLUME=qualify-release_qualify-data
APP_VOLUME_MOUNT=/var/lib/ployz-volumes/$APP_VOLUME
PAIRING='{"relayUrl":"http://127.0.0.1:1/","secret":"qualification-synthetic-pairing-secret-v1"}'

error() { echo "ERROR: $*" >&2; exit 1; }

need_file() {
    local directory=$1 label=$2 file=$3
    [ -f "$directory/$file" ] || error "$label is missing $file"
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

verify_archive() {
    local directory=$1 label=$2 archive=$3 expected actual
    expected=$(awk -v archive="$archive" '$2 == archive || $2 == "*" archive { print $1; exit }' "$directory/checksums.txt")
    [ -n "$expected" ] || error "$label checksums.txt has no hash for $archive"
    actual=$(sha256 "$directory/$archive")
    [ "$actual" = "$expected" ] || error "$label $archive checksum was $actual, expected $expected"
}

cli_archive() {
    case "$(uname -s):$(uname -m)" in
        Linux:x86_64) echo ployz_linux_amd64.tar.gz ;;
        Linux:aarch64) echo ployz_linux_arm64.tar.gz ;;
        Darwin:x86_64) echo ployz_macos_amd64.tar.gz ;;
        Darwin:arm64) echo ployz_macos_arm64.tar.gz ;;
        *) error "unsupported controller platform $(uname -s) $(uname -m)" ;;
    esac
}

daemon_archive() {
    case "$1" in
        x86_64) echo ployzd_linux_amd64.tar.gz ;;
        aarch64) echo ployzd_linux_arm64.tar.gz ;;
        *) error "unsupported Machine architecture $1" ;;
    esac
}

SSH_ARGS=()
if [ -n "$SSH_KEY" ]; then
    SSH_ARGS=(-i "$SSH_KEY")
fi

ssh_host() {
    # shellcheck disable=SC2029 # Every post-host argument is intentionally interpreted remotely.
    ssh "${SSH_ARGS[@]}" "$@"
}

scp_host() {
    scp "${SSH_ARGS[@]}" "$@"
}

json_field() {
    python3 -c 'import json, sys
value = json.load(sys.stdin)
for part in sys.argv[1].split("."):
    value = value[part]
print(value)' "$1"
}

machine_state_signature() {
    ssh_host "$first" sudo cat /var/lib/ployz/machine.json | python3 -c 'import hashlib, json, sys
value = json.load(sys.stdin)
def stable(item):
    if isinstance(item, dict):
        return {key: stable(child) for key, child in item.items() if key not in {"runtime", "selected_endpoints"}}
    if isinstance(item, list):
        return [stable(child) for child in item]
    return item
encoded = json.dumps(stable(value), sort_keys=True, separators=(",", ":")).encode()
print(hashlib.sha256(encoded).hexdigest())'
}

machine_cloud_pairing() {
    ssh_host "$first" sudo cat /var/lib/ployz/machine.json | python3 -c 'import json, sys
value = json.load(sys.stdin)["cloud_pairing"]
print(json.dumps(value, sort_keys=True, separators=(",", ":")))'
}

start_enroll_fixture() {
    local port_file=$work/cloud-enroll.port i
    python3 "$COMPOSE_DIR/cloud-enroll-fixture.py" "$port_file" "$work/cloud-enroll-evidence.jsonl" >"$work/cloud-enroll.log" 2>&1 &
    ENROLL_FIXTURE_PID=$!
    for ((i = 0; i < 50; i++)); do
        if [ -s "$port_file" ]; then
            ENROLL_PORT=$(cat "$port_file")
            return
        fi
        kill -0 "$ENROLL_FIXTURE_PID" 2>/dev/null || break
        sleep 0.1
    done
    cat "$work/cloud-enroll.log" >&2 || true
    error "Cloud enrollment fixture did not start"
}

finish_enroll_fixture() {
    if ! wait "$ENROLL_FIXTURE_PID"; then
        cat "$work/cloud-enroll.log" >&2 || true
        error "Cloud enrollment fixture rejected the enrollment contract"
    fi
    ENROLL_FIXTURE_PID=
    [ "$(wc -l <"$work/cloud-enroll-evidence.jsonl")" -eq 2 ] || error "Cloud enrollment fixture did not observe exactly two requests"
}

read_receipt() {
    ssh_host "$first" 'sudo cat /var/lib/ployz/upgrade-attempt.json 2>/dev/null || true'
}

wait_for_new_receipt() {
    local previous=$1 receipt attempt_id i
    for ((i = 0; i < 150; i++)); do
        receipt=$(read_receipt)
        attempt_id=$(printf '%s' "$receipt" | json_field attempt.attempt_id 2>/dev/null || true)
        if [ -n "$attempt_id" ] && [ "$attempt_id" != "$previous" ]; then
            printf '%s\n' "$receipt"
            return
        fi
        sleep 0.2
    done
    error "Machine did not record a new upgrade attempt within 30 seconds"
}

wait_for_outcome() {
    local attempt_id=$1 expected=$2 receipt observed_id outcome i
    for ((i = 0; i < 900; i++)); do
        receipt=$(read_receipt)
        observed_id=$(printf '%s' "$receipt" | json_field attempt.attempt_id 2>/dev/null || true)
        outcome=$(printf '%s' "$receipt" | json_field attempt.outcome 2>/dev/null || true)
        if [ "$observed_id" = "$attempt_id" ] && [ "$outcome" = "$expected" ]; then
            printf '%s\n' "$receipt"
            return
        fi
        case "$outcome" in
            failed|interrupted|succeeded)
                [ "$observed_id" != "$attempt_id" ] || error "upgrade $attempt_id ended as $outcome, expected $expected"
                ;;
        esac
        sleep 1
    done
    error "upgrade $attempt_id did not become $expected within 15 minutes"
}

require_field() {
    local json=$1 field=$2 expected=$3 actual
    actual=$(printf '%s' "$json" | json_field "$field")
    [ "$actual" = "$expected" ] || error "$field was $actual, expected $expected"
}

wait_for_application() {
    local value i
    for ((i = 0; i < 120; i++)); do
        value=$(ssh_host "$first" "curl -fsS --max-time 2 '$APP_URL'" 2>/dev/null || true)
        if [ "$value" = "$APP_VALUE" ]; then
            return
        fi
        sleep 1
    done
    error "qualification application did not serve its persistent value"
}

assert_provisioned_volume() {
    local volumes
    volumes=$("$PLOYZ" volume ls --context "$CONTEXT")
    printf '%s\n' "$volumes"
    printf '%s\n' "$volumes" | awk -F '\t' -v volume="$APP_VOLUME" '
        $2 == volume && $3 == "PROVISIONED" && $6 == "ployz" { found = 1 }
        END { exit !found }
    ' || error "$APP_VOLUME is not a provisioned Ployz Volume"
    ssh_host "$first" "test \"\$(sudo docker volume inspect '$APP_VOLUME' --format '{{.Driver}}')\" = ployz" || error "$APP_VOLUME is not backed by the Ployz Docker driver"
    ssh_host "$first" "sudo zfs list -H -o mountpoint | grep -Fqx '$APP_VOLUME_MOUNT'" || error "$APP_VOLUME has no mounted ZFS dataset"
}

start_traffic() {
    ssh_host "$first" "sudo rm -f '$TRAFFIC_STOP'"
    # shellcheck disable=SC2016 # The loop and its variables belong to the remote shell.
    ssh_host "$first" 'while [ ! -e /var/lib/ployz/qualification/traffic.stop ]; do value=$(curl -fsS --max-time 2 http://127.0.0.1:18082/identity 2>&1 || true); if [ "$value" = qualify-persistent-data ]; then echo ok; else printf "failure: %s\n" "$value"; fi; sleep 0.2; done' >"$work/traffic.log" 2>&1 &
    TRAFFIC_PID=$!
}

stop_traffic() {
    if [ -z "$TRAFFIC_PID" ]; then
        return 0
    fi
    ssh_host "$first" "sudo touch '$TRAFFIC_STOP'" >/dev/null 2>&1 || true
    wait "$TRAFFIC_PID" || true
    TRAFFIC_PID=
}

cleanup() {
    stop_traffic
    if [ -n "$ENROLL_FIXTURE_PID" ]; then
        kill "$ENROLL_FIXTURE_PID" >/dev/null 2>&1 || true
        wait "$ENROLL_FIXTURE_PID" >/dev/null 2>&1 || true
        ENROLL_FIXTURE_PID=
    fi
    [ -z "${work:-}" ] || rm -rf "$work"
    [ -z "$CONFIG_DIR" ] || rm -rf "$CONFIG_DIR"
}

stage_remote_release() {
    local label=$1 directory=$2 archive=$3 upload
    upload=/tmp/ployz-qualify-$$-$label
    ssh_host "$first" "rm -rf '$upload' && mkdir -p '$upload'"
    scp_host "$directory/$archive" "$directory/checksums.txt" "$first:$upload/"
    ssh_host "$first" "sudo rm -rf '$REMOTE_RELEASE_ROOT/$label' && sudo install -d -m 0755 '$REMOTE_RELEASE_ROOT/$label' && sudo install -m 0644 '$upload/$archive' '$upload/checksums.txt' '$REMOTE_RELEASE_ROOT/$label/' && rm -rf '$upload'"
}

select_remote_release() {
    ssh_host "$first" "sudo ln -sfn '$REMOTE_RELEASE_ROOT/$1' '$REMOTE_RELEASE_ROOT/current'"
}

assert_remote_hash() {
    local path=$1 expected=$2
    ssh_host "$first" "test \"\$(sudo sha256sum '$path' | awk '{print \$1}')\" = '$expected'"
}

[ -n "$HOSTS" ] || error "set PLOYZ_QUALIFY_HOSTS to one or more user@host targets"
[ -n "$ARTIFACT_DIR" ] || error "set PLOYZ_ARTIFACT_DIR to the source draft archive directory"
[ -n "$UPGRADE_ARTIFACT_DIR" ] || error "set PLOYZ_UPGRADE_ARTIFACT_DIR to the target draft archive directory"
[ -f "$COMPOSE_DIR/compose.yaml" ] || error "missing $COMPOSE_DIR/compose.yaml"
command -v python3 >/dev/null 2>&1 || error "python3 is required to inspect qualification evidence"

read -r -a HOST_LIST <<<"$HOSTS"
[ "${#HOST_LIST[@]}" -ge 1 ] || error "PLOYZ_QUALIFY_HOSTS is empty"

for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz; do
    need_file "$ARTIFACT_DIR" PLOYZ_ARTIFACT_DIR "$archive"
done
need_file "$ARTIFACT_DIR" PLOYZ_ARTIFACT_DIR "$(cli_archive)"
need_file "$ARTIFACT_DIR" PLOYZ_ARTIFACT_DIR checksums.txt
need_file "$UPGRADE_ARTIFACT_DIR" PLOYZ_UPGRADE_ARTIFACT_DIR "$(cli_archive)"
need_file "$UPGRADE_ARTIFACT_DIR" PLOYZ_UPGRADE_ARTIFACT_DIR checksums.txt

if [ "$DRY_RUN" != 0 ]; then
    echo "qualify dry-run"
    echo "hosts: ${HOST_LIST[*]}"
    echo "source artifacts: $ARTIFACT_DIR"
    echo "target artifacts: $UPGRADE_ARTIFACT_DIR"
    echo "compose: $COMPOSE_DIR/compose.yaml"
    echo "ssh-key: ${SSH_KEY:-cli-default}"
    if [ "$RESET" != 0 ]; then
        echo "reset: yes (--yes on machine init)"
    else
        echo "reset: no (initialized hosts fail without PLOYZ_QUALIFY_RESET=1)"
    fi
    echo "steps: normal ZFS machine init/add from the source release; synthetic loopback Cloud Pairing; persistent application traffic; target upgrade with client disconnect and reconnect; corrupt preflight; failed activation; explicit previous-binary repair"
    exit 0
fi

work=$(mktemp -d)
trap cleanup EXIT
mkdir -p "$work/source-cli" "$work/target-cli"
verify_archive "$ARTIFACT_DIR" source "$(cli_archive)"
verify_archive "$UPGRADE_ARTIFACT_DIR" target "$(cli_archive)"
tar -xzf "$ARTIFACT_DIR/$(cli_archive)" -C "$work/source-cli"
tar -xzf "$UPGRADE_ARTIFACT_DIR/$(cli_archive)" -C "$work/target-cli"
PLOYZ=$work/source-cli/ployz
TARGET_PLOYZ=$work/target-cli/ployz
[ -x "$PLOYZ" ] || error "source CLI archive did not contain ployz"
[ -x "$TARGET_PLOYZ" ] || error "target CLI archive did not contain ployz"
source_version=$("$PLOYZ" version) || error "could not read source CLI version"
target_version=$("$TARGET_PLOYZ" version) || error "could not read target CLI version"
[ "$source_version" != "$target_version" ] || error "source and target versions must differ"
CONFIG_DIR=$(mktemp -d)
export PLOYZ_CONFIG=$CONFIG_DIR/config.yaml
export PLOYZ_RELEASE_DIR=$ARTIFACT_DIR

first=${HOST_LIST[0]}
echo "machine init $first at $source_version"
init_cmd=("$PLOYZ" machine init --version "$source_version" --storage zfs --name qualify-1 --context "$CONTEXT" --no-dns --no-ingress)
if [ "$RESET" != 0 ]; then
    init_cmd+=(--yes)
fi
if [ -n "$SSH_KEY" ]; then
    init_cmd+=(--ssh-key "$SSH_KEY")
fi
"${init_cmd[@]}" "$first"

i=1
while [ "$i" -lt "${#HOST_LIST[@]}" ]; do
    echo "machine add ${HOST_LIST[$i]}"
    add_cmd=("$PLOYZ" machine add --yes --version "$source_version" --storage zfs --name "qualify-$((i + 1))" --context "$CONTEXT")
    if [ -n "$SSH_KEY" ]; then
        add_cmd+=(--ssh-key "$SSH_KEY")
    fi
    "${add_cmd[@]}" "${HOST_LIST[$i]}"
    i=$((i + 1))
done

echo "establish a synthetic non-null Cloud Pairing through Cloud enrollment"
start_enroll_fixture
"$PLOYZ" cloud enroll pmet_qualification --name qualify-1 --storage none --no-dns --no-ingress --cloud-url "http://127.0.0.1:$ENROLL_PORT" --context "$CONTEXT"
finish_enroll_fixture
pairing_before=$(machine_cloud_pairing)
[ "$pairing_before" = "$PAIRING" ] || error "Machine did not persist the exact synthetic Cloud Pairing"

machine_arch=$(ssh_host "$first" uname -m)
machine_archive=$(daemon_archive "$machine_arch")
need_file "$UPGRADE_ARTIFACT_DIR" PLOYZ_UPGRADE_ARTIFACT_DIR "$machine_archive"
verify_archive "$UPGRADE_ARTIFACT_DIR" target "$machine_archive"

mkdir -p "$work/releases/target" "$work/releases/corrupt" "$work/releases/failure" "$work/target-payload"
cp "$UPGRADE_ARTIFACT_DIR/$machine_archive" "$UPGRADE_ARTIFACT_DIR/checksums.txt" "$work/releases/target/"
cp "$UPGRADE_ARTIFACT_DIR/$machine_archive" "$UPGRADE_ARTIFACT_DIR/checksums.txt" "$work/releases/corrupt/"
printf 'corrupt\n' >>"$work/releases/corrupt/$machine_archive"
tar -xzf "$UPGRADE_ARTIFACT_DIR/$machine_archive" -C "$work/target-payload"
[ -x "$work/target-payload/ployzd" ] || error "target daemon archive did not contain ployzd"
[ -f "$work/target-payload/ployz-uninstall" ] || error "target daemon archive did not contain ployz-uninstall"
target_daemon_hash=$(sha256 "$work/target-payload/ployzd")
cat >"$work/target-payload/ployzd" <<EOF
#!/bin/sh
if [ "\${1:-}" = version ]; then
    printf '%s\n' '$target_version'
    exit 0
fi
echo 'intentional qualification activation failure' >&2
exit 42
EOF
chmod 0755 "$work/target-payload/ployzd"
tar -czf "$work/releases/failure/$machine_archive" -C "$work/target-payload" ployzd ployz-uninstall
failure_daemon_hash=$(sha256 "$work/target-payload/ployzd")
printf '%s  %s\n' "$(sha256 "$work/releases/failure/$machine_archive")" "$machine_archive" >"$work/releases/failure/checksums.txt"

for release in target corrupt failure; do
    stage_remote_release "$release" "$work/releases/$release" "$machine_archive"
done
select_remote_release target
ssh_host "$first" "sudo install -d -m 0755 /etc/systemd/system/ployz.service.d && printf '[Service]\nEnvironment=PLOYZ_UPGRADE_RELEASE_DIR=$REMOTE_RELEASE_ROOT/current\n' | sudo tee /etc/systemd/system/ployz.service.d/qualification.conf >/dev/null && sudo systemctl daemon-reload && sudo systemctl restart ployz.service"

echo "deploy persistent ZFS-backed application"
"$PLOYZ" deploy --yes --context "$CONTEXT" -f "$COMPOSE_DIR/compose.yaml"
assert_provisioned_volume
wait_for_application
state_before=$(machine_state_signature)
start_traffic

echo "upgrade qualify-1 from $source_version to $target_version, then disconnect the requesting client"
"$PLOYZ" machine upgrade "$target_version" --machine qualify-1 --context "$CONTEXT" >"$work/success-client.log" 2>&1 &
client_pid=$!
success_receipt=$(wait_for_new_receipt "")
success_id=$(printf '%s' "$success_receipt" | json_field attempt.attempt_id)
kill "$client_pid" >/dev/null 2>&1 || true
wait "$client_pid" >/dev/null 2>&1 || true
success_receipt=$(wait_for_outcome "$success_id" succeeded)
require_field "$success_receipt" attempt.version "$target_version"
success_inspection=$("$PLOYZ" machine upgrade inspect qualify-1 --attempt "$success_id" --output json --context "$CONTEXT")
require_field "$success_inspection" outcome succeeded
require_field "$success_inspection" attempt_id "$success_id"
require_field "$success_inspection" version "$target_version"
assert_remote_hash /usr/local/bin/ployzd "$target_daemon_hash"
wait_for_application
[ "$(machine_state_signature)" = "$state_before" ] || error "Machine identity, pairing, or configuration changed during upgrade"
[ "$(machine_cloud_pairing)" = "$pairing_before" ] || error "Cloud Pairing changed during upgrade"

echo "reject a checksum-corrupt target before activation"
select_remote_release corrupt
if "$PLOYZ" machine upgrade "$target_version" --machine qualify-1 --context "$CONTEXT" >"$work/corrupt-client.log" 2>&1; then
    error "checksum-corrupt upgrade unexpectedly succeeded"
fi
grep -qi checksum "$work/corrupt-client.log" || error "checksum-corrupt upgrade did not report checksum evidence"
corrupt_receipt=$(wait_for_new_receipt "$success_id")
corrupt_id=$(printf '%s' "$corrupt_receipt" | json_field attempt.attempt_id)
corrupt_receipt=$(wait_for_outcome "$corrupt_id" failed)
require_field "$corrupt_receipt" attempt.stage verifying
assert_remote_hash /usr/local/bin/ployzd "$target_daemon_hash"
ssh_host "$first" 'test ! -e /var/lib/ployz/.upgrade-active'
wait_for_application

echo "activate an intentionally failing daemon and preserve local diagnostic evidence"
select_remote_release failure
"$PLOYZ" machine upgrade "$target_version" --machine qualify-1 --context "$CONTEXT" >"$work/failure-client.log" 2>&1 &
client_pid=$!
failure_receipt=$(wait_for_new_receipt "$corrupt_id")
failure_id=$(printf '%s' "$failure_receipt" | json_field attempt.attempt_id)
kill "$client_pid" >/dev/null 2>&1 || true
wait "$client_pid" >/dev/null 2>&1 || true
failure_receipt=$(wait_for_outcome "$failure_id" failed)
failure_stage=$(printf '%s' "$failure_receipt" | json_field attempt.stage)
case "$failure_stage" in
    restarting|readiness) ;;
    *) error "failed activation was reported at $failure_stage" ;;
esac
ssh_host "$first" "sudo journalctl -u 'ployz-upgrade-$failure_id.service' --no-pager -n 200 || true"
sleep 3
assert_remote_hash /usr/local/bin/ployzd "$failure_daemon_hash"
assert_remote_hash /usr/local/bin/ployzd.previous "$target_daemon_hash"
ssh_host "$first" 'test ! -e /var/lib/ployz/.upgrade-active && ! sudo systemctl is-active --quiet ployz.service'
wait_for_application

echo "repair explicitly from the one retained previous binary"
ssh_host "$first" 'sudo systemctl stop ployz.service || true; sudo cp --preserve=mode,ownership /usr/local/bin/ployzd.previous /usr/local/bin/.ployzd-repair; sudo sync /usr/local/bin/.ployzd-repair; sudo mv /usr/local/bin/.ployzd-repair /usr/local/bin/ployzd; sudo sync /usr/local/bin; sudo systemctl start ployz.service'
failure_inspection=$("$PLOYZ" machine upgrade inspect qualify-1 --attempt "$failure_id" --output json --context "$CONTEXT")
require_field "$failure_inspection" outcome failed
require_field "$failure_inspection" attempt_id "$failure_id"
require_field "$failure_inspection" stage "$failure_stage"
assert_remote_hash /usr/local/bin/ployzd "$target_daemon_hash"
wait_for_application
[ "$(machine_state_signature)" = "$state_before" ] || error "Machine identity, pairing, or configuration changed after explicit repair"
[ "$(machine_cloud_pairing)" = "$pairing_before" ] || error "Cloud Pairing changed after explicit repair"

stop_traffic
if grep -q '^failure:' "$work/traffic.log"; then
    cat "$work/traffic.log" >&2
    error "application traffic failed during daemon upgrade or repair"
fi
traffic_successes=$(grep -c '^ok$' "$work/traffic.log" || true)
[ "$traffic_successes" -ge 5 ] || error "continuous traffic collected only $traffic_successes successful probes"
printf 'continuous application traffic: %s successful probes, 0 failures\n' "$traffic_successes"

assert_provisioned_volume
echo "qualify-release passed on ${HOST_LIST[*]}: $source_version -> $target_version"
