#!/usr/bin/env bash
# Rung 4. Run ONLY on a disposable systemd Machine with an installed local release.
# Usage: sudo scripts/test-tailcat-systemd.sh /path/to/local/release VERSION
# Exercises the shared installer and destructive uninstaller; no release authority claim.
set -euo pipefail
release_dir=$(realpath "${1:?local release directory}")
version=${2:?release version}
[[ $EUID == 0 && -d /run/systemd/system ]]
[[ -x /usr/local/bin/ployzd && -x /usr/local/bin/ployzd-tailcat ]]
state=/var/lib/ployz/tailcat/state.json
systemctl is-active --quiet ployz.service ployz-tailcat.service
helper_pid=$(systemctl show --property=MainPID --value ployz-tailcat.service)
listeners=$(ss -H -ltnp)
[[ "$listeners" != *"pid=$helper_pid,"* ]]
[[ $(stat -c %a "$state") == 600 && $(stat -c %a "$(dirname "$state")") == 700 ]]
# A separate Docker workload must survive endpoint failure and software replacement.
workload=$(docker run -d --label tailcat-qualification alpine:3.22 sleep 600)
trap 'docker rm -f "$workload" >/dev/null 2>&1 || true' EXIT
# Test write confinement from the actual helper's mount namespace and service UID.
# Unix DAC alone is insufficient because the service user owns the data parent.
helper_pid=$(systemctl show --property=MainPID --value ployz-tailcat.service)
nsenter --target "$helper_pid" --mount -- setpriv --reuid=ployz --regid=ployz --init-groups \
    python3 - <<'PYTHON'
from pathlib import Path
private = Path('/var/lib/ployz/tailcat/qualification-write')
private.write_text('allowed')
private.unlink()
try:
    Path('/var/lib/ployz/qualification-write').write_text('forbidden')
except OSError:
    pass
else:
    raise AssertionError('helper can replace entries in Machine data root')
PYTHON
identity=$(sha256sum "$state" | cut -d' ' -f1)
links=$(ip -j link show type wireguard)
# Reproduce an interrupted activation: disk has the exact version, but the active
# process still runs the replaced inode. Published retry must not download or stall.
cp -p /usr/local/bin/ployzd-tailcat /usr/local/bin/ployzd-tailcat.replacement
mv /usr/local/bin/ployzd-tailcat.replacement /usr/local/bin/ployzd-tailcat
/usr/local/bin/ployzd install --software-only --version "$version"
[[ $(sha256sum "$state" | cut -d' ' -f1) == "$identity" ]]
systemctl kill --kill-whom=main --signal=SIGKILL ployz-tailcat.service
timeout 40 bash -c 'until systemctl is-active --quiet ployz-tailcat.service; do sleep 1; done'
[[ $(sha256sum "$state" | cut -d' ' -f1) == "$identity" ]]
[[ $(docker inspect -f '{{.State.Running}}' "$workload") == true ]]
[[ $(ip -j link show type wireguard) == "$links" ]]
# Replace the actual daemon socket with a bounded echo fixture while the daemon is stopped.
# Every connection redials its Unix destination, including after socket inode recreation.
systemctl stop ployz.service
python3 - <<'PY'
import grp, json, os, socket, subprocess, threading
path = '/run/ployz/ployz.sock'
with open('/var/lib/ployz/tailcat/state.json') as f:
    capability = json.load(f)['capability'].encode()
for turn in range(2):
    if os.path.exists(path): os.unlink(path)
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(path)
    os.chown(path, 0, grp.getgrnam("ployz").gr_gid)
    os.chmod(path, 0o660)
    listener.listen(1)
    listener.settimeout(25)
    errors = []
    def echo():
        try:
            conn, _ = listener.accept()
            with conn:
                conn.settimeout(20)
                data = b''
                while chunk := conn.recv(4096): data += chunk
                conn.sendall(data)
        except Exception as e: errors.append(e)
    worker = threading.Thread(target=echo)
    worker.start()
    result = subprocess.run(['/usr/local/bin/ployzd-tailcat', 'connect'],
        input=capability+b'\n'+b'endpoint-socket-recreation', capture_output=True, timeout=30)
    worker.join(30)
    listener.close()
    assert not errors and not worker.is_alive(), 'Unix forwarding did not finish'
    assert result.returncode == 0 and result.stdout == b'endpoint-socket-recreation', 'Tailcat stream failed'
os.unlink(path)
PY
systemctl start ployz.service
# The same-version local artifact replacement exercises exact preflight and service restart.
/usr/local/bin/ployzd install --software-only --version "$version" --release-dir "$release_dir"
[[ $(sha256sum "$state" | cut -d' ' -f1) == "$identity" ]]
[[ $(docker inspect -f '{{.State.Running}}' "$workload") == true ]]
[[ $(ip -j link show type wireguard) == "$links" ]]
# Corrupt existing state must fail startup without replacing the identity.
systemctl stop ployz-tailcat.service
cp -p "$state" "${state}.qualification"
printf 'invalid-state' > "$state"
if systemctl start ployz-tailcat.service; then
    echo 'corrupt endpoint state was accepted' >&2; exit 1
fi
[[ $(cat "$state") == invalid-state ]]
systemctl stop ployz-tailcat.service
mv "${state}.qualification" "$state"
systemctl reset-failed ployz-tailcat.service
systemctl start ployz-tailcat.service
[[ $(sha256sum "$state" | cut -d' ' -f1) == "$identity" ]]
# Removal uses the shipped uninstaller, never an alternate lifecycle owner.
PLOYZ_AUTO_CONFIRM=true /usr/local/bin/ployz-uninstall
[[ ! -e /usr/local/bin/ployzd-tailcat && ! -e /etc/systemd/system/ployz-tailcat.service && ! -e "$state" ]]
if systemctl is-active --quiet ployz-tailcat.service; then exit 1; fi
printf 'Tailcat systemd lifecycle passed (Rung 4)\n'
