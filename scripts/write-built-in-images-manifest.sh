#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 OUTPUT_PATH [networking=REF] [corrosion=REF] [nats=REF] [dns=REF] [gateway=REF]" >&2
  exit 1
fi

output_path="$1"
shift

networking=''
corrosion=''
nats=''
dns=''
gateway=''

for assignment in "$@"; do
  case "$assignment" in
    networking=*)
      networking="${assignment#networking=}"
      ;;
    corrosion=*)
      corrosion="${assignment#corrosion=}"
      ;;
    nats=*)
      nats="${assignment#nats=}"
      ;;
    dns=*)
      dns="${assignment#dns=}"
      ;;
    gateway=*)
      gateway="${assignment#gateway=}"
      ;;
    *)
      echo "unsupported manifest override '${assignment}'" >&2
      exit 1
      ;;
  esac
done

mkdir -p "$(dirname "$output_path")"

{
  if [[ -n "$networking" ]]; then
    printf 'networking = "%s"\n' "$networking"
  fi
  if [[ -n "$corrosion" ]]; then
    printf 'corrosion = "%s"\n' "$corrosion"
  fi
  if [[ -n "$nats" ]]; then
    printf 'nats = "%s"\n' "$nats"
  fi
  if [[ -n "$dns" ]]; then
    printf 'dns = "%s"\n' "$dns"
  fi
  if [[ -n "$gateway" ]]; then
    printf 'gateway = "%s"\n' "$gateway"
  fi
} >"$output_path"

printf '%s\n' "$output_path"
