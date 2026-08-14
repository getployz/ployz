#!/bin/sh
set -eu

rm -f /var/run/docker.pid /var/run/docker.sock
if [ "${PLOYZ_TESTKIT_CONTAINERD_STORE:-1}" = 0 ]; then
  cp /etc/docker/daemon-overlay.json /etc/docker/daemon.json
fi
mkdir -p /run/sshd /root/.ssh
if [ ! -f /root/.ssh/id_ed25519 ]; then
  ssh-keygen -q -t ed25519 -N '' -f /root/.ssh/id_ed25519
  cp /root/.ssh/id_ed25519.pub /root/.ssh/authorized_keys
  chmod 600 /root/.ssh/authorized_keys
fi
ssh-keygen -A
/usr/sbin/sshd
# ponytail: Corrosion uses host networking; enable a bridge when workload tests need one.
dockerd --bridge=none --iptables=false --ip6tables=false >/var/log/dockerd.log 2>&1 &
deadline=60
until docker info >/dev/null 2>&1; do
  deadline=$((deadline - 1))
  [ "$deadline" -gt 0 ] || { cat /var/log/dockerd.log >&2; exit 1; }
  sleep 1
done
if iptables -t filter -L >/dev/null 2>&1 \
    && ! iptables -t filter -L DOCKER-USER >/dev/null 2>&1; then
  iptables -t filter -N DOCKER-USER
fi

docker load --input /opt/ployz/images/corrosion.tar >/dev/null
docker load --input /opt/ployz/images/alpine.tar >/dev/null
docker load --input /opt/ployz/images/caddy.tar >/dev/null
if [ -n "${PLOYZ_TESTKIT_HOST_API_PORT:-}" ]; then
  socat TCP-LISTEN:"$PLOYZ_TESTKIT_HOST_API_PORT",bind=0.0.0.0,reuseaddr,fork UNIX-CONNECT:/run/ployz/ployz.sock &
fi
docker load --input /opt/ployz/images/unregistry.tar >/dev/null
while :; do
  ployzd --machine-api-address '[::]:51000' "$@" &
  ployzd_pid=$!
  echo "$ployzd_pid" >/run/ployzd.pid
  wait "$ployzd_pid" || exit $?
done
