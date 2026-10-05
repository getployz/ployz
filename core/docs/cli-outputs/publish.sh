#!/usr/bin/env bash
# publish.sh DIR
# Copies a finished driver run over the tracked captures. DIR must hold the
# .ok marker a driver writes as its last step; the marker names the capture
# group (store, cloud, cluster or cloud-cluster). Every entry in DIR must be a
# regular file, and every capture must name a file already tracked in that
# group. A copy of DIR is scrubbed with scrub.sh, then each file lands through
# a temp file in the group directory and mv, so a destination symlink is
# replaced, never written through. DIR keeps the raw captures.
set -euo pipefail
shopt -s inherit_errexit
src=${1:?usage: publish.sh DIR}
docs=$(cd -- "$(dirname -- "$0")" && pwd) || exit 1
refuse() { echo "publish: $*; nothing published" >&2; exit 1; }

if [ ! -f "$src/.ok" ] || [ -L "$src/.ok" ]; then
  refuse "$src/.ok is missing or not a regular file, so that run did not finish"
fi
group=$(cat -- "$src/.ok")
case $group in
  store | cloud | cluster | cloud-cluster) ;;
  *) refuse "$src/.ok names an unknown capture group: $group" ;;
esac

tracked=$(git -C "$docs" ls-files -- "$group") || exit 1
names=()
shopt -s nullglob dotglob
for path in "$src"/*; do
  name=${path##*/}
  if [ ! -f "$path" ] || [ -L "$path" ]; then
    refuse "$path is not a regular file"
  fi
  [ "$name" = .ok ] && continue
  if ! grep -qxF -- "$group/$name" <<<"$tracked"; then
    refuse "$name is not a tracked capture in $group/"
  fi
  names+=("$name")
done
[ "${#names[@]}" != 0 ] || refuse "$src has no captures"

scrubbed=$(mktemp -d) || exit 1
landing=
trap 'rm -rf -- "$scrubbed"; [ -z "$landing" ] || rm -f -- "$landing"' EXIT
for name in "${names[@]}"; do
  cp -- "$src/$name" "$scrubbed/$name"
done
"$docs/scrub.sh" "${names[@]/#/$scrubbed/}"
for name in "${names[@]}"; do
  landing=$(mktemp "$docs/$group/.publish.XXXXXX") || exit 1
  cp -- "$scrubbed/$name" "$landing"
  chmod 644 -- "$landing"
  mv -f -- "$landing" "$docs/$group/$name"
  landing=
done
echo "published ${#names[@]} files from $src to $docs/$group"
