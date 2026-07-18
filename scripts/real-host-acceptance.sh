#!/usr/bin/env bash
# Fresh-host acceptance: Rocky 9 amd64 core + Ubuntu 24.04 arm64 edge.
# See docs/operations/real-host-acceptance.md before running.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)

ts() { date +%s; }
log() { echo "[$(date +%H:%M:%S)] $*"; }

zfs_phase() {
  local number=$1
  local name=$2
  shift 2
  local started elapsed phase_log tee_pid
  started=$(ts)
  log "ZFS PHASE ${number}: ${name}"
  phase_log="$ZFS_EVIDENCE_DIR/${number}-${name}.log"

  # This must remain a simple command. Calling a phase function from `if`, `||`,
  # or another tested context disables Bash errexit throughout that function.
  # Process substitution streams the phase while the function stays in this
  # shell, so successful phases can publish identity for later phases.
  "$@" > >(tee "$phase_log") 2>&1
  tee_pid=$!
  wait "$tee_pid"

  elapsed=$(( $(ts) - started ))
  printf 'phase_%s_%s_seconds=%s\n' "$number" "${name//-/_}" "$elapsed" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  log "TIMING zfs-${name}=${elapsed}s"
}

real_host_acceptance_phase_failure_probe() {
  printf 'before-failure\n'
  false
  printf 'after-failure\n'
}

real_host_acceptance_phase_success_probe() {
  phase_probe_identity=preserved
  printf 'successful-phase\n'
}

valid_boot_id() {
  [[ "$1" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]]
}

restore_verified_module() {
  local source=$1
  local destination=$2
  local expected_sha=$3
  local expected_mode=$4
  local expected_uid=$5
  local expected_gid=$6
  local destination_dir temporary=

  test -f "$source"
  test "$(sha256sum "$source" | awk '{print $1}')" = "$expected_sha"
  if test -f "$destination" \
    && test "$(sha256sum "$destination" | awk '{print $1}')" = "$expected_sha" \
    && test "$(stat -c %a "$destination")" = "$expected_mode" \
    && test "$(stat -c %u "$destination")" = "$expected_uid" \
    && test "$(stat -c %g "$destination")" = "$expected_gid"; then
    return 0
  fi

  destination_dir=$(dirname -- "$destination")
  (
    temporary=$(mktemp "${destination_dir}/.ployz-zfs-module.XXXXXX")
    trap '[ -z "$temporary" ] || rm -f -- "$temporary"' EXIT
    cp -- "$source" "$temporary"
    chown "$expected_uid:$expected_gid" "$temporary"
    chmod "$expected_mode" "$temporary"
    test "$(sha256sum "$temporary" | awk '{print $1}')" = "$expected_sha"
    test "$(stat -c %a "$temporary")" = "$expected_mode"
    test "$(stat -c %u "$temporary")" = "$expected_uid"
    test "$(stat -c %g "$temporary")" = "$expected_gid"
    mv -f -- "$temporary" "$destination"
    temporary=
  )
  test "$(sha256sum "$destination" | awk '{print $1}')" = "$expected_sha"
  test "$(stat -c %a "$destination")" = "$expected_mode"
  test "$(stat -c %u "$destination")" = "$expected_uid"
  test "$(stat -c %g "$destination")" = "$expected_gid"
}

wait_for_storage_ready_testimony() {
  local expected_pool=$1
  local forbidden_alarm_pin=${2:-}
  local timeout_seconds=${STORAGE_READY_TIMEOUT_SECONDS:-360}
  local deadline=$((SECONDS + timeout_seconds))
  local testimony= storage_alarms=

  while (( SECONDS < deadline )); do
    if testimony=$(core_before_deadline "$deadline" "timeout 12s ployz machine inspect '${core_machine_id}'"); then
      storage_alarms=$(printf '%s\n' "$testimony" | sed -n 's/^storage-alarms //p')
      if printf '%s\n' "$testimony" | grep -Fx "storage ready pool=${expected_pool}" >/dev/null \
        && { [ -z "$forbidden_alarm_pin" ] \
          || ! printf '%s\n' "$storage_alarms" | tr ',' '\n' | cut -d: -f1 \
            | grep -Fx "$forbidden_alarm_pin" >/dev/null; }; then
        printf '%s\n' "$testimony"
        return 0
      fi
    fi
    sleep_before_deadline "$deadline" 3
  done
  printf '%s\n' "$testimony"
  log "storage testimony did not report ready pool=${expected_pool} without forbidden alarm ${forbidden_alarm_pin:-none} within ${timeout_seconds} seconds"
  return 1
}

wait_for_core_reboot() {
  local prior_boot_id=$1
  local deadline_description=${2:-9 minutes}
  local saw_disconnect=0
  local current_boot_id=
  local disconnect_deadline=$((SECONDS + ${REBOOT_DISCONNECT_TIMEOUT_SECONDS:-120}))
  local return_deadline

  log "waiting for core reboot ${deadline_description}" >&2
  while (( SECONDS < disconnect_deadline )); do
    if ! core_before_deadline "$disconnect_deadline" true 2>/dev/null; then
      saw_disconnect=1
      break
    fi
    sleep_before_deadline "$disconnect_deadline" 2
  done
  [ "$saw_disconnect" = 1 ] || { log "core did not disconnect for reboot" >&2; return 1; }

  return_deadline=$((SECONDS + ${REBOOT_RETURN_TIMEOUT_SECONDS:-540}))
  while (( SECONDS < return_deadline )); do
    if current_boot_id=$(core_before_deadline "$return_deadline" 'cat /proc/sys/kernel/random/boot_id' 2>/dev/null) \
      && valid_boot_id "$current_boot_id" \
      && [ "$current_boot_id" != "$prior_boot_id" ]; then
      printf '%s\n' "$current_boot_id"
      return 0
    fi
    sleep_before_deadline "$return_deadline" 3
  done
  log "core did not return with a new boot id within ${deadline_description}" >&2
  return 1
}

write_zfs_recovery_instructions() {
  local output=$1
  cat > "$output" <<EOF
# ZFS module recovery for the original system from root@${CORE} or provider rescue.
  set -euo pipefail
EOF
  declare -f restore_verified_module >> "$output"
  cat >> "$output" <<EOF
  system_root=\${PLOYZ_RECOVERY_MOUNT_ROOT:-/}
  case "\$system_root" in
    /*) ;;
    *) printf 'PLOYZ_RECOVERY_MOUNT_ROOT must be absolute\\n' >&2; exit 1 ;;
  esac
  if [ "\$system_root" != / ]; then
    canonical_root=\$(readlink -f -- "\$system_root")
    test "\$canonical_root" = "\$system_root"
    mountpoint -q -- "\$system_root"
  fi
  system_path() {
    if [ "\$system_root" = / ]; then
      printf '%s\\n' "\$1"
    else
      printf '%s%s\\n' "\$system_root" "\$1"
    fi
  }
  manifest=\$(system_path '${recovery_root}/zfs-module-recovery.env')
  module_path='${module_path}'
  module_backup='${module_backup}'
  quarantined_module='${quarantined_module}'
  test -f "\$manifest"
  test "\$(awk -F= '\$1 == "module_path" {print \$2}' "\$manifest")" = "\$module_path"
  test "\$(awk -F= '\$1 == "backup_path" {print \$2}' "\$manifest")" = "\$module_backup"
  test "\$(awk -F= '\$1 == "quarantine_path" {print \$2}' "\$manifest")" = "\$quarantined_module"
  expected=\$(awk -F= '\$1 == "module_sha256" {print \$2}' "\$manifest")
  expected_mode=\$(awk -F= '\$1 == "module_mode" {print \$2}' "\$manifest")
  expected_uid=\$(awk -F= '\$1 == "module_uid" {print \$2}' "\$manifest")
  expected_gid=\$(awk -F= '\$1 == "module_gid" {print \$2}' "\$manifest")
  expected_machine_id=\$(awk -F= '\$1 == "machine_id" {print \$2}' "\$manifest")
  kernel=\$(awk -F= '\$1 == "kernel" {print \$2}' "\$manifest")
  test -n "\$expected"
  test -n "\$expected_mode"
  test -n "\$expected_uid"
  test -n "\$expected_gid"
  test -n "\$expected_machine_id"
  test -n "\$kernel"
  test "\$(tr -d '\\n' < "\$(system_path /etc/machine-id)")" = "\$expected_machine_id"
  test -d "\$(system_path /lib/modules/\$kernel)"
  rooted_module_path=\$(system_path "\$module_path")
  rooted_module_backup=\$(system_path "\$module_backup")
  rooted_quarantined_module=\$(system_path "\$quarantined_module")
  test -f "\$rooted_module_backup"
  test -f "\$rooted_quarantined_module"
  test "\$(sha256sum "\$rooted_module_backup" | awk '{print \$1}')" = "\$expected"
  test "\$(sha256sum "\$rooted_quarantined_module" | awk '{print \$1}')" = "\$expected"
  restore_verified_module "\$rooted_module_backup" "\$rooted_module_path" \
    "\$expected" "\$expected_mode" "\$expected_uid" "\$expected_gid"
  if [ "\$system_root" = / ]; then
    depmod -a "\$kernel"
    test "\$(modinfo -k "\$kernel" -n zfs)" = "\$module_path"
    reboot
  else
    chroot "\$system_root" depmod -a "\$kernel"
    test "\$(chroot "\$system_root" modinfo -k "\$kernel" -n zfs)" = "\$module_path"
    sync
    printf 'module restored under %s; unmount it and reboot the original system\\n' "\$system_root"
  fi
# No pool, dataset, volume, container data, or host is destroyed by this harness.
EOF
}

write_zfs_ssh_restore_program() {
  declare -f restore_verified_module
  cat <<'REMOTE_RESTORE'
set -euo pipefail
manifest=$1
module_path=$2
module_backup=$3
quarantined_module=$4

test -f "$manifest"
test "$(awk -F= '$1 == "module_path" {print $2}' "$manifest")" = "$module_path"
test "$(awk -F= '$1 == "backup_path" {print $2}' "$manifest")" = "$module_backup"
test "$(awk -F= '$1 == "quarantine_path" {print $2}' "$manifest")" = "$quarantined_module"
expected=$(awk -F= '$1 == "module_sha256" {print $2}' "$manifest")
expected_mode=$(awk -F= '$1 == "module_mode" {print $2}' "$manifest")
expected_uid=$(awk -F= '$1 == "module_uid" {print $2}' "$manifest")
expected_gid=$(awk -F= '$1 == "module_gid" {print $2}' "$manifest")
expected_machine_id=$(awk -F= '$1 == "machine_id" {print $2}' "$manifest")
kernel=$(awk -F= '$1 == "kernel" {print $2}' "$manifest")
test -n "$expected"
test -n "$expected_mode"
test -n "$expected_uid"
test -n "$expected_gid"
test -n "$expected_machine_id"
test -n "$kernel"
test "$(tr -d '\n' < /etc/machine-id)" = "$expected_machine_id"
test -f "$module_backup"
test -f "$quarantined_module"
test "$(sha256sum "$module_backup" | awk '{print $1}')" = "$expected"
test "$(sha256sum "$quarantined_module" | awk '{print $1}')" = "$expected"
restore_verified_module "$module_backup" "$module_path" \
  "$expected" "$expected_mode" "$expected_uid" "$expected_gid"
depmod -a "$kernel"
test "$(modinfo -k "$kernel" -n zfs)" = "$module_path"
REMOTE_RESTORE
}

configure_acceptance_architecture() {
  local zfs_certify=$1
  local amd64_edge_waiver=$2

  case "$amd64_edge_waiver" in
    0)
      ACCEPTANCE_ARCHITECTURE_MODE=mixed-amd64-arm64
      BUILD_PLATFORM_ARGUMENTS='--platform linux/amd64 --platform linux/arm64'
      BUILD_EXPECTED_PLATFORMS='linux/amd64,linux/arm64'
      BUILD_ARCHITECTURES_LABEL='amd64 and arm64'
      EDGE_ARCHITECTURE_LABEL='arm64 Ubuntu edge'
      EDGE_ACCEPTED_ARCHITECTURES='aarch64 arm64'
      ARCHITECTURE_WAIVER_METADATA=
      ARCHITECTURE_RESULT_METADATA=
      ZFS_SUCCESS_MARKER='ZFS REAL-HOST CERTIFICATION PASSED'
      ACCEPTANCE_SUCCESS_MARKER='ACCEPTANCE PASSED: mixed-arch + firewalld/UFW + public HTTPS'
      ;;
    1)
      if [ "$zfs_certify" != 1 ]; then
        log "PLOYZ_REAL_HOST_AMD64_EDGE_WAIVER=1 is only valid with PLOYZ_REAL_HOST_ZFS_CERTIFY=1"
        return 1
      fi
      ACCEPTANCE_ARCHITECTURE_MODE=amd64-only-non-mixed
      BUILD_PLATFORM_ARGUMENTS='--platform linux/amd64'
      BUILD_EXPECTED_PLATFORMS='linux/amd64'
      BUILD_ARCHITECTURES_LABEL=amd64
      EDGE_ARCHITECTURE_LABEL='amd64 Ubuntu edge (ZFS waiver)'
      EDGE_ACCEPTED_ARCHITECTURES=x86_64
      ARCHITECTURE_WAIVER_METADATA=$'edge_arch_waiver=arm64_provider_capacity_unavailable\narchitecture_mode=amd64-only-non-mixed\nbuild_platforms=linux/amd64\nissue_391_arm_native_build=not-applicable\n'
      ARCHITECTURE_RESULT_METADATA=$'issue_536_result=valid\n'
      ZFS_SUCCESS_MARKER='ZFS REAL-HOST CERTIFICATION PASSED: AMD64 EDGE WAIVER (NON-MIXED)'
      ACCEPTANCE_SUCCESS_MARKER='ACCEPTANCE PASSED: amd64 edge waiver; mixed architecture not certified'
      ;;
    *)
      log "PLOYZ_REAL_HOST_AMD64_EDGE_WAIVER must be 0 or 1"
      return 1
      ;;
  esac
}

write_authenticated_git_fixture_program() {
  local core_ipv4=$1

  cat <<SETUP_GIT
set -euo pipefail
rm -rf /tmp/ployz-build-git
mkdir -p /tmp/ployz-build-git/work/dockerfile /tmp/ployz-build-git/work/railpack /tmp/ployz-build-git/work/slow
mv /tmp/ployz-authenticated-git-server.py /tmp/ployz-build-git/server.py
chmod 0700 /tmp/ployz-build-git/server.py
printf '%s\n' 'FROM alpine:3.20' 'COPY marker /marker' 'CMD ["sh", "-c", "while true; do sleep 600; done"]' > /tmp/ployz-build-git/work/dockerfile/Dockerfile
printf '%s\n' 'real-host exact Dockerfile commit' > /tmp/ployz-build-git/work/dockerfile/marker
printf '%s\n' '{"scripts":{"start":"node server.js"},"engines":{"node":"22"}}' > /tmp/ployz-build-git/work/railpack/package.json
printf '%s\n' 'require("http").createServer((_, res) => res.end("real-host railpack\\n")).listen(process.env.PORT || 3000);' > /tmp/ployz-build-git/work/railpack/server.js
printf '%s\n' 'FROM alpine:3.20' 'RUN echo blocking-build-start && sleep 600' 'CMD ["true"]' > /tmp/ployz-build-git/work/slow/Dockerfile
git -C /tmp/ployz-build-git/work init -q -b main
git -C /tmp/ployz-build-git/work config user.name 'Ployz acceptance'
git -C /tmp/ployz-build-git/work config user.email 'acceptance@example.invalid'
git -C /tmp/ployz-build-git/work add .
GIT_AUTHOR_DATE='2026-01-01T00:00:00Z' GIT_COMMITTER_DATE='2026-01-01T00:00:00Z' git -C /tmp/ployz-build-git/work commit -qm fixture
git clone -q --bare /tmp/ployz-build-git/work /tmp/ployz-build-git/repo.git
git -C /tmp/ployz-build-git/repo.git update-server-info
git -C /tmp/ployz-build-git/work rev-parse HEAD > /tmp/ployz-build-git/commit
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj '/CN=${core_ipv4}' -addext 'subjectAltName=IP:${core_ipv4}' -keyout /tmp/ployz-build-git/server.key -out /tmp/ployz-build-git/server.crt >/dev/null 2>&1
install -m 0644 /tmp/ployz-build-git/server.crt /etc/pki/ca-trust/source/anchors/ployz-build-git.crt
update-ca-trust
firewall-cmd --quiet --add-port=9443/tcp
printf '%s\n' 'PLOYZ_BUILD_GIT_SECRET=build-secret-370' > /tmp/ployz-build-git/secret.env
chmod 0600 /tmp/ployz-build-git/secret.env
systemd-run --unit=ployz-real-host-build-git --setenv=GIT_USERNAME=builder --setenv=GIT_PASSWORD=build-secret-370 /usr/bin/python3 /tmp/ployz-build-git/server.py >/dev/null
for _ in {1..50}; do curl -fsS -u builder:build-secret-370 'https://${core_ipv4}:9443/repo.git/info/refs?service=git-upload-pack' >/dev/null && exit 0; sleep 0.2; done
exit 1
SETUP_GIT
}

run_real_host_acceptance_regression_test() {
  local evidence failure_output success_output reboot_output recovery_output rescue_root fake_bin
  local module_contents module_sha module_mode module_uid module_gid child_status=0
  local restored_inode ssh_restore_output restore_function_output
  local git_install_command fixture_log_command git_install_line fixture_log_line rendered_git_program
  local cancel_command cancel_status_capture cancel_status_guard cancel_watch cancel_watch_status_capture
  local cancel_watch_status_guard json_assertion
  local cancel_command_line cancel_status_capture_line cancel_status_guard_line cancel_watch_line
  local cancel_watch_status_capture_line cancel_watch_status_guard_line json_assertion_line
  evidence=$(mktemp -d)
  trap 'rm -rf "$evidence"' RETURN
  : > "$evidence/metadata.env"

  git_install_command='core "timeout 5m dnf install -y git"'
  fixture_log_command='log "preparing authenticated exact-commit Git build fixture"'
  [ "$(grep -Fxc "$git_install_command" "$0")" -eq 1 ]
  git_install_line=$(grep -Fnx "$git_install_command" "$0" | cut -d: -f1)
  fixture_log_line=$(grep -Fnx "$fixture_log_command" "$0" | cut -d: -f1)
  [ "$git_install_line" -eq $((fixture_log_line - 1)) ]

  cancel_command='core '\''ployz build cancel op_real_host_build_cancel --reason "real-host cancellation proof"'\'''
  cancel_status_capture='cancel_exit=$?'
  cancel_status_guard='[ "$cancel_exit" -eq 1 ] || { log "build cancellation exited ${cancel_exit}, expected 1"; exit 1; }'
  cancel_watch='cancel_events=$(core '\''ployz ops watch op_real_host_build_cancel --json'\'')'
  cancel_watch_status_capture='cancel_watch_exit=$?'
  cancel_watch_status_guard='[ "$cancel_watch_exit" -eq 1 ] || { log "cancelled build watch exited ${cancel_watch_exit}, expected 1"; exit 1; }'
  json_assertion='assert len(cancelled) == 1 and cancelled[0]["cleanup"]["kind"] == "completed"'
  [ "$(grep -Fxc "$cancel_command" "$0")" -eq 1 ]
  [ "$(grep -Fxc "$cancel_status_capture" "$0")" -eq 1 ]
  [ "$(grep -Fxc "$cancel_status_guard" "$0")" -eq 1 ]
  [ "$(grep -Fxc "$cancel_watch" "$0")" -eq 1 ]
  [ "$(grep -Fxc "$cancel_watch_status_capture" "$0")" -eq 1 ]
  [ "$(grep -Fxc "$cancel_watch_status_guard" "$0")" -eq 1 ]
  [ "$(grep -Fxc "$json_assertion" "$0")" -eq 1 ]
  cancel_command_line=$(grep -Fnx "$cancel_command" "$0" | cut -d: -f1)
  cancel_status_capture_line=$(grep -Fnx "$cancel_status_capture" "$0" | cut -d: -f1)
  cancel_status_guard_line=$(grep -Fnx "$cancel_status_guard" "$0" | cut -d: -f1)
  cancel_watch_line=$(grep -Fnx "$cancel_watch" "$0" | cut -d: -f1)
  cancel_watch_status_capture_line=$(grep -Fnx "$cancel_watch_status_capture" "$0" | cut -d: -f1)
  cancel_watch_status_guard_line=$(grep -Fnx "$cancel_watch_status_guard" "$0" | cut -d: -f1)
  json_assertion_line=$(grep -Fnx "$json_assertion" "$0" | cut -d: -f1)
  [ "$(sed -n "$((cancel_command_line - 1))p" "$0")" = 'set +e' ]
  [ "$cancel_status_capture_line" -eq $((cancel_command_line + 1)) ]
  [ "$(sed -n "$((cancel_status_capture_line + 1))p" "$0")" = 'set -e' ]
  [ "$cancel_status_guard_line" -eq $((cancel_status_capture_line + 2)) ]
  [ "$(sed -n "$((cancel_watch_line - 1))p" "$0")" = 'set +e' ]
  [ "$cancel_watch_status_capture_line" -eq $((cancel_watch_line + 1)) ]
  [ "$(sed -n "$((cancel_watch_status_capture_line + 1))p" "$0")" = 'set -e' ]
  [ "$cancel_watch_status_guard_line" -eq $((cancel_watch_status_capture_line + 2)) ]
  [ "$json_assertion_line" -gt "$cancel_watch_status_guard_line" ]

  rendered_git_program="$evidence/authenticated-git-fixture.sh"
  write_authenticated_git_fixture_program 192.0.2.1 > "$rendered_git_program"
  grep -Fx "openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj '/CN=192.0.2.1' -addext 'subjectAltName=IP:192.0.2.1' -keyout /tmp/ployz-build-git/server.key -out /tmp/ployz-build-git/server.crt >/dev/null 2>&1" "$rendered_git_program" >/dev/null
  grep -F "'https://192.0.2.1:9443/repo.git/info/refs?service=git-upload-pack'" "$rendered_git_program" >/dev/null
  grep -Fx "for _ in {1..50}; do curl -fsS -u builder:build-secret-370 'https://192.0.2.1:9443/repo.git/info/refs?service=git-upload-pack' >/dev/null && exit 0; sleep 0.2; done" "$rendered_git_program" >/dev/null
  if grep -F '$(' "$rendered_git_program" >/dev/null; then
    return 1
  fi
  bash -n "$rendered_git_program"

  failure_output=$("$0" --phase-failure-probe "$evidence" 2>&1) || child_status=$?
  [ "$child_status" -ne 0 ]
  printf '%s\n' "$failure_output" | grep -Fx before-failure >/dev/null
  grep -Fx before-failure "$evidence/97-failure-probe.log" >/dev/null
  if grep -Fx after-failure "$evidence/97-failure-probe.log" >/dev/null; then
    return 1
  fi

  success_output=$("$0" --phase-success-probe "$evidence")
  printf '%s\n' "$success_output" | grep -Fx successful-phase >/dev/null
  grep -Fx identity-preserved "$evidence/success.result" >/dev/null
  grep -Fx successful-phase "$evidence/98-success-probe.log" >/dev/null

  reboot_output=$("$0" --reboot-output-probe success 2>"$evidence/reboot-success.stderr")
  [ "$reboot_output" = 22222222-2222-2222-2222-222222222222 ]
  grep -Fx 'waiting for core reboot probe' "$evidence/reboot-success.stderr" >/dev/null
  child_status=0
  "$0" --reboot-output-probe failure >"$evidence/reboot-failure.stdout" \
    2>"$evidence/reboot-failure.stderr" || child_status=$?
  [ "$child_status" -ne 0 ]
  [ ! -s "$evidence/reboot-failure.stdout" ]
  grep -Fx 'core did not return with a new boot id within probe' \
    "$evidence/reboot-failure.stderr" >/dev/null

  recovery_output="$evidence/recovery-probe.sh"
  "$0" --recovery-instructions-probe "$recovery_output"
  grep -F 'PLOYZ_RECOVERY_MOUNT_ROOT' "$recovery_output" >/dev/null
  # shellcheck disable=SC2016 # This assertion matches literal generated-shell variables.
  grep -Fx '    depmod -a "$kernel"' "$recovery_output" >/dev/null
  # shellcheck disable=SC2016 # This assertion matches literal generated-shell variables.
  grep -Fx '    test "$(modinfo -k "$kernel" -n zfs)" = "$module_path"' "$recovery_output" >/dev/null
  bash -n "$recovery_output"

  ssh_restore_output="$evidence/ssh-restore-probe.sh"
  restore_function_output="$evidence/restore-function.sh"
  write_zfs_ssh_restore_program > "$ssh_restore_output"
  declare -f restore_verified_module > "$restore_function_output"
  head -n "$(wc -l < "$restore_function_output")" "$ssh_restore_output" \
    | cmp -s - "$restore_function_output"
  bash -n "$ssh_restore_output"

  "$0" --storage-ready-probe > "$evidence/storage-ready.stdout"
  grep -Fx 'storage ready pool=probe-pool' "$evidence/storage-ready.stdout" >/dev/null
  grep -Fx 'storage-alarms none' "$evidence/storage-ready.stdout" >/dev/null
  grep -Fx 'inspect_calls=3' "$evidence/storage-ready.stdout" >/dev/null

  configure_acceptance_architecture 1 1
  [ "$ACCEPTANCE_ARCHITECTURE_MODE" = amd64-only-non-mixed ]
  [ "$BUILD_PLATFORM_ARGUMENTS" = '--platform linux/amd64' ]
  [ "$BUILD_EXPECTED_PLATFORMS" = linux/amd64 ]
  [ "$BUILD_ARCHITECTURES_LABEL" = amd64 ]
  [ "$EDGE_ARCHITECTURE_LABEL" = 'amd64 Ubuntu edge (ZFS waiver)' ]
  [ "$EDGE_ACCEPTED_ARCHITECTURES" = x86_64 ]
  [ "$ARCHITECTURE_WAIVER_METADATA" = $'edge_arch_waiver=arm64_provider_capacity_unavailable\narchitecture_mode=amd64-only-non-mixed\nbuild_platforms=linux/amd64\nissue_391_arm_native_build=not-applicable\n' ]
  [ "$ARCHITECTURE_RESULT_METADATA" = $'issue_536_result=valid\n' ]
  [ "$ZFS_SUCCESS_MARKER" = \
    'ZFS REAL-HOST CERTIFICATION PASSED: AMD64 EDGE WAIVER (NON-MIXED)' ]
  [ "$ACCEPTANCE_SUCCESS_MARKER" = \
    'ACCEPTANCE PASSED: amd64 edge waiver; mixed architecture not certified' ]
  child_status=0
  configure_acceptance_architecture 0 1 > "$evidence/waiver-without-zfs.stdout" \
    || child_status=$?
  [ "$child_status" -ne 0 ]
  grep -F 'PLOYZ_REAL_HOST_AMD64_EDGE_WAIVER=1 is only valid with PLOYZ_REAL_HOST_ZFS_CERTIFY=1' \
    "$evidence/waiver-without-zfs.stdout" >/dev/null
  configure_acceptance_architecture 1 0
  [ "$ACCEPTANCE_ARCHITECTURE_MODE" = mixed-amd64-arm64 ]
  [ "$BUILD_PLATFORM_ARGUMENTS" = '--platform linux/amd64 --platform linux/arm64' ]
  [ "$BUILD_EXPECTED_PLATFORMS" = linux/amd64,linux/arm64 ]
  [ "$BUILD_ARCHITECTURES_LABEL" = 'amd64 and arm64' ]
  [ "$EDGE_ARCHITECTURE_LABEL" = 'arm64 Ubuntu edge' ]
  [ "$EDGE_ACCEPTED_ARCHITECTURES" = 'aarch64 arm64' ]
  [ -z "$ARCHITECTURE_WAIVER_METADATA" ]
  [ -z "$ARCHITECTURE_RESULT_METADATA" ]
  [ "$ZFS_SUCCESS_MARKER" = 'ZFS REAL-HOST CERTIFICATION PASSED' ]
  [ "$ACCEPTANCE_SUCCESS_MARKER" = \
    'ACCEPTANCE PASSED: mixed-arch + firewalld/UFW + public HTTPS' ]

  rescue_root="$evidence/original-root"
  fake_bin="$evidence/bin"
  mkdir -p "$fake_bin" "$rescue_root/etc" "$rescue_root/lib/modules/probe/extra" \
    "$rescue_root/root/ployz-zfs-recovery-probe/module/lib/modules/probe/extra" \
    "$rescue_root/root/ployz-zfs-recovery-probe/quarantined"
  printf 'probe-machine-id\n' > "$rescue_root/etc/machine-id"
  module_contents=probe-zfs-module
  printf '%s\n' "$module_contents" > \
    "$rescue_root/root/ployz-zfs-recovery-probe/module/lib/modules/probe/extra/zfs.ko.xz"
  cp "$rescue_root/root/ployz-zfs-recovery-probe/module/lib/modules/probe/extra/zfs.ko.xz" \
    "$rescue_root/root/ployz-zfs-recovery-probe/quarantined/zfs.ko.xz"
  module_sha=$(printf '%s\n' "$module_contents" | sha256sum | awk '{print $1}')
  module_mode=$(stat -c %a "$rescue_root/root/ployz-zfs-recovery-probe/quarantined/zfs.ko.xz")
  module_uid=$(id -u)
  module_gid=$(id -g)
  cat > "$rescue_root/root/ployz-zfs-recovery-probe/zfs-module-recovery.env" <<EOF
module_path=/lib/modules/probe/extra/zfs.ko.xz
backup_path=/root/ployz-zfs-recovery-probe/module/lib/modules/probe/extra/zfs.ko.xz
quarantine_path=/root/ployz-zfs-recovery-probe/quarantined/zfs.ko.xz
module_sha256=${module_sha}
module_mode=${module_mode}
module_uid=${module_uid}
module_gid=${module_gid}
initramfs=/boot/initramfs-probe.img
kernel=probe
machine_id=probe-machine-id
EOF
  cat > "$fake_bin/mountpoint" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ "$1" = -q ] && [ "$2" = -- ] && [ "$3" = "$EXPECTED_RESCUE_ROOT" ]
EOF
  cat > "$fake_bin/chroot" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
root=$1
shift
[ "$root" = "$EXPECTED_RESCUE_ROOT" ]
printf '%s\n' "$*" >> "$CHROOT_LOG"
case $1 in
  depmod) [ "$2" = -a ] && [ "$3" = probe ] ;;
  modinfo) printf '/lib/modules/probe/extra/zfs.ko.xz\n' ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$fake_bin/mountpoint" "$fake_bin/chroot"
  EXPECTED_RESCUE_ROOT="$rescue_root" CHROOT_LOG="$evidence/chroot.log" \
    PATH="$fake_bin:$PATH" PLOYZ_RECOVERY_MOUNT_ROOT="$rescue_root" bash "$recovery_output" \
    > "$evidence/rescue.stdout"
  restored_inode=$(stat -c %i "$rescue_root/lib/modules/probe/extra/zfs.ko.xz")
  EXPECTED_RESCUE_ROOT="$rescue_root" CHROOT_LOG="$evidence/chroot.log" \
    PATH="$fake_bin:$PATH" PLOYZ_RECOVERY_MOUNT_ROOT="$rescue_root" bash "$recovery_output" \
    > "$evidence/rescue-retry.stdout"
  [ "$(stat -c %i "$rescue_root/lib/modules/probe/extra/zfs.ko.xz")" = "$restored_inode" ]
  printf 'invalid-module\n' > "$rescue_root/lib/modules/probe/extra/zfs.ko.xz"
  chmod 0600 "$rescue_root/lib/modules/probe/extra/zfs.ko.xz"
  EXPECTED_RESCUE_ROOT="$rescue_root" CHROOT_LOG="$evidence/chroot.log" \
    PATH="$fake_bin:$PATH" PLOYZ_RECOVERY_MOUNT_ROOT="$rescue_root" bash "$recovery_output" \
    > "$evidence/rescue-replacement.stdout"
  grep -Fx 'depmod -a probe' "$evidence/chroot.log" >/dev/null
  grep -Fx 'modinfo -k probe -n zfs' "$evidence/chroot.log" >/dev/null
  grep -Fx "module restored under ${rescue_root}; unmount it and reboot the original system" \
    "$evidence/rescue.stdout" >/dev/null
  grep -Fx "$module_contents" "$rescue_root/lib/modules/probe/extra/zfs.ko.xz" >/dev/null
  [ "$(stat -c %a "$rescue_root/lib/modules/probe/extra/zfs.ko.xz")" = "$module_mode" ]
  [ "$(stat -c %u "$rescue_root/lib/modules/probe/extra/zfs.ko.xz")" = "$module_uid" ]
  [ "$(stat -c %g "$rescue_root/lib/modules/probe/extra/zfs.ko.xz")" = "$module_gid" ]
  printf 'real-host phase regression: PASS\n'
}

case ${1:-} in
  --self-test)
    run_real_host_acceptance_regression_test
    exit 0
    ;;
  --phase-failure-probe)
    ZFS_EVIDENCE_DIR=${2:?missing probe directory}
    zfs_phase 97 failure-probe real_host_acceptance_phase_failure_probe
    exit 0
    ;;
  --phase-success-probe)
    ZFS_EVIDENCE_DIR=${2:?missing probe directory}
    : > "$ZFS_EVIDENCE_DIR/metadata.env"
    phase_probe_identity=
    zfs_phase 98 success-probe real_host_acceptance_phase_success_probe
    [ "$phase_probe_identity" = preserved ]
    printf 'identity-preserved\n' > "$ZFS_EVIDENCE_DIR/success.result"
    exit 0
    ;;
  --reboot-output-probe)
    reboot_probe_mode=${2:?missing reboot probe mode}
    REBOOT_DISCONNECT_TIMEOUT_SECONDS=1
    REBOOT_RETURN_TIMEOUT_SECONDS=1
    reboot_probe_calls=0
    log() { printf '%s\n' "$*" >&2; }
    core_before_deadline() {
      reboot_probe_calls=$((reboot_probe_calls + 1))
      if [ "$reboot_probe_calls" -eq 1 ]; then
        return 255
      fi
      if [ "$reboot_probe_mode" = success ]; then
        printf '22222222-2222-2222-2222-222222222222\n'
        return 0
      fi
      return 255
    }
    sleep_before_deadline() { SECONDS=$1; }
    wait_for_core_reboot 11111111-1111-1111-1111-111111111111 probe
    exit 0
    ;;
  --recovery-instructions-probe)
    recovery_output=${2:?missing recovery probe output}
    CORE=192.0.2.10
    recovery_root=/root/ployz-zfs-recovery-probe
    module_path=/lib/modules/probe/extra/zfs.ko.xz
    module_backup=/root/ployz-zfs-recovery-probe/module/lib/modules/probe/extra/zfs.ko.xz
    quarantined_module=/root/ployz-zfs-recovery-probe/quarantined/zfs.ko.xz
    write_zfs_recovery_instructions "$recovery_output"
    exit 0
    ;;
  --storage-ready-probe)
    core_machine_id=probe-machine
    STORAGE_READY_TIMEOUT_SECONDS=30
    storage_ready_probe_calls=$(mktemp)
    trap 'rm -f "$storage_ready_probe_calls"' EXIT
    core_before_deadline() {
      printf 'inspect\n' >> "$storage_ready_probe_calls"
      case $(wc -l < "$storage_ready_probe_calls") in
        1) printf 'storage ready pool=stale-pool\nstorage-alarms none\n' ;;
        2) printf 'storage ready pool=probe-pool\nstorage-alarms probe-ns/probe-volume:zfs-module-missing\n' ;;
        *) printf 'storage ready pool=probe-pool\nstorage-alarms none\n' ;;
      esac
    }
    sleep_before_deadline() { :; }
    wait_for_storage_ready_testimony probe-pool probe-ns/probe-volume
    printf 'inspect_calls=%s\n' "$(wc -l < "$storage_ready_probe_calls")"
    exit 0
    ;;
esac

ZFS_CERTIFY=${PLOYZ_REAL_HOST_ZFS_CERTIFY:-0}
ZFS_EVIDENCE_DIR=${PLOYZ_REAL_HOST_EVIDENCE_DIR:-}
EXPECTED_RELEASE_TAG=${PLOYZ_EXPECTED_RELEASE_TAG:-}
EXPECTED_RUNTIME_SHA=${PLOYZ_EXPECTED_RUNTIME_SHA:-}
MINIMUM_ZFS_RUNTIME_SHA=2f754ab5cff785fd67cf4c83231f4025ec6ad8ee
ZFS_REQUESTED_MAX_SIZE_BYTES=2147483648

CORE="${1:?usage: real-host-acceptance.sh <core-ip> <edge-ip>}"
EDGE="${2:?usage: real-host-acceptance.sh <core-ip> <edge-ip>}"
SSH_OPTS=(-o StrictHostKeyChecking=accept-new -o GSSAPIAuthentication=no \
          -o ConnectTimeout=20 -o ServerAliveInterval=15)

remote() { ssh "${SSH_OPTS[@]}" "root@$1" "${@:2}"; }
core() { remote "$CORE" "$@"; }
managed_host() { grep -Eo '[A-Za-z0-9.-]+\.up\.ployz\.app' | tail -1 || true; }
valid_ipv4() {
  local part
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || return 1
  IFS=. read -r -a parts <<<"$1"
  for part in "${parts[@]}"; do (( 10#$part <= 255 )) || return 1; done
}

valid_ipv4 "$CORE" || { log "core must be a literal IPv4 address"; exit 1; }
valid_ipv4 "$EDGE" || { log "edge must be a literal IPv4 address"; exit 1; }

if [ "$ZFS_CERTIFY" != 0 ] && [ "$ZFS_CERTIFY" != 1 ]; then
  log "PLOYZ_REAL_HOST_ZFS_CERTIFY must be 0 or 1"
  exit 1
fi
configure_acceptance_architecture \
  "$ZFS_CERTIFY" "${PLOYZ_REAL_HOST_AMD64_EDGE_WAIVER:-0}" || exit 1
if [ "$ZFS_CERTIFY" = 1 ]; then
  [ -n "$ZFS_EVIDENCE_DIR" ] || {
    log "PLOYZ_REAL_HOST_EVIDENCE_DIR is required for ZFS certification"
    exit 1
  }
  [[ "$ZFS_EVIDENCE_DIR" = /* ]] || {
    log "PLOYZ_REAL_HOST_EVIDENCE_DIR must be an absolute path"
    exit 1
  }
  [ -n "$EXPECTED_RELEASE_TAG" ] || {
    log "PLOYZ_EXPECTED_RELEASE_TAG is required for ZFS certification"
    exit 1
  }
  [[ "$EXPECTED_RUNTIME_SHA" =~ ^[0-9a-f]{40}$ ]] || {
    log "PLOYZ_EXPECTED_RUNTIME_SHA must be a full lowercase Git SHA"
    exit 1
  }
  [ -z "$(git -C "$REPO_ROOT" status --porcelain)" ] || {
    log "the local harness worktree must be clean for ZFS certification"
    exit 1
  }
  local_tag_sha=$(git -C "$REPO_ROOT" rev-parse --verify "${EXPECTED_RELEASE_TAG}^{commit}" 2>/dev/null) || {
    log "expected release tag ${EXPECTED_RELEASE_TAG} is not available locally"
    exit 1
  }
  [ "$local_tag_sha" = "$EXPECTED_RUNTIME_SHA" ] || {
    log "expected release tag resolves to ${local_tag_sha}, not ${EXPECTED_RUNTIME_SHA}"
    exit 1
  }
  git -C "$REPO_ROOT" merge-base --is-ancestor "$MINIMUM_ZFS_RUNTIME_SHA" "$EXPECTED_RUNTIME_SHA" || {
    log "runtime ${EXPECTED_RUNTIME_SHA} does not contain required ZFS testimony commit ${MINIMUM_ZFS_RUNTIME_SHA}"
    exit 1
  }
  mkdir -p "$ZFS_EVIDENCE_DIR"
  [ -z "$(find "$ZFS_EVIDENCE_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] || {
    log "ZFS evidence directory must be empty: ${ZFS_EVIDENCE_DIR}"
    exit 1
  }
  harness_sha=$(git -C "$REPO_ROOT" rev-parse HEAD)
  {
    printf 'started_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'core=%s\nedge=%s\n' "$CORE" "$EDGE"
    printf 'release_tag=%s\nruntime_sha=%s\n' "$EXPECTED_RELEASE_TAG" "$EXPECTED_RUNTIME_SHA"
    printf 'harness_sha=%s\nminimum_runtime_sha=%s\n' "$harness_sha" "$MINIMUM_ZFS_RUNTIME_SHA"
    printf '%s' "$ARCHITECTURE_WAIVER_METADATA"
  } > "$ZFS_EVIDENCE_DIR/metadata.env"
  exec > >(tee -a "$ZFS_EVIDENCE_DIR/transcript.log") 2>&1
fi

stopped_host=
stopped_container=
probe_dir=
probe_pid=
git_fixture_dir=
zfs_recovery_message=
cleanup() {
  if [ -n "$probe_pid" ]; then
    touch "$probe_dir/stop"
    wait "$probe_pid" 2>/dev/null || true
  fi
  if [ -n "$stopped_container" ]; then
    remote "$stopped_host" "docker start '${stopped_container}' >/dev/null" || true
  fi
  [ -z "$probe_dir" ] || rm -rf "$probe_dir"
  if [ -n "$git_fixture_dir" ]; then
    core 'systemctl stop ployz-real-host-build-git.service 2>/dev/null || true; firewall-cmd --quiet --remove-port=9443/tcp 2>/dev/null || true; rm -rf /tmp/ployz-authenticated-git-server.py /tmp/ployz-build-git /etc/pki/ca-trust/source/anchors/ployz-build-git.crt; update-ca-trust >/dev/null 2>&1 || true' || true
    remote "$EDGE" 'rm -f /usr/local/share/ca-certificates/ployz-build-git.crt; update-ca-certificates >/dev/null 2>&1 || true' || true
    rm -rf "$git_fixture_dir"
  fi
  if [ -n "$zfs_recovery_message" ]; then
    printf '\n%s\n' "$zfs_recovery_message" >&2
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

log "waiting for both hosts to accept SSH"
for host in "$CORE" "$EDGE"; do
  ready=0
  for _ in $(seq 1 50); do
    if remote "$host" true 2>/dev/null; then ready=1; break; fi
    sleep 6
  done
  [ "$ready" = 1 ] || { log "SSH unavailable on $host"; exit 1; }
done

core_arch=$(core 'uname -m')
edge_arch=$(remote "$EDGE" 'uname -m')
[ "$core_arch" = x86_64 ] || { log "core must be amd64 (x86_64), got $core_arch"; exit 1; }
case " $EDGE_ACCEPTED_ARCHITECTURES " in
  *" $edge_arch "*) ;;
  *) log "edge architecture $edge_arch is not accepted by $ACCEPTANCE_ARCHITECTURE_MODE"; exit 1 ;;
esac
core 'grep -qE "^(ID|ID_LIKE)=.*(rhel|fedora|rocky)" /etc/os-release' || {
  log "core must be Rocky/RHEL-family"; exit 1;
}
remote "$EDGE" 'grep -q "^ID=ubuntu" /etc/os-release' || {
  log "edge must be Ubuntu"; exit 1;
}
if [ "$ZFS_CERTIFY" = 1 ]; then
  core 'source /etc/os-release; [ "$ID" = rocky ] && [[ "$VERSION_ID" = 9* ]]' || {
    log "ZFS certification requires Rocky Linux 9 on the core"; exit 1;
  }
  remote "$EDGE" 'source /etc/os-release; [ "$ID" = ubuntu ] && [ "$VERSION_ID" = 24.04 ]' || {
    log "ZFS certification requires Ubuntu 24.04 on the edge"; exit 1;
  }
  core_os=$(core 'source /etc/os-release; printf "%s %s" "$ID" "$VERSION_ID"')
  edge_os=$(remote "$EDGE" 'source /etc/os-release; printf "%s %s" "$ID" "$VERSION_ID"')
  core_kernel=$(core 'uname -r')
  edge_kernel=$(remote "$EDGE" 'uname -r')
  printf 'core_os=%s\nedge_os=%s\ncore_arch=%s\nedge_arch=%s\ncore_kernel=%s\nedge_kernel=%s\n' \
    "$core_os" "$edge_os" "$core_arch" "$edge_arch" "$core_kernel" "$edge_kernel" >> "$ZFS_EVIDENCE_DIR/metadata.env"
fi

# Rocky images normally start firewalld. Ubuntu images normally ship UFW
# inactive, so allow SSH before enabling it. Keeper owns the Ployz port rules.
core 'systemctl is-active --quiet firewalld' || {
  log "core firewalld must already be active"; exit 1;
}
remote "$EDGE" 'ufw allow OpenSSH >/dev/null && ufw --force enable >/dev/null'

# The operator shells out to ssh for init/add.
for host in "$CORE" "$EDGE"; do
  remote "$host" 'mkdir -p ~/.ssh; printf "Host *\n  GSSAPIAuthentication no\n  StrictHostKeyChecking accept-new\n" > ~/.ssh/config; chmod 600 ~/.ssh/config'
done
core '[ -f ~/.ssh/id_ed25519 ] || ssh-keygen -t ed25519 -N "" -f ~/.ssh/id_ed25519 -q; grep -qF "$(cat ~/.ssh/id_ed25519.pub)" ~/.ssh/authorized_keys || cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys'
core "ssh-keyscan -T 8 ${CORE} 127.0.0.1 >> ~/.ssh/known_hosts 2>/dev/null"

log "installing the public alpha CLI on the core"
core "timeout 10m sh -c 'curl --connect-timeout 20 --max-time 120 -fsSL https://ployz.sh | sh -s -- --channel alpha >/dev/null'"
log "version: $(core 'grep -h PLOYZ_VERSION /etc/ployz/release.env')"
if [ "$ZFS_CERTIFY" = 1 ]; then
  curl --connect-timeout 20 --max-time 120 -fsSL https://ployz.sh/channels/alpha.env \
    -o "$ZFS_EVIDENCE_DIR/live-alpha.env"
  live_alpha_tag=$(awk -F= '$1 == "PLOYZ_RELEASE_TAG" { print substr($0, index($0, "=") + 1); exit }' "$ZFS_EVIDENCE_DIR/live-alpha.env")
  [ "$live_alpha_tag" = "$EXPECTED_RELEASE_TAG" ] || {
    log "live alpha channel points to ${live_alpha_tag:-missing}, not ${EXPECTED_RELEASE_TAG}"
    exit 1
  }
  installed_tag=$(core "awk -F= '\$1 == \"PLOYZ_RELEASE_TAG\" { print substr(\$0, index(\$0, \"=\") + 1); exit }' /etc/ployz/release.env")
  installed_manifest=$(core "awk -F= '\$1 == \"PLOYZ_RELEASE_MANIFEST_URL\" { print substr(\$0, index(\$0, \"=\") + 1); exit }' /etc/ployz/release.env")
  [ "$installed_tag" = "$EXPECTED_RELEASE_TAG" ] || {
    log "installed release tag ${installed_tag:-missing} does not match ${EXPECTED_RELEASE_TAG}"
    exit 1
  }
  case "$installed_manifest" in
    "https://github.com/getployz/ployz/releases/download/${EXPECTED_RELEASE_TAG}/"*) ;;
    *) log "installed manifest is not pinned beneath immutable release tag ${EXPECTED_RELEASE_TAG}"; exit 1 ;;
  esac
  printf 'live_alpha_tag=%s\ncore_installed_tag=%s\ncore_installed_manifest=%s\n' \
    "$live_alpha_tag" "$installed_tag" "$installed_manifest" >> "$ZFS_EVIDENCE_DIR/metadata.env"
fi

log "machine init (amd64 Rocky core)"
t0=$(ts)
core "timeout 15m ployz machine init root@${CORE} --name ployz-core --public-ip ${CORE}"
log "TIMING machine-init=$(( $(ts)-t0 ))s"

core_key=$(core 'cat ~/.ssh/id_ed25519.pub')
remote "$EDGE" "grep -qF '${core_key}' ~/.ssh/authorized_keys 2>/dev/null || echo '${core_key}' >> ~/.ssh/authorized_keys"
core "ssh-keyscan -T 8 ${EDGE} >> ~/.ssh/known_hosts 2>/dev/null"

log "machine add (${EDGE_ARCHITECTURE_LABEL})"
t0=$(ts)
core "timeout 15m ployz machine add root@${EDGE} --name ployz-edge"
log "TIMING machine-add=$(( $(ts)-t0 ))s"
core 'ployz machine list'
if [ "$ZFS_CERTIFY" = 1 ]; then
  edge_installed_tag=$(remote "$EDGE" "awk -F= '\$1 == \"PLOYZ_RELEASE_TAG\" { print substr(\$0, index(\$0, \"=\") + 1); exit }' /etc/ployz/release.env")
  edge_installed_manifest=$(remote "$EDGE" "awk -F= '\$1 == \"PLOYZ_RELEASE_MANIFEST_URL\" { print substr(\$0, index(\$0, \"=\") + 1); exit }' /etc/ployz/release.env")
  [ "$edge_installed_tag" = "$EXPECTED_RELEASE_TAG" ] || {
    log "edge installed release tag ${edge_installed_tag:-missing} does not match ${EXPECTED_RELEASE_TAG}"
    exit 1
  }
  case "$edge_installed_manifest" in
    "https://github.com/getployz/ployz/releases/download/${EXPECTED_RELEASE_TAG}/"*) ;;
    *) log "edge installed manifest is not pinned beneath immutable release tag ${EXPECTED_RELEASE_TAG}"; exit 1 ;;
  esac
  printf 'edge_installed_tag=%s\nedge_installed_manifest=%s\n' \
    "$edge_installed_tag" "$edge_installed_manifest" >> "$ZFS_EVIDENCE_DIR/metadata.env"
fi

core "timeout 5m dnf install -y git"
log "preparing authenticated exact-commit Git build fixture"
git_fixture_dir=$(mktemp -d)
scp "${SSH_OPTS[@]}" \
  "$REPO_ROOT/testing/ployz-e2e/tests/dind_cluster/fixtures/authenticated_git_server.py" \
  "root@${CORE}:/tmp/ployz-authenticated-git-server.py" >/dev/null
write_authenticated_git_fixture_program "$CORE" \
  | ssh "${SSH_OPTS[@]}" "root@${CORE}" 'bash -s'
scp "${SSH_OPTS[@]}" "root@${CORE}:/tmp/ployz-build-git/server.crt" "$git_fixture_dir/server.crt" >/dev/null
scp "${SSH_OPTS[@]}" "$git_fixture_dir/server.crt" "root@${EDGE}:/usr/local/share/ca-certificates/ployz-build-git.crt" >/dev/null
remote "$EDGE" 'update-ca-certificates >/dev/null'
build_commit=$(core 'cat /tmp/ployz-build-git/commit')

assert_build_evidence() {
  local operation_id=$1
  local expected_adapter=$2
  local first second first_index second_index
  first=$(core "ployz ops watch '${operation_id}' --json")
  second=$(core "ployz ops watch '${operation_id}' --json")
  [ "$first" = "$second" ] || { log "${operation_id} operation evidence was not stable"; exit 1; }
  first_index=$(BUILD_EVENTS="$first" python3 -c 'import json,os; events=[json.loads(line)["event"] for line in os.environ["BUILD_EVENTS"].splitlines() if line]; print(next(event["receipt"]["index_digest"] for event in events if event.get("event") == "build_completed"))')
  second_index=$(BUILD_EVENTS="$second" python3 -c 'import json,os; events=[json.loads(line)["event"] for line in os.environ["BUILD_EVENTS"].splitlines() if line]; print(next(event["receipt"]["index_digest"] for event in events if event.get("event") == "build_completed"))')
  [ -n "$first_index" ] && [ "$first_index" = "$second_index" ] || { log "${operation_id} logical index was empty or unstable"; exit 1; }
  BUILD_EVENTS="$first" BUILD_COMMIT="$build_commit" BUILD_ADAPTER="$expected_adapter" \
    BUILD_EXPECTED_PLATFORMS="$BUILD_EXPECTED_PLATFORMS" python3 - <<'PY'
import json, os
events = [json.loads(line)["event"] for line in os.environ["BUILD_EVENTS"].splitlines() if line]
verified = [event for event in events if event.get("event") == "build_commit_verified"]
assert any(event["commit"]["commit"] == os.environ["BUILD_COMMIT"] for event in verified)
completed = [event for event in events if event.get("event") == "build_completed"]
assert len(completed) == 1
platforms = completed[0]["receipt"]["platforms"]
expected_platforms = {tuple(platform.split("/", 1)) for platform in os.environ["BUILD_EXPECTED_PLATFORMS"].split(",")}
assert {(item[0]["os"], item[0]["architecture"]) for item in platforms} == expected_platforms
submitted = [event for event in events if event.get("event") == "build_submitted"]
assert len(submitted) == 1 and submitted[0]["adapter"]["adapter"] == os.environ["BUILD_ADAPTER"]
PY
}

log "building authenticated exact SHA for ${BUILD_ARCHITECTURES_LABEL} with Dockerfile"
core "set -a; . /tmp/ployz-build-git/secret.env; set +a; timeout 30m ployz build submit --git 'https://${CORE}:9443/repo.git' --commit '${build_commit}' --git-username builder --git-secret-env PLOYZ_BUILD_GIT_SECRET --subdir dockerfile ${BUILD_PLATFORM_ARGUMENTS} --dockerfile Dockerfile --operation-id op_real_host_build_dockerfile"
assert_build_evidence op_real_host_build_dockerfile dockerfile

log "building authenticated exact SHA for ${BUILD_ARCHITECTURES_LABEL} with Railpack"
core "set -a; . /tmp/ployz-build-git/secret.env; set +a; timeout 30m ployz build submit --git 'https://${CORE}:9443/repo.git' --commit '${build_commit}' --git-username builder --git-secret-env PLOYZ_BUILD_GIT_SECRET --subdir railpack ${BUILD_PLATFORM_ARGUMENTS} --railpack --cache-scope real-host-railpack --operation-id op_real_host_build_railpack"
assert_build_evidence op_real_host_build_railpack railpack

log "cancelling a blocking authenticated build and checking cleanup evidence"
core "set -a; . /tmp/ployz-build-git/secret.env; set +a; ployz build submit --git 'https://${CORE}:9443/repo.git' --commit '${build_commit}' --git-username builder --git-secret-env PLOYZ_BUILD_GIT_SECRET --subdir slow --platform linux/amd64 --dockerfile Dockerfile --operation-id op_real_host_build_cancel --detach"
for _ in $(seq 1 180); do
  if core 'ployz ops status op_real_host_build_cancel' | grep -q 'state building'; then break; fi
  sleep 1
done
core 'ployz ops status op_real_host_build_cancel' | grep -q 'state building'
set +e
core 'ployz build cancel op_real_host_build_cancel --reason "real-host cancellation proof"'
cancel_exit=$?
set -e
[ "$cancel_exit" -eq 1 ] || { log "build cancellation exited ${cancel_exit}, expected 1"; exit 1; }
set +e
cancel_events=$(core 'ployz ops watch op_real_host_build_cancel --json')
cancel_watch_exit=$?
set -e
[ "$cancel_watch_exit" -eq 1 ] || { log "cancelled build watch exited ${cancel_watch_exit}, expected 1"; exit 1; }
CANCEL_EVENTS="$cancel_events" python3 - <<'PY'
import json, os
events = [json.loads(line)["event"] for line in os.environ["CANCEL_EVENTS"].splitlines() if line]
cancelled = [event for event in events if event.get("event") == "build_cancelled"]
assert len(cancelled) == 1 and cancelled[0]["cleanup"]["kind"] == "completed"
PY

log "checking keeper-managed firewall rules"
for port in 53/udp 4222/tcp 80/tcp 443/tcp 51820/udp; do
  core "firewall-cmd --quiet --query-port=${port} && firewall-cmd --permanent --quiet --query-port=${port}"
done
for port in 53/udp 80/tcp 443/tcp 51820/udp; do
  remote "$EDGE" "ufw status verbose | awk '\$1 != \"${port}\" { next } \$2 == \"(v6)\" { next } { found=1; assured=(\$2 == \"ALLOW\" && \$3 == \"IN\" && \$4 == \"Anywhere\"); exit } END { exit !(found && assured) }'"
done

log "deploying image-based Compose app (2 replicas, managed HTTPS URL)"
core 'cat > /tmp/ployz-acceptance.yml <<"YAML"
name: acceptance
services:
  web:
    image: nginx:alpine
    deploy:
      replicas: 2
    x-ports:
      - auto:web:80
YAML'
t0=$(ts)
deploy_output=$(core 'timeout 15m ployz deploy -f /tmp/ployz-acceptance.yml')
printf '%s\n' "$deploy_output"
log "TIMING deploy=$(( $(ts)-t0 ))s"

hostname=$(printf '%s\n' "$deploy_output" | managed_host)
service_output=$(core 'ployz service inspect web')
printf '%s\n' "$service_output"
if [ -z "$hostname" ]; then
  hostname=$(printf '%s\n' "$service_output" | managed_host)
fi
[ -n "$hostname" ] || { log "managed public HTTPS URL not found"; exit 1; }
core_container=$(printf '%s\n' "$service_output" | awk '/^container .* machine ployz-core / {print $2; exit}')
edge_container=$(printf '%s\n' "$service_output" | awk '/^container .* machine ployz-edge / {print $2; exit}')
[ -n "$core_container" ] || { log "no web replica placed on ployz-core"; exit 1; }
[ -n "$edge_container" ] || { log "no web replica placed on ployz-edge"; exit 1; }

log "public HTTPS URL: https://${hostname}"
code=$(curl --connect-timeout 20 --max-time 60 -sS -o /dev/null -w '%{http_code}' "https://${hostname}/")
log "  public DNS route -> HTTPS ${code}"
[ "$code" = 200 ] || { log "public URL returned ${code}"; exit 1; }

log "cross-machine routing with each gateway's local replica stopped"
for pair in "$CORE:$core_container" "$EDGE:$edge_container"; do
  host=${pair%%:*}
  container=${pair#*:}
  stopped_host=$host
  stopped_container=$container
  remote "$host" "docker stop '${container}' >/dev/null"
  if code=$(remote "$host" "curl --retry 10 --retry-delay 1 --retry-all-errors --connect-timeout 5 --max-time 30 --resolve '${hostname}:443:127.0.0.1' -fsS -o /dev/null -w '%{http_code}' 'https://${hostname}/'"); then
    route_ok=1
  else
    route_ok=0
  fi
  remote "$host" "docker start '${container}' >/dev/null"
  stopped_host=
  stopped_container=
  log "  gateway ${host} -> remote replica -> HTTPS ${code:-failed}"
  [ "$route_ok" = 1 ] && [ "$code" = 200 ] || { log "cross-machine route failed via ${host}"; exit 1; }
done

log "restart invisibility: probing the core gateway while ployzd-control restarts"
probe_dir=$(mktemp -d)
(
  while [ ! -e "$probe_dir/stop" ]; do
    if curl --connect-timeout 2 --max-time 5 --resolve "${hostname}:443:${CORE}" -fsS -o /dev/null "https://${hostname}/"; then
      touch "$probe_dir/ready"
    else
      touch "$probe_dir/failed"
    fi
    sleep 0.1
  done
) &
probe_pid=$!
for _ in $(seq 1 100); do
  [ -e "$probe_dir/ready" ] && break
  sleep 0.1
done
[ -e "$probe_dir/ready" ] || { log "restart probe did not become ready"; exit 1; }
[ ! -e "$probe_dir/failed" ] || { log "route failed before restart"; exit 1; }
if core 'systemctl restart ployzd-control && sleep 3 && systemctl is-active --quiet ployzd-control'; then
  restart_ok=1
else
  restart_ok=0
fi
touch "$probe_dir/stop"
wait "$probe_pid"
probe_pid=
[ "$restart_ok" = 1 ] || { rm -rf "$probe_dir"; log "ployzd-control restart failed"; exit 1; }
[ ! -e "$probe_dir/failed" ] || { rm -rf "$probe_dir"; log "route failed during restart"; exit 1; }
rm -rf "$probe_dir"
probe_dir=
log "  continuous HTTPS probe saw no interruption"

core_before_deadline() {
  local deadline=$1
  shift
  local remaining=$((deadline - SECONDS))
  local attempt
  (( remaining > 0 )) || return 124
  attempt=$remaining
  (( attempt <= 15 )) || attempt=15
  timeout --foreground --signal=TERM --kill-after=2s "${attempt}s" \
    ssh "${SSH_OPTS[@]}" "root@${CORE}" "$@"
}

core_with_local_timeout() {
  local budget=$1
  shift
  timeout --foreground --signal=TERM --kill-after=2s "$budget" \
    ssh "${SSH_OPTS[@]}" "root@${CORE}" "$@"
}

sleep_before_deadline() {
  local deadline=$1
  local interval=$2
  local remaining=$((deadline - SECONDS))
  (( remaining > 0 )) || return 0
  (( interval <= remaining )) || interval=$remaining
  sleep "$interval"
}

reboot_core() {
  local prior_boot_id=$1
  core_with_local_timeout 20s "nohup sh -c 'sleep 1; systemctl reboot' >/dev/null 2>&1 &" || true
  wait_for_core_reboot "$prior_boot_id"
}

wait_for_core_api() {
  local deadline=$((SECONDS + 360))
  while (( SECONDS < deadline )); do
    if core_before_deadline "$deadline" 'systemctl is-active --quiet ployzd-control && timeout 10s ployz machine list >/dev/null' 2>/dev/null; then
      return 0
    fi
    sleep_before_deadline "$deadline" 3
  done
  log "Control/API did not return within 6 minutes"
  return 1
}

zfs_storage_prepare() {
  local testimony_deadline
  storage_prepare_output=$(core "timeout 2m ployz machine storage-prepare '${core_machine_id}' --detach")
  printf '%s\n' "$storage_prepare_output"
  storage_operation_id=$(printf '%s\n' "$storage_prepare_output" | awk '/^operation / { print $2; exit }')
  [ -n "$storage_operation_id" ] || { log "storage preparation did not report an operation id"; return 1; }
  core "timeout 20s ployz ops status '${storage_operation_id}'"
  storage_operation_evidence=$(core "timeout 20m ployz ops watch '${storage_operation_id}' --json")
  printf '%s\n' "$storage_operation_evidence"
  storage_operation_evidence_sha=$(printf '%s' "$storage_operation_evidence" | sha256sum | awk '{print $1}')
  machine_storage=
  pool=
  testimony_deadline=$((SECONDS + 360))
  while (( SECONDS < testimony_deadline )); do
    if machine_storage=$(core_before_deadline "$testimony_deadline" "timeout 12s ployz machine inspect '${core_machine_id}'"); then
      pool=$(printf '%s\n' "$machine_storage" | sed -n 's/^storage ready pool=//p' | head -1)
      [ -n "$pool" ] && break
    fi
    sleep_before_deadline "$testimony_deadline" 3
  done
  printf '%s\n' "$machine_storage"
  [ -n "$pool" ] || { log "storage preparation did not report a ready pool within 6 minutes"; return 1; }
}

zfs_deploy_database() {
  core "timeout 15m ployz volume create '${zfs_namespace}' '${zfs_volume}' --machine '${core_machine_id}' --max-size 2G"
  core "cat > '${remote_compose}' <<'YAML'
name: ${zfs_namespace}
services:
  ${zfs_service}:
    image: postgres:15-alpine
    restart: unless-stopped
    environment:
      POSTGRES_DB: ployzcert
      POSTGRES_USER: ployzcert
      POSTGRES_PASSWORD: ployzcert
    volumes:
      - ${zfs_volume}:/var/lib/postgresql/data
    healthcheck:
      test: [\"CMD-SHELL\", \"pg_isready -U ployzcert -d ployzcert\"]
      interval: 2s
      timeout: 2s
      retries: 60
volumes:
  ${zfs_volume}:
    x-ployz:
      max-size: 2G
YAML"
  core "timeout 20m ployz deploy -f '${remote_compose}'"
  volume_output=$(core 'timeout 20s ployz volume list')
  printf '%s\n' "$volume_output"
  volume_row=$(printf '%s\n' "$volume_output" | awk -v ns="$zfs_namespace" -v volume="$zfs_volume" '$1 == ns && $2 == volume { print; exit }')
  [ -n "$volume_row" ] || { log "provisioned volume was not listed"; return 1; }
  [ "$(printf '%s\n' "$volume_row" | awk '{print $3}')" = "$core_machine_id" ] || {
    log "provisioned volume was not pinned to the Rocky core"; return 1;
  }
  [ "$(printf '%s\n' "$volume_row" | awk '{print $4}')" = provisioned ] || {
    log "volume is not provisioned"; return 1;
  }
  dataset=$(printf '%s\n' "$volume_row" | awk '{print $5}')
  if [ -z "$dataset" ] || [ "$dataset" = - ]; then
    log "provisioned dataset identity is missing"
    return 1
  fi

  db_container=$(core "docker ps -q --filter 'label=plz.namespace_id=${zfs_namespace}' --filter 'label=plz.service_id=${zfs_service}' | head -1")
  [ -n "$db_container" ] || { log "database container is not running on the core"; return 1; }
  baseline_db_container=$db_container
  core "timeout 30s docker exec '${db_container}' psql -U ployzcert -d ployzcert -v ON_ERROR_STOP=1 -c \"CREATE TABLE certification (marker text PRIMARY KEY); INSERT INTO certification VALUES ('${row_marker}');\""
}

zfs_capture_baseline() {
  prepared_descriptor_path=/var/lib/ployz/prepared-storage.json
  backing_file_path=/var/lib/ployz/zfs/ployz.img
  prepared_descriptor=$(core "cat '${prepared_descriptor_path}'")
  printf '%s\n' "$prepared_descriptor"
  PREPARED_DESCRIPTOR="$prepared_descriptor" EXPECTED_POOL="$pool" python3 - <<'PY'
import json
import os

descriptor = json.loads(os.environ["PREPARED_DESCRIPTOR"])
assert descriptor == {
    "pool": os.environ["EXPECTED_POOL"],
    "origin": {
        "kind": "owned_image",
        "backing_file": "/var/lib/ployz/zfs/ployz.img",
    },
    "dataset_root": f'{os.environ["EXPECTED_POOL"]}/ployz/volumes',
}
PY
  prepared_descriptor_sha=$(printf '%s' "$prepared_descriptor" | sha256sum | awk '{print $1}')
  backing_file_realpath=$(core "readlink -f '${backing_file_path}'")
  backing_file_identity=$(core "stat -Lc '%d:%i:%s' '${backing_file_path}'")
  [ "$backing_file_realpath" = "$backing_file_path" ] || {
    log "prepared storage backing file does not resolve to ${backing_file_path}"; return 1;
  }
  pool_guid=$(core "zpool get -H -o value guid '${pool}'")
  pool_health=$(core "zpool get -H -o value health '${pool}'")
  [ "$pool_health" = ONLINE ] || {
    log "pool health is ${pool_health}, expected ONLINE"; return 1;
  }
  dataset_guid=$(core "zfs get -H -o value guid '${dataset}'")
  dataset_mountpoint=$(core "zfs get -H -o value mountpoint '${dataset}'")
  dataset_quota=$(core "zfs get -H -p -o value quota '${dataset}'")
  dataset_refquota=$(core "zfs get -H -p -o value refquota '${dataset}'")
  docker_volume_name=$(core "docker inspect --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Name}}{{end}}{{end}}' '${db_container}'")
  docker_volume_source=$(core "docker inspect --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Source}}{{end}}{{end}}' '${db_container}'")
  bind_device=$(core "docker volume inspect --format '{{index .Options \"device\"}}' '${docker_volume_name}'")
  docker_labels=$(core "docker inspect --format '{{json .Config.Labels}}' '${db_container}'")
  docker_labels_sha=$(printf '%s' "$docker_labels" | sha256sum | awk '{print $1}')
  if [ -z "$pool_guid" ] || [ -z "$dataset_guid" ] || [ -z "$dataset_mountpoint" ] \
    || [ -z "$dataset_quota" ] || [ -z "$dataset_refquota" ] \
    || [ -z "$docker_volume_name" ] || [ -z "$docker_volume_source" ] || [ -z "$bind_device" ] \
    || [ -z "$docker_labels" ]; then
    log "baseline storage identity is incomplete"
    return 1
  fi
  case "$dataset_mountpoint" in
    /var/lib/ployz/volumes/*) ;;
    *) log "dataset mountpoint is outside /var/lib/ployz/volumes"; return 1 ;;
  esac
  [ "$dataset_quota" = "$ZFS_REQUESTED_MAX_SIZE_BYTES" ] || {
    log "dataset quota is ${dataset_quota}, expected ${ZFS_REQUESTED_MAX_SIZE_BYTES}"; return 1;
  }
  [ "$dataset_refquota" = 0 ] || {
    log "dataset refquota is ${dataset_refquota}, expected 0"; return 1;
  }
  [ "$bind_device" = "$dataset_mountpoint" ] || { log "Docker volume is not bound to the dataset mountpoint"; return 1; }
  DOCKER_LABELS="$docker_labels" EXPECTED_NAMESPACE="$zfs_namespace" EXPECTED_SERVICE="$zfs_service" python3 - <<'PY'
import json
import os

labels = json.loads(os.environ["DOCKER_LABELS"])
assert labels["plz.managed"] == "true"
assert labels["plz.namespace_id"] == os.environ["EXPECTED_NAMESPACE"]
assert labels["plz.service_id"] == os.environ["EXPECTED_SERVICE"]
for required in ("plz.namespace_revision_entry", "plz.operation_id", "plz.step_id", "plz.container_type"):
    assert labels.get(required)
PY
  core "mountpoint -q '${dataset_mountpoint}' && test -d '${docker_volume_source}'"
  core "timeout 30s docker exec '${db_container}' psql -At -U ployzcert -d ployzcert -c \"SELECT marker FROM certification WHERE marker='${row_marker}'\"" | grep -Fx "$row_marker"
  {
    printf 'namespace=%s\nvolume=%s\nservice=%s\n' "$zfs_namespace" "$zfs_volume" "$zfs_service"
    printf 'machine_id=%s\nstorage_operation_id=%s\nstorage_operation_evidence_sha256=%s\npool=%s\ndataset=%s\n' \
      "$core_machine_id" "$storage_operation_id" "$storage_operation_evidence_sha" "$pool" "$dataset"
    printf 'pool_guid=%s\npool_health=%s\ndataset_guid=%s\ndataset_mountpoint=%s\n' \
      "$pool_guid" "$pool_health" "$dataset_guid" "$dataset_mountpoint"
    printf 'requested_max_size_bytes=%s\ndataset_quota=%s\ndataset_refquota=%s\n' \
      "$ZFS_REQUESTED_MAX_SIZE_BYTES" "$dataset_quota" "$dataset_refquota"
    printf 'docker_volume_name=%s\ndocker_volume_source=%s\nbind_device=%s\nbaseline_container=%s\n' \
      "$docker_volume_name" "$docker_volume_source" "$bind_device" "$baseline_db_container"
    printf 'prepared_descriptor_path=%s\nprepared_descriptor_sha256=%s\n' "$prepared_descriptor_path" "$prepared_descriptor_sha"
    printf 'backing_file_path=%s\nbacking_file_identity=%s\ndocker_labels_sha256=%s\n' \
      "$backing_file_path" "$backing_file_identity" "$docker_labels_sha"
    printf 'docker_labels=%s\nprepared_descriptor=%s\n' "$docker_labels" "$prepared_descriptor"
    printf 'row_marker=%s\n' "$row_marker"
  } >> "$ZFS_EVIDENCE_DIR/metadata.env"
}

zfs_verify_preserved_storage_evidence() {
  local actual_descriptor actual_descriptor_sha actual_backing_path actual_backing_identity
  local actual_labels actual_labels_sha actual_operation_evidence actual_operation_evidence_sha
  actual_descriptor=$(core "cat '${prepared_descriptor_path}'")
  actual_descriptor_sha=$(printf '%s' "$actual_descriptor" | sha256sum | awk '{print $1}')
  actual_backing_path=$(core "readlink -f '${backing_file_path}'")
  actual_backing_identity=$(core "stat -Lc '%d:%i:%s' '${backing_file_path}'")
  actual_labels=$(core "docker inspect --format '{{json .Config.Labels}}' '${baseline_db_container}'")
  actual_labels_sha=$(printf '%s' "$actual_labels" | sha256sum | awk '{print $1}')
  actual_operation_evidence=$(core "timeout 20s ployz ops watch '${storage_operation_id}' --json")
  actual_operation_evidence_sha=$(printf '%s' "$actual_operation_evidence" | sha256sum | awk '{print $1}')
  if [ "$actual_descriptor_sha" != "$prepared_descriptor_sha" ] \
    || [ "$actual_backing_path" != "$backing_file_path" ] \
    || [ "$actual_backing_identity" != "$backing_file_identity" ] \
    || [ "$actual_labels_sha" != "$docker_labels_sha" ] \
    || [ "$actual_operation_evidence_sha" != "$storage_operation_evidence_sha" ]; then
    log "prepared descriptor, backing file, Docker labels, or operation evidence changed"
    return 1
  fi
  printf 'preserved_descriptor_sha256=%s\npreserved_backing_file=%s\npreserved_backing_identity=%s\npreserved_docker_labels_sha256=%s\npreserved_operation_evidence_sha256=%s\n' \
    "$actual_descriptor_sha" "$actual_backing_path" "$actual_backing_identity" "$actual_labels_sha" "$actual_operation_evidence_sha"
}

zfs_verify_reboot_recovery() {
  local forbidden_alarm_pin=${1:-}
  local recovered_dataset_quota recovered_dataset_refquota recovered_pool_health machine_storage
  wait_for_core_api
  machine_storage=$(wait_for_storage_ready_testimony "$pool" "$forbidden_alarm_pin")
  printf '%s\n' "$machine_storage"
  zfs_verify_preserved_storage_evidence
  core "zfs_time=\$(systemctl show zfs.target -p ActiveEnterTimestampMonotonic --value); docker_time=\$(systemctl show docker.service -p ActiveEnterTimestampMonotonic --value); after=\$(systemctl show docker.service -p After --value); printf 'zfs_active_us=%s\\ndocker_active_us=%s\\ndocker_after=%s\\n' \"\$zfs_time\" \"\$docker_time\" \"\$after\"; [ \"\$zfs_time\" -gt 0 ] && [ \"\$docker_time\" -gt 0 ] && [ \"\$zfs_time\" -le \"\$docker_time\" ] && printf '%s\\n' \"\$after\" | tr ' ' '\\n' | grep -Fx zfs.target >/dev/null"
  core_with_local_timeout 200s "timeout 3m sh -c 'until zpool list -H -o guid \"${pool}\" | grep -Fx \"${pool_guid}\"; do sleep 2; done'"
  recovered_pool_health=$(core "zpool get -H -o value health '${pool}'")
  [ "$recovered_pool_health" = ONLINE ] || {
    log "pool health after reboot is ${recovered_pool_health}, expected ONLINE"; return 1;
  }
  [ "$(core "zfs get -H -o value guid '${dataset}'")" = "$dataset_guid" ] || {
    log "dataset GUID changed across reboot"; return 1;
  }
  recovered_dataset_quota=$(core "zfs get -H -p -o value quota '${dataset}'")
  recovered_dataset_refquota=$(core "zfs get -H -p -o value refquota '${dataset}'")
  if [ "$recovered_dataset_quota" != "$ZFS_REQUESTED_MAX_SIZE_BYTES" ] \
    || [ "$recovered_dataset_quota" != "$dataset_quota" ] \
    || [ "$recovered_dataset_refquota" != 0 ] \
    || [ "$recovered_dataset_refquota" != "$dataset_refquota" ]; then
    log "dataset quota/refquota changed or no longer matches the requested 2 GiB limit"
    return 1
  fi
  printf 'recovered_pool_health=%s\nrecovered_dataset_quota=%s\nrecovered_dataset_refquota=%s\n' \
    "$recovered_pool_health" "$recovered_dataset_quota" "$recovered_dataset_refquota"
  core "mountpoint -q '${bind_device}' && test -d '${docker_volume_source}'"
  recovered_container=
  local container_deadline=$((SECONDS + 240))
  while (( SECONDS < container_deadline )); do
    if ! recovered_container=$(core_before_deadline "$container_deadline" "docker ps -q --filter 'label=plz.namespace_id=${zfs_namespace}' --filter 'label=plz.service_id=${zfs_service}' | head -1"); then
      recovered_container=
    fi
    [ -n "$recovered_container" ] && break
    sleep_before_deadline "$container_deadline" 2
  done
  [ -n "$recovered_container" ] || { log "database container did not return after reboot"; return 1; }
  [ "$recovered_container" = "$baseline_db_container" ] || { log "a replacement database container returned after reboot"; return 1; }
  recovered_volume_name=$(core "docker inspect --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Name}}{{end}}{{end}}' '${recovered_container}'")
  recovered_source=$(core "docker inspect --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Source}}{{end}}{{end}}' '${recovered_container}'")
  recovered_device=$(core "docker volume inspect --format '{{index .Options \"device\"}}' '${recovered_volume_name}'")
  if [ "$recovered_volume_name" != "$docker_volume_name" ] \
    || [ "$recovered_source" != "$docker_volume_source" ] \
    || [ "$recovered_device" != "$bind_device" ]; then
    log "database returned on a different Docker volume or bind device"
    return 1
  fi
  core "timeout 30s docker exec '${recovered_container}' psql -At -U ployzcert -d ployzcert -c \"SELECT marker FROM certification WHERE marker='${row_marker}'\"" | grep -Fx "$row_marker"
  db_container=$recovered_container
}

zfs_quarantine_module() {
  local prepare_command quarantine_command
  recovery_root="/root/ployz-zfs-recovery-${zfs_suffix}"
  module_path=$(core 'modinfo -n zfs')
  [[ "$module_path" = /lib/modules/* ]] || { log "refusing unexpected ZFS module path ${module_path}"; return 1; }
  module_backup="${recovery_root}/module/${module_path#/}"
  quarantined_module="${recovery_root}/quarantined/$(basename "$module_path")"
  initramfs="/boot/initramfs-$(core 'uname -r').img"
  printf '05 exact module path=%s backup=%s quarantine=%s initramfs=%s\n' \
    "$module_path" "$module_backup" "$quarantined_module" "$initramfs" >> "$ZFS_EVIDENCE_DIR/commands.log"

  printf -v prepare_command 'bash -s -- %q %q %q %q %q' \
    "$recovery_root" "$module_path" "$module_backup" "$quarantined_module" "$initramfs"
  # shellcheck disable=SC2029 # printf %q above makes every remote argument a shell word.
  timeout --foreground --signal=TERM --kill-after=2s 120s \
    ssh "${SSH_OPTS[@]}" "root@${CORE}" "$prepare_command" <<'REMOTE_PREPARE'
set -euo pipefail
recovery_root=$1
module_path=$2
module_backup=$3
quarantined_module=$4
initramfs=$5
kernel=$(uname -r)
machine_id=$(tr -d '\n' < /etc/machine-id)
manifest="${recovery_root}/zfs-module-recovery.env"

command -v lsinitrd >/dev/null
test -f "$module_path"
test -f "$initramfs"
test -n "$machine_id"
test ! -e "$recovery_root"
install -d -m 0700 "$(dirname "$module_backup")" "$(dirname "$quarantined_module")"
lsinitrd "$initramfs" > "${recovery_root}/initramfs.inventory"
if grep -Eq '(^|/)(zfs|spl)\.ko(\.(xz|zst|gz))?($|[[:space:]])' "${recovery_root}/initramfs.inventory"; then
  printf 'active initramfs contains ZFS or SPL\n' >&2
  exit 1
fi
lsmod > "${recovery_root}/lsmod.before"
systemctl list-unit-files 'zfs*' --no-pager > "${recovery_root}/zfs-units.before"
systemctl list-units 'zfs*' --all --no-pager >> "${recovery_root}/zfs-units.before"

cp -a "$module_path" "$module_backup"
module_sha=$(sha256sum "$module_path" | awk '{print $1}')
backup_sha=$(sha256sum "$module_backup" | awk '{print $1}')
test "$backup_sha" = "$module_sha"
module_mode=$(stat -c %a "$module_path")
module_uid=$(stat -c %u "$module_path")
module_gid=$(stat -c %g "$module_path")
{
  printf 'module_path=%s\nbackup_path=%s\nquarantine_path=%s\n' "$module_path" "$module_backup" "$quarantined_module"
  printf 'module_sha256=%s\nmodule_mode=%s\nmodule_uid=%s\nmodule_gid=%s\n' "$module_sha" "$module_mode" "$module_uid" "$module_gid"
  printf 'initramfs=%s\nkernel=%s\nmachine_id=%s\n' "$initramfs" "$kernel" "$machine_id"
} > "${manifest}.tmp"
chmod 0600 "${manifest}.tmp"
mv "${manifest}.tmp" "$manifest"
test "$(awk -F= '$1 == "module_sha256" {print $2}' "$manifest")" = "$module_sha"
cat "${recovery_root}/lsmod.before"
cat "${recovery_root}/zfs-units.before"
cat "$manifest"
REMOTE_PREPARE

  timeout --foreground --signal=TERM --kill-after=2s 30s \
    scp "${SSH_OPTS[@]}" "root@${CORE}:${recovery_root}/zfs-module-recovery.env" "$ZFS_EVIDENCE_DIR/zfs-module-recovery.env" >/dev/null
  write_zfs_recovery_instructions "$ZFS_EVIDENCE_DIR/recovery.txt"
  zfs_recovery_message=$(cat "$ZFS_EVIDENCE_DIR/recovery.txt")
  cat "$ZFS_EVIDENCE_DIR/zfs-module-recovery.env"
  cat "$ZFS_EVIDENCE_DIR/recovery.txt"

  printf -v quarantine_command 'bash -s -- %q %q %q %q' \
    "$recovery_root/zfs-module-recovery.env" "$module_path" "$module_backup" "$quarantined_module"
  # shellcheck disable=SC2029 # printf %q above makes every remote argument a shell word.
  timeout --foreground --signal=TERM --kill-after=2s 120s \
    ssh "${SSH_OPTS[@]}" "root@${CORE}" "$quarantine_command" <<'REMOTE_QUARANTINE'
set -euo pipefail
manifest=$1
module_path=$2
module_backup=$3
quarantined_module=$4

test -f "$manifest"
test "$(awk -F= '$1 == "module_path" {print $2}' "$manifest")" = "$module_path"
test "$(awk -F= '$1 == "backup_path" {print $2}' "$manifest")" = "$module_backup"
test "$(awk -F= '$1 == "quarantine_path" {print $2}' "$manifest")" = "$quarantined_module"
expected=$(awk -F= '$1 == "module_sha256" {print $2}' "$manifest")
kernel=$(awk -F= '$1 == "kernel" {print $2}' "$manifest")
test -n "$expected"
test -n "$kernel"
test -f "$module_path"
test -f "$module_backup"
test ! -e "$quarantined_module"
test "$(sha256sum "$module_path" | awk '{print $1}')" = "$expected"
test "$(sha256sum "$module_backup" | awk '{print $1}')" = "$expected"
mv "$module_path" "$quarantined_module"
test "$(sha256sum "$quarantined_module" | awk '{print $1}')" = "$expected"
depmod -a "$kernel"
if modinfo -k "$kernel" zfs >/dev/null 2>&1; then
  printf 'ZFS remains discoverable after quarantine\n' >&2
  exit 1
fi
REMOTE_QUARANTINE
}

zfs_verify_module_failure() {
  wait_for_core_api
  zfs_verify_preserved_storage_evidence
  core "! zpool list '${pool}' >/dev/null 2>&1 && ! zfs list '${dataset}' >/dev/null 2>&1"
  core "test ! -e '${bind_device}'"
  failure_containers=$(core "docker ps -aq --filter 'label=plz.namespace_id=${zfs_namespace}' --filter 'label=plz.service_id=${zfs_service}'")
  if [ "$(printf '%s\n' "$failure_containers" | sed '/^$/d' | wc -l)" -ne 1 ] \
    || [ "$failure_containers" != "$baseline_db_container" ]; then
    log "failure evidence does not contain exactly the baseline database container"
    return 1
  fi
  for container in $failure_containers; do
    [ "$(core "docker inspect --format '{{.State.Running}}' '${container}'")" = false ] || {
      log "database container ${container} is running without ZFS"; return 1;
    }
    failure_volume_name=$(core "docker inspect --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Name}}{{end}}{{end}}' '${container}'")
    if [ "$container" != "$baseline_db_container" ] || [ "$failure_volume_name" != "$docker_volume_name" ]; then
      log "database failure evidence names a replacement container or volume"
      return 1
    fi
  done
  if start_failure=$(core "docker start '${baseline_db_container}'" 2>&1); then
    log "database container started without its ZFS bind device"; return 1
  fi
  state_error=$(core "docker inspect --format '{{.State.Error}}' '${baseline_db_container}'")
  printf 'docker-start-error:\n%s\ndocker-state-error:\n%s\n' "$start_failure" "$state_error"
  printf '%s\n%s\n' "$start_failure" "$state_error" | grep -Eqi 'mount|bind|no such file|does not exist' || {
    log "Docker start failed without mount/bind evidence"; return 1;
  }
  [ "$(core "docker inspect --format '{{.State.Running}}' '${baseline_db_container}'")" = false ] \
    || { log "database container became running after failed start"; return 1; }
  core "journalctl -b -u docker.service --no-pager -n 100"
  core 'systemctl is-active --quiet ployzd-control && timeout 10s ployz ops list >/dev/null'
  local testimony_deadline=$((SECONDS + 360))
  while (( SECONDS < testimony_deadline )); do
    if failure_testimony=$(core_before_deadline "$testimony_deadline" "timeout 12s ployz machine inspect '${core_machine_id}'") \
      && printf '%s\n' "$failure_testimony" | grep -Fx 'storage unavailable zfs-module-missing' >/dev/null \
      && printf '%s\n' "$failure_testimony" | grep -Fx "storage-alarms ${zfs_namespace}/${zfs_volume}:zfs-module-missing" >/dev/null; then
      printf '%s\n' "$failure_testimony"
      return 0
    fi
    sleep_before_deadline "$testimony_deadline" 3
  done
  printf '%s\n' "$failure_testimony"
  log "typed ZFS module failure and exact stranded pin did not converge"
  return 1
}

zfs_restore_module() {
  local restore_command
  printf -v restore_command 'bash -s -- %q %q %q %q' \
    "$recovery_root/zfs-module-recovery.env" "$module_path" "$module_backup" "$quarantined_module"
  # shellcheck disable=SC2029 # printf %q above makes every remote argument a shell word.
  write_zfs_ssh_restore_program \
    | timeout --foreground --signal=TERM --kill-after=2s 120s \
      ssh "${SSH_OPTS[@]}" "root@${CORE}" "$restore_command"
}

zfs_verify_alarm_cleared() {
  zfs_verify_reboot_recovery "${zfs_namespace}/${zfs_volume}"
}

run_zfs_real_host_certification() {
  local zfs_started
  zfs_started=$(ts)
  zfs_suffix="$(date -u +%m%d%H%M)-$$"
  zfs_namespace="zfs${zfs_suffix//-/}"
  zfs_volume="data${zfs_suffix//-/}"
  zfs_service="db${zfs_suffix//-/}"
  row_marker="ployz-${zfs_suffix}-$(printf '%s' "${CORE}-${EDGE}-${zfs_started}" | sha256sum | cut -c1-12)"
  remote_compose="/tmp/ployz-zfs-${zfs_suffix}.yml"
  core_machine_id=$(core "ployz machine list | awk '\$2 == \"ployz-core\" { print \$1; exit }'")
  [ -n "$core_machine_id" ] || { log "could not resolve the Rocky core machine id"; return 1; }
  install -m 0444 "$REPO_ROOT/scripts/real-host-acceptance.sh" "$ZFS_EVIDENCE_DIR/sealed-harness.sh"
  cat > "$ZFS_EVIDENCE_DIR/commands.log" <<EOF
Harness source: sealed-harness.sh (${harness_sha})
01 timeout 2m ployz machine storage-prepare ${core_machine_id} --detach; timeout 20m ployz ops watch OPERATION --json
02 ployz volume create ${zfs_namespace} ${zfs_volume} --machine ${core_machine_id} --max-size 2G; ployz deploy -f ${remote_compose}
03 zpool/zfs GUID, mountpoint, exact quota=2147483648 and refquota=0; docker inspect container/volume; PostgreSQL row query
04 systemctl reboot; systemd zfs.target-before-docker timestamps; same pool/dataset/quota/refquota/container/volume/bind/row
05 modinfo/lsinitrd guard; root-only copy+metadata+checksum; move module; depmod; modinfo absence
06 systemctl reboot; absent pool/dataset/bind device; stopped same container; Control/API and typed stranded-pin testimony
07 verify backup checksum; restore exact module path; depmod; modinfo
08 systemctl reboot; same pool/dataset/quota/refquota/container/volume/bind/row; alarm clear
09 systemctl reboot; repeat full recovery and alarm-clear proof
Wait budgets use monotonic absolute deadlines: disconnect 120s; reboot return 540s; API/testimony 360s; container 240s; each retry-loop SSH attempt at most 15s.
Commands containing fixture credentials are intentionally redacted; their bounded outcomes are in the numbered logs.
EOF

  zfs_phase 01 storage-prepare zfs_storage_prepare
  zfs_phase 02 database-deploy zfs_deploy_database
  zfs_phase 03 baseline-identity zfs_capture_baseline

  initial_boot_id=$(core 'cat /proc/sys/kernel/random/boot_id')
  reboot_started=$(ts)
  baseline_reboot_id=$(reboot_core "$initial_boot_id")
  printf 'initial_boot_id=%s\nbaseline_reboot_id=%s\nbaseline_reboot_seconds=%s\n' \
    "$initial_boot_id" "$baseline_reboot_id" "$(( $(ts) - reboot_started ))" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  zfs_phase 04 baseline-reboot zfs_verify_reboot_recovery

  zfs_phase 05 module-quarantine zfs_quarantine_module
  reboot_started=$(ts)
  quarantine_boot_id=$(reboot_core "$baseline_reboot_id")
  printf 'quarantine_boot_id=%s\nquarantine_reboot_seconds=%s\n' \
    "$quarantine_boot_id" "$(( $(ts) - reboot_started ))" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  zfs_phase 06 module-failure zfs_verify_module_failure

  zfs_phase 07 module-restore zfs_restore_module
  reboot_started=$(ts)
  restore_boot_id=$(reboot_core "$quarantine_boot_id")
  printf 'restore_boot_id=%s\nrestore_reboot_seconds=%s\n' \
    "$restore_boot_id" "$(( $(ts) - reboot_started ))" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  zfs_phase 08 recovery zfs_verify_alarm_cleared

  reboot_started=$(ts)
  final_boot_id=$(reboot_core "$restore_boot_id")
  printf 'final_boot_id=%s\nfinal_reboot_seconds=%s\n' \
    "$final_boot_id" "$(( $(ts) - reboot_started ))" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  zfs_phase 09 final-reboot zfs_verify_alarm_cleared
  printf '%s' "$ARCHITECTURE_RESULT_METADATA" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  printf 'completed_utc=%s\ntotal_seconds=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$(( $(ts) - zfs_started ))" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  checksum_tmp=$(mktemp)
  (
    cd "$ZFS_EVIDENCE_DIR"
    find . -maxdepth 1 -type f ! -name transcript.log ! -name sha256sums -print0 \
      | sort -z | xargs -0 sha256sum > "$checksum_tmp"
  )
  mv "$checksum_tmp" "$ZFS_EVIDENCE_DIR/sha256sums"
  zfs_recovery_message=
  log "$ZFS_SUCCESS_MARKER"
}

if [ "$ZFS_CERTIFY" = 1 ]; then
  run_zfs_real_host_certification
fi

log "$ACCEPTANCE_SUCCESS_MARKER"
