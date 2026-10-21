#!/usr/bin/env bash
set -euo pipefail

# Boot SOURCE with PLOYZ_TESTKIT_PRIME=1 so entrypoint loads nested images,
# stops dockerd, writes /opt/ployz/docker-graph.tar, and exits without
# starting ployzd. Commit that container as TARGET, labelled with SOURCE's
# image id, and print the committed image's id. Later Machines extract the tar
# onto the /var/lib/docker volume so nested overlay is one filesystem, and skip
# a docker load per image at boot.
source=${1:?usage: scripts/prime-testkit-image.sh SOURCE TARGET}
target=${2:?usage: scripts/prime-testkit-image.sh SOURCE TARGET}
source_id=$(docker image inspect --format '{{.Id}}' "$source")

container=ployz-testkit-prime-$(od -An -N4 -tx4 /dev/urandom | tr -d ' ')
trap 'docker rm --force --volumes "$container" >/dev/null 2>&1 || true' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

docker run --detach --name "$container" --privileged --env PLOYZ_TESTKIT_PRIME=1 "$source_id" >/dev/null
# Polled rather than docker wait, so a TERM is handled within a second.
for _ in $(seq 180); do
    [ "$(docker inspect --format '{{.State.Running}}' "$container")" = false ] && break
    sleep 1
done
test "$(docker inspect --format '{{.State.Running}} {{.State.ExitCode}}' "$container")" = "false 0"
docker commit --change 'ENV PLOYZ_TESTKIT_PRIME=0' --change "LABEL dev.ployz.testkit.primed-from=$source_id" "$container" "$target"
