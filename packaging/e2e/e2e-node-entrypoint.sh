#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${PLOYZ_E2E_SSH_AUTHORIZED_KEY:-}" ]]; then
  echo "missing PLOYZ_E2E_SSH_AUTHORIZED_KEY" >&2
  exit 1
fi

install -d -m 700 /root/.ssh
printf '%s\n' "${PLOYZ_E2E_SSH_AUTHORIZED_KEY}" >/root/.ssh/authorized_keys
chmod 600 /root/.ssh/authorized_keys

install -d -m 755 /run/sshd /var/lib/ployz
rm -f /var/run/docker.sock /run/docker.sock /run/docker.pid
/usr/local/bin/e2e-dind.sh dockerd --host=unix:///var/run/docker.sock >/var/log/dockerd.log 2>&1 &

for _ in $(seq 1 100); do
  if docker info >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done

if ! docker info >/dev/null 2>&1; then
  echo "inner dockerd did not become ready" >&2
  cat /var/log/dockerd.log >&2 || true
  exit 1
fi

/usr/sbin/sshd

if [[ ! -d /e2e-payload ]]; then
  echo "missing /e2e-payload" >&2
  exit 1
fi

if [[ ! -x /usr/local/bin/ployz.sh ]]; then
  echo "missing /usr/local/bin/ployz.sh" >&2
  exit 1
fi

HOME=/root /usr/local/bin/ployz.sh install --source payload --payload-dir /e2e-payload --mode host-exec --no-daemon-install

ln -sf /root/.local/bin/ployz /usr/local/bin/ployz
ln -sf /root/.local/bin/ployzd /usr/local/bin/ployzd
ln -sf /root/.local/bin/ployz-gateway /usr/local/bin/ployz-gateway
ln -sf /root/.local/bin/ployz-dns /usr/local/bin/ployz-dns
ln -sf /root/.local/bin/corrosion /usr/local/bin/corrosion

exec /root/.local/bin/ployzd --data-dir /var/lib/ployz run --mode host-exec
