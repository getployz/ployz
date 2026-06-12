#!/usr/bin/env bash
set -euo pipefail

# Sweeps every Docker resource the DinD e2e harness ever created, by the
# constant label the harness puts on all of them (containers, networks,
# volumes). Safe to run any time; it only touches labeled resources.

LABEL="dev.ployz.dind.managed=true"

docker ps -aq --filter "label=${LABEL}" | xargs -r docker rm -fv
docker network ls -q --filter "label=${LABEL}" | xargs -r docker network rm
docker volume ls -q --filter "label=${LABEL}" | xargs -r docker volume rm -f
