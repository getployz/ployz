#!/usr/bin/env bash
set -euo pipefail

echo "ployz-e2e boot: node=${PLOYZ_E2E_NODE:-unknown} scenario=${PLOYZ_E2E_SCENARIO:-unknown} run_id=${PLOYZ_E2E_RUN_ID:-unknown} image=${PLOYZ_E2E_IMAGE:-unknown} image_id=${PLOYZ_E2E_IMAGE_ID:-unknown}"

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

if [[ -f /e2e-payload/metadata.env ]]; then
  echo "ployz-e2e payload metadata:"
  sed 's/^/  /' /e2e-payload/metadata.env
else
  echo "ployz-e2e payload metadata: missing /e2e-payload/metadata.env"
fi

HOME=/root /usr/local/bin/ployz.sh install --source payload --payload-dir /e2e-payload --mode host-exec --no-daemon-install

ln -sf /root/.local/bin/ployz /usr/local/bin/ployz
ln -sf /root/.local/bin/ployzd /usr/local/bin/ployzd
ln -sf /root/.local/bin/ployz-gateway /usr/local/bin/ployz-gateway
ln -sf /root/.local/bin/ployz-dns /usr/local/bin/ployz-dns
ln -sf /root/.local/bin/corrosion /usr/local/bin/corrosion

for binary in /root/.local/bin/ployz /root/.local/bin/ployzd /root/.local/bin/corrosion; do
  if [[ -x "${binary}" ]]; then
    sha256="$(sha256sum "${binary}" | awk '{print $1}')"
    echo "ployz-e2e binary: path=${binary} sha256=${sha256}"
    "${binary}" --version || true
  else
    echo "ployz-e2e binary missing: path=${binary}"
  fi
done

exec /root/.local/bin/ployzd --data-dir /var/lib/ployz run --mode host-exec
