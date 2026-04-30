#!/usr/bin/env bash
set -euo pipefail

echo "ployz-e2e boot: node=${PLOYZ_E2E_NODE:-unknown} scenario=${PLOYZ_E2E_SCENARIO:-unknown} run_id=${PLOYZ_E2E_RUN_ID:-unknown} image=${PLOYZ_E2E_IMAGE:-unknown} image_id=${PLOYZ_E2E_IMAGE_ID:-unknown}"

runtime="${PLOYZ_E2E_RUNTIME:-host}"
if [[ "${runtime}" != "host" && "${runtime}" != "docker" ]]; then
  echo "unsupported PLOYZ_E2E_RUNTIME=${runtime}" >&2
  exit 1
fi

if [[ -z "${PLOYZ_E2E_SSH_AUTHORIZED_KEY:-}" ]]; then
  echo "missing PLOYZ_E2E_SSH_AUTHORIZED_KEY" >&2
  exit 1
fi

install -d -m 700 /root/.ssh
printf '%s\n' "${PLOYZ_E2E_SSH_AUTHORIZED_KEY}" >/root/.ssh/authorized_keys
chmod 600 /root/.ssh/authorized_keys

install -d -m 755 /run/sshd /var/lib/ployz

stale_process_found=0
for process_name in \
  dockerd \
  containerd \
  docker-proxy \
  ployzd \
  corrosion \
  ployz-dns \
  ployz-gateway \
  sshd
do
  if pgrep -x "${process_name}" >/dev/null 2>&1; then
    stale_process_found=1
    pkill -9 -x "${process_name}" >/dev/null 2>&1 || true
  fi
done
if pgrep -f '^containerd-shim' >/dev/null 2>&1; then
  stale_process_found=1
  pkill -9 -f '^containerd-shim' >/dev/null 2>&1 || true
fi
if [[ "${stale_process_found}" -eq 1 ]]; then
  echo "ployz-e2e cleaned stale processes from previous container start"
  sleep 0.2
fi

rm -f /var/run/docker.sock /run/docker.sock /run/docker.pid
rm -rf /var/run/docker/containerd /run/docker/containerd
docker_storage_driver="${PLOYZ_E2E_DOCKER_STORAGE_DRIVER:-vfs}"
echo "ployz-e2e dockerd storage driver: ${docker_storage_driver}"
/usr/local/bin/e2e-dind.sh \
  dockerd \
  --host=unix:///var/run/docker.sock \
  --storage-driver="${docker_storage_driver}" \
  >/var/log/dockerd.log 2>&1 &

for _ in $(seq 1 100); do
  if docker info >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done

if ! docker info >/dev/null 2>&1; then
  echo "inner dockerd did not become ready" >&2
  cat /var/log/dockerd.log >&2 || true
  exit 1
fi

# Start sshd before the disk-heavy preload loop so wait_for_ssh isn't gated
# on image loading. wait_for_daemon still gates on the ployzd socket below,
# which only appears after preload and install complete.
/usr/sbin/sshd

preload_dir="/opt/ployz-e2e/preloaded-images"
preload_manifest="${preload_dir}/built_in_images.toml"
if [[ -d "${preload_dir}" ]]; then
  echo "ployz-e2e preload images start"
  shopt -s nullglob
  for tar_path in "${preload_dir}"/*.tar; do
    echo "ployz-e2e preload image load: ${tar_path}"
    docker load -i "${tar_path}" >/dev/null
  done
  shopt -u nullglob
  echo "ployz-e2e preload images complete"
fi

if [[ "${runtime}" == "docker" && -f "${preload_manifest}" ]]; then
  export PLOYZ_BUILTIN_IMAGES_MANIFEST="${preload_manifest}"
  echo "ployz-e2e built-in images manifest=${PLOYZ_BUILTIN_IMAGES_MANIFEST}"
elif [[ "${runtime}" != "docker" ]]; then
  echo "ployz-e2e built-in images manifest skipped runtime=${runtime}"
else
  echo "ployz-e2e preload images: no manifest at ${preload_manifest}"
fi

if [[ ! -d /e2e-payload ]]; then
  echo "missing /e2e-payload" >&2
  exit 1
fi

if [[ ! -x /usr/local/bin/ployz.sh ]]; then
  echo "missing /usr/local/bin/ployz.sh" >&2
  exit 1
fi

if [[ -f /e2e-payload/metadata.env ]]; then
  echo "ployz-e2e payload metadata:"
  sed 's/^/  /' /e2e-payload/metadata.env
else
  echo "ployz-e2e payload metadata: missing /e2e-payload/metadata.env"
fi

if [[ -n "${PLOYZ_E2E_ZFS_MODE:-}" ]]; then
  zfs_mode="${PLOYZ_E2E_ZFS_MODE}"
  if [[ "${zfs_mode}" != "fake" && "${zfs_mode}" != "real" ]]; then
    echo "unsupported PLOYZ_E2E_ZFS_MODE=${zfs_mode}" >&2
    exit 1
  fi

  install -d -m 755 /var/lib/ployz-e2e-zfs /root/.config/ployz
  printf '%s\n' "${zfs_mode}" >/var/lib/ployz-e2e-zfs/mode

  if [[ "${zfs_mode}" == "fake" ]]; then
    zfs_root="${PLOYZ_E2E_ZFS_ROOT:-tank/ployz-e2e}"
    zfs_root_mount="/${zfs_root}"
    install -d -m 755 "${zfs_root_mount}"
    printf '%s\n' "${zfs_root}" >/var/lib/ployz-e2e-zfs/root
    printf '%s\n' "${zfs_root%%/*}" >/var/lib/ployz-e2e-zfs/pool.name
    echo "ployz-e2e fake zfs root=${zfs_root}"
  cat >/usr/local/bin/zfs <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

state_dir=/var/lib/ployz-e2e-zfs
calls_file="${state_dir}/calls.log"
datasets_file="${state_dir}/datasets.tsv"
root_file="${state_dir}/root"
mkdir -p "${state_dir}"
printf 'zfs %q' "$1" >>"${calls_file}"
for arg in "$@"; do
  printf ' %q' "${arg}" >>"${calls_file}"
done
printf '\n' >>"${calls_file}"
root="$(cat "${root_file}")"
root_mount="/${root}"

quota_bytes() {
  value="$1"
  case "${value}" in
    none|-)
      printf '0\n'
      ;;
    *[Kk])
      number="${value%[Kk]}"
      printf '%s\n' "$((number * 1024))"
      ;;
    *[Mm])
      number="${value%[Mm]}"
      printf '%s\n' "$((number * 1024 * 1024))"
      ;;
    *[Gg])
      number="${value%[Gg]}"
      printf '%s\n' "$((number * 1024 * 1024 * 1024))"
      ;;
    *[Tt])
      number="${value%[Tt]}"
      printf '%s\n' "$((number * 1024 * 1024 * 1024 * 1024))"
      ;;
    *)
      printf '%s\n' "${value}"
      ;;
  esac
}

if [[ $# -ge 5 && "$1" == "list" && "$2" == "-H" && "$3" == "-o" && "$4" == "mountpoint" ]]; then
  dataset="$5"
  if [[ "${dataset}" == "${root}" ]]; then
    printf '%s\n' "${root_mount}"
    exit 0
  fi
  if [[ -f "${datasets_file}" ]]; then
    mountpoint="$(awk -F '\t' -v dataset="${dataset}" '$1 == dataset { print $3; exit }' "${datasets_file}")"
    if [[ -n "${mountpoint}" ]]; then
      printf '%s\n' "${mountpoint}"
      exit 0
    fi
  fi
  printf 'dataset does not exist\n' >&2
  exit 1
fi

if [[ $# -ge 5 && "$1" == "list" && "$3" == "-o" && "$4" == "name,quota,mountpoint" ]]; then
  if [[ "$2" != "-H" && "$2" != "-Hp" ]]; then
    printf 'unsupported fake zfs command\n' >&2
    exit 2
  fi
  dataset="$5"
  if [[ -f "${datasets_file}" ]]; then
    row="$(awk -F '\t' -v dataset="${dataset}" '$1 == dataset { print; exit }' "${datasets_file}")"
    if [[ -n "${row}" ]]; then
      IFS=$'\t' read -r name quota mountpoint <<<"${row}"
      if [[ "$2" == "-Hp" ]]; then
        quota="$(quota_bytes "${quota}")"
      fi
      printf '%s\t%s\t%s\n' "${name}" "${quota}" "${mountpoint}"
      exit 0
    fi
  fi
  printf 'dataset does not exist\n' >&2
  exit 1
fi

if [[ $# -ge 6 && "$1" == "list" && "$2" == "-Hp" && "$3" == "-r" && "$4" == "-o" && "$5" == "name,quota" ]]; then
  printf '%s\t0\n' "${root}"
  if [[ -f "${datasets_file}" ]]; then
    while IFS=$'\t' read -r dataset quota _mountpoint; do
      printf '%s\t%s\n' "${dataset}" "$(quota_bytes "${quota}")"
    done <"${datasets_file}"
  fi
  exit 0
fi

if [[ $# -ge 1 && "$1" == "create" ]]; then
  quota=
  mountpoint=
  dataset="${@: -1}"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      -o)
        opt="$2"
        shift 2
        case "${opt}" in
          quota=*) quota="${opt#quota=}" ;;
          mountpoint=*) mountpoint="${opt#mountpoint=}" ;;
        esac
        ;;
      *) shift ;;
    esac
  done
  if [[ -z "${quota}" || -z "${mountpoint}" ]]; then
    printf 'missing quota or mountpoint\n' >&2
    exit 1
  fi
  mkdir -p "${mountpoint}"
  tmp="$(mktemp)"
  if [[ -f "${datasets_file}" ]]; then
    awk -F '\t' -v dataset="${dataset}" '$1 != dataset { print }' "${datasets_file}" >"${tmp}"
  fi
  printf '%s\t%s\t%s\n' "${dataset}" "${quota}" "${mountpoint}" >>"${tmp}"
  mv "${tmp}" "${datasets_file}"
  exit 0
fi

if [[ $# -ge 3 && "$1" == "set" ]]; then
  opt="$2"
  dataset="$3"
  case "${opt}" in
    quota=*)
      quota="${opt#quota=}"
      if [[ ! -f "${datasets_file}" ]]; then
        printf 'dataset does not exist\n' >&2
        exit 1
      fi
      row="$(awk -F '\t' -v dataset="${dataset}" '$1 == dataset { print; exit }' "${datasets_file}")"
      if [[ -z "${row}" ]]; then
        printf 'dataset does not exist\n' >&2
        exit 1
      fi
      IFS=$'\t' read -r _name _old_quota mountpoint <<<"${row}"
      tmp="$(mktemp)"
      awk -F '\t' -v dataset="${dataset}" '$1 != dataset { print }' "${datasets_file}" >"${tmp}"
      printf '%s\t%s\t%s\n' "${dataset}" "${quota}" "${mountpoint}" >>"${tmp}"
      mv "${tmp}" "${datasets_file}"
      exit 0
      ;;
    *)
      exit 0
      ;;
  esac
fi

if [[ $# -ge 6 && "$1" == "get" && "$4" == "value" && "$5" == "quota" ]]; then
  dataset="$6"
  if [[ -f "${datasets_file}" ]]; then
    quota="$(awk -F '\t' -v dataset="${dataset}" '$1 == dataset { print $2; exit }' "${datasets_file}")"
    if [[ -n "${quota}" ]]; then
      printf '%s\n' "${quota}"
      exit 0
    fi
  fi
  printf 'dataset does not exist\n' >&2
  exit 1
fi

if [[ $# -ge 6 && "$1" == "get" && "$4" == "value" && "$5" == "used" ]]; then
  printf '0\n'
  exit 0
fi

printf 'unsupported fake zfs command\n' >&2
exit 2
EOF
    chmod 0755 /usr/local/bin/zfs
  cat >/usr/local/bin/zpool <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

state_dir=/var/lib/ployz-e2e-zfs
calls_file="${state_dir}/calls.log"
root="$(cat "${state_dir}/root")"
pool="${root%%/*}"
printf 'zpool %q' "$1" >>"${calls_file}"
for arg in "$@"; do
  printf ' %q' "${arg}" >>"${calls_file}"
done
printf '\n' >>"${calls_file}"

if [[ $# -ge 5 && "$1" == "list" && "$2" == "-Hp" && "$3" == "-o" && "$4" == "size" && "$5" == "${pool}" ]]; then
  printf '10737418240\n'
  exit 0
fi

if [[ $# -ge 2 && "$1" == "destroy" && "$2" == "${pool}" ]]; then
  exit 0
fi

printf 'unsupported fake zpool command\n' >&2
exit 2
EOF
    chmod 0755 /usr/local/bin/zpool
  else
    if [[ "$(uname -s)" != "Linux" ]]; then
      echo "real ZFS e2e requires a Linux host/container" >&2
      exit 1
    fi
    if [[ ! -e /dev/zfs ]]; then
      echo "real ZFS e2e requires /dev/zfs to be visible in the node container" >&2
      exit 1
    fi
    if ! command -v zfs >/dev/null 2>&1 || ! command -v zpool >/dev/null 2>&1; then
      echo "real ZFS e2e requires zfs and zpool commands in the node image" >&2
      exit 1
    fi

    raw_pool="${PLOYZ_E2E_ZFS_POOL:-ployze2e_${PLOYZ_E2E_NODE:-node}_${PLOYZ_E2E_RUN_ID:-manual}}"
    pool="$(printf '%s' "${raw_pool}" | tr -c 'A-Za-z0-9_' '_' | cut -c1-60)"
    if [[ -z "${pool}" || "${pool}" != ployze2e_* ]]; then
      pool="ployze2e_${pool}"
      pool="$(printf '%s' "${pool}" | cut -c1-60)"
    fi
    zfs_root="${pool}/ployz-e2e"
    pool_img=/var/lib/ployz-e2e-zfs/pool.img
    pool_size="${PLOYZ_E2E_ZFS_POOL_SIZE:-2G}"

    if zpool list -H "${pool}" >/dev/null 2>&1; then
      echo "real ZFS e2e pool already exists: ${pool}" >&2
      exit 1
    fi

    loop_file=/var/lib/ployz-e2e-zfs/pool.loop

    echo "ployz-e2e real zfs pool=${pool} root=${zfs_root} image=${pool_img} size=${pool_size}"
    truncate -s "${pool_size}" "${pool_img}"
    if ! loopdev="$(losetup --find --show "${pool_img}")"; then
      rm -f "${pool_img}"
      exit 1
    fi
    printf '%s\n' "${loopdev}" >"${loop_file}"
    echo "ployz-e2e real zfs loop=${loopdev}"
    if ! zpool create -f "${pool}" "${loopdev}"; then
      losetup -d "${loopdev}" >/dev/null 2>&1 || true
      rm -f "${pool_img}"
      rm -f "${loop_file}"
      exit 1
    fi
    if ! zfs create -p "${zfs_root}"; then
      zpool destroy "${pool}" >/dev/null 2>&1 || true
      losetup -d "${loopdev}" >/dev/null 2>&1 || true
      rm -f "${pool_img}"
      rm -f "${loop_file}"
      exit 1
    fi
    printf '%s\n' "${pool}" >/var/lib/ployz-e2e-zfs/pool.name
    printf '%s\n' "${zfs_root}" >/var/lib/ployz-e2e-zfs/root
  fi

  cat >/root/.config/ployz/config.toml <<EOF
[storage]
zfs_root = "${zfs_root}"
EOF
fi

HOME=/root /usr/local/bin/ployz.sh install --source payload --payload-dir /e2e-payload --runtime "${runtime}" --service-mode user --no-daemon-install

ln -sf /root/.local/bin/ployzctl /usr/local/bin/ployzctl
ln -sf /root/.local/bin/ployzd /usr/local/bin/ployzd
ln -sf /root/.local/bin/ployz-gateway /usr/local/bin/ployz-gateway
ln -sf /root/.local/bin/ployz-dns /usr/local/bin/ployz-dns
ln -sf /root/.local/bin/corrosion /usr/local/bin/corrosion

for binary in /root/.local/bin/ployzctl /root/.local/bin/ployzd /root/.local/bin/corrosion; do
  if [[ -x "${binary}" ]]; then
    sha256="$(sha256sum "${binary}" | awk '{print $1}')"
    echo "ployz-e2e binary: path=${binary} sha256=${sha256}"
    "${binary}" --version || true
  else
    echo "ployz-e2e binary missing: path=${binary}"
  fi
done

exec /root/.local/bin/ployzd --data-dir /var/lib/ployz run --runtime "${runtime}" --service-mode user
