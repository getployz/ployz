#!/bin/sh
# Hourly site rebuild (Railway's site-rebuild cron). The site fetches docs from getployz/ployz and publishes
# articles whose date has come at build time, so an unchanged commit is rebuilt from the source snapshot in
# /root/ployz/site-rebuild/src. ployz-site is redeployed only when the built pages changed: every Uncloud deploy
# reloads Caddy, which closes websockets for every project on np1.
set -eu
dir=/root/ployz
commit=$(cut -c1-7 "$dir/site-rebuild/src.commit")
tag="$commit-$(date -u +%Y%m%d%H%M)"
log() { echo "$(date -u +%FT%TZ) $*"; }

docker build -q --no-cache --build-arg PUBLIC_POSTHOG_KEY --build-arg PUBLIC_POSTHOG_HOST \
  -t "ployz/site:$tag" "$dir/site-rebuild/src" >/dev/null

pages() { docker run --rm --entrypoint sh "$1" -c 'cd /srv && find . -type f | sort | xargs sha256sum | sha256sum' | cut -c1-16; }
current=$(docker ps --filter label=uncloud.service.name=ployz-site --format '{{.Image}}' | head -1)
if [ -n "$current" ] && [ "$(pages "ployz/site:$tag")" = "$(pages "$current")" ]; then
  log "pages unchanged; keeping $current"
  docker rmi "ployz/site:$tag" >/dev/null
  exit 0
fi

# compose.prod.yaml reads ployz-site's tag from SITE_TAG in /root/ployz/.env.
printf 'SITE_TAG=%s\n' "$tag" > "$dir/.env.tmp" && mv "$dir/.env.tmp" "$dir/.env"
cd "$dir" && uc --connect unix:///run/uncloud/uncloud.sock deploy -f compose.prod.yaml -y ployz-site
log "deployed ployz/site:$tag"

# Keep the three newest rebuilt images; an image still in use is not removed.
docker images ployz/site --format '{{.Tag}}' | grep -E '^[0-9a-f]{7}-[0-9]{12}$' | sort -r | tail -n +4 \
  | while read -r old; do docker rmi "ployz/site:$old" >/dev/null 2>&1 || true; done
