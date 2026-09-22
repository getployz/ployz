#!/usr/bin/env bash
set -euo pipefail

: "${GH_TOKEN:?GH_TOKEN is required}"
: "${RAILWAY_TOKEN:?RAILWAY_TOKEN is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${GITHUB_ACTOR:?GITHUB_ACTOR is required}"

# Both caller and Cloud workflow cancel superseded runs. Check main again at the
# external write boundaries, since an older build may finish as a new push arrives.
require_current() {
  local head
  head=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/heads/main" --jq .object.sha)
  if [ "$head" != "$GITHUB_SHA" ]; then
    echo "Superseded by a newer main commit; skipping remaining publication/deployment."
    exit 0
  fi
}
require_current

image="ghcr.io/${GITHUB_REPOSITORY,,}-cloud:main"
printf '%s' "$GH_TOKEN" | docker login ghcr.io -u "$GITHUB_ACTOR" --password-stdin
docker tag ployz-cloud:ci "$image"
docker push "$image"

require_current
npm install --global @railway/cli@5.45.6
# --from-source pulls the new :main image; plain redeploy can reuse the old one.
railway redeploy --from-source --yes --service 8089d161-49d3-4c77-b683-2bede5a784f5 &
web_pid=$!
railway redeploy --from-source --yes --service 1fd9b511-86d8-4497-87ba-5ebad34849d7 &
worker_pid=$!
status=0
wait "$web_pid" || status=1
wait "$worker_pid" || status=1
exit "$status"
