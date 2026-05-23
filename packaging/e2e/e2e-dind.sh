#!/usr/bin/env bash
set -euo pipefail

export container=docker

if command -v mountpoint >/dev/null 2>&1; then
  if [[ -d /sys/kernel/security ]] && ! mountpoint -q /sys/kernel/security; then
    mount -t securityfs none /sys/kernel/security || true
  fi

  if ! mountpoint -q /tmp; then
    mount -t tmpfs none /tmp || true
  fi
fi

if [[ -f /sys/fs/cgroup/cgroup.controllers ]]; then
  mkdir -p /sys/fs/cgroup/init
  xargs -rn1 </sys/fs/cgroup/cgroup.procs >/sys/fs/cgroup/init/cgroup.procs || :
  timeout 5s sh -c "until sed -e 's/ / +/g' -e 's/^/+/' < /sys/fs/cgroup/cgroup.controllers > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null; do sleep 0.1; done"
fi

mount --make-rshared / || true

exec "$@"
