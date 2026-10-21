#!/usr/bin/env bash
set -euo pipefail

# Boot SOURCE with PLOYZ_TESTKIT_PRIME=1 so entrypoint loads nested images,
# stops dockerd, writes /opt/ployz/docker-graph.tar, and exits without
# starting ployzd. Commit that container as TARGET, labelled with SOURCE's
# image id. Later Machines extract the tar onto the /var/lib/docker volume so
# nested overlay is one filesystem, and skip a docker load per image at boot.
source=${1:?usage: scripts/prime-testkit-image.sh SOURCE TARGET}
target=${2:?usage: scripts/prime-testkit-image.sh SOURCE TARGET}
source_id=$(docker image inspect --format '{{.Id}}' "$source")

container=
trap 'test -z "${container}" || docker rm --force --volumes "${container}" >/dev/null 2>&1 || true' EXIT

container=$(docker run --detach --privileged --env PLOYZ_TESTKIT_PRIME=1 "$source_id")
status=$(timeout 180 docker wait "${container}")
test "${status}" = 0
docker commit --change 'ENV PLOYZ_TESTKIT_PRIME=0' --change "LABEL dev.ployz.testkit.primed-from=$source_id" "${container}" "$target" >/dev/null
