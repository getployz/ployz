#!/bin/sh
set -eu

rm -f /var/run/docker.pid /var/run/docker.sock
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
while :; do
  ployzd --machine-api-address 0.0.0.0:51000 "$@" &
  ployzd_pid=$!
  echo "$ployzd_pid" >/run/ployzd.pid
  wait "$ployzd_pid" || exit $?
done
