#!/usr/bin/env bash
set -euo pipefail

# Boot IMAGE with PLOYZ_TESTKIT_PRIME=1 so entrypoint loads nested images,
# stops dockerd, writes /opt/ployz/docker-graph.tar, and exits without
# starting ployzd. Commit the tar onto IMAGE. Later Machines extract it onto
# the /var/lib/docker volume so nested overlay is one filesystem.
IMAGE=${PLOYZ_TESTKIT_IMAGE:-${IMAGE:?set IMAGE or PLOYZ_TESTKIT_IMAGE}}

container=
trap 'test -z "${container}" || docker rm --force --volumes "${container}" >/dev/null 2>&1 || true' EXIT

container=$(docker run --detach --privileged --env PLOYZ_TESTKIT_PRIME=1 "${IMAGE}")
status=$(timeout 180 docker wait "${container}")
test "${status}" = 0
docker commit --change 'ENV PLOYZ_TESTKIT_PRIME=0' "${container}" "${IMAGE}" >/dev/null
