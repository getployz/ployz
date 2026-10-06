//! Fake ZFS command adapter for volume-plugin route tests.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// One ordinary writable Pool for plugin tests that do not exercise Pool growth.
pub(super) const USABLE_POOL: &str = "tank\t4294967296\t0\t4294967296\tONLINE\toff\n";

/// The `slot` marker's mirror slot bound: 3 GiB of the 4 GiB [`USABLE_POOL`].
pub(super) const SLOT_BOUND_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// Creates fake `zpool` and `zfs` programs backed by marker files in `directory`.
pub(super) fn fake_zfs(directory: &Path, pools: &str) -> (PathBuf, PathBuf) {
    let script = directory.join("fake-zfs");
    let commands = directory.join("commands");
    let root = directory.join("root");
    let readonly_root = directory.join("readonly-root");
    let incompatible_root = directory.join("incompatible-root");
    let volume = directory.join("volume");
    let readonly_volume = directory.join("readonly-volume");
    let incompatible_volume = directory.join("incompatible-volume");
    let unbounded_volume = directory.join("unbounded-volume");
    let descendant = directory.join("descendant");
    let sibling = directory.join("sibling");
    let mounted = directory.join("mounted");
    let destroy_fails = directory.join("destroy-fails");
    let list_fails = directory.join("list-fails");
    let slot = directory.join("slot");
    let props = directory.join("props");
    let script_body = format!(
        r#"#!/bin/sh
set -eu
name=${{0##*/}}
printf '%s %s\n' "$name" "$*" >> '{commands}'
if [ "$name" = zpool ]; then
  printf '{pools}'
  exit 0
fi
# ZFS refuses a user property (a name with a colon) holding anything but a-z, 0-9 and :._-
check_property() {{
  case "$1" in *:*) case "$1" in *[!abcdefghijklmnopqrstuvwxyz0123456789:._-]*)
    echo "invalid property '$1'" >&2; exit 1 ;;
  esac ;; esac
}}
case "$*" in
  'list -Hp -o name,refquota,used,usedbydataset,mountpoint,mounted,readonly -r tank')
    if [ -e '{list_fails}' ]; then echo 'pool is busy' >&2; exit 1; fi
    printf 'tank\t0\t0\t0\t/tank\tyes\toff\n'
    if [ -e '{root}' ]; then
      if [ -e '{incompatible_root}' ]; then root_mountpoint=/tank/ployz; else root_mountpoint=/var/lib/ployz-volumes; fi
      if [ -e '{readonly_root}' ]; then root_readonly=on; else root_readonly=off; fi
      printf 'tank/ployz\t0\t0\t0\t%s\tno\t%s\n' "$root_mountpoint" "$root_readonly"
    fi
    if [ -e '{volume}' ]; then
      if [ -e '{mounted}' ]; then state=yes; else state=no; fi
      if [ -e '{incompatible_volume}' ]; then volume_mountpoint=/srv/data; else volume_mountpoint=/var/lib/ployz-volumes/data; fi
      if [ -e '{unbounded_volume}' ]; then refquota=none; else refquota=1073741824; fi
      if [ -e '{readonly_volume}' ]; then volume_readonly=on; else volume_readonly=off; fi
      printf 'tank/ployz/data\t%s\t966367642\t966367642\t%s\t%s\t%s\n' "$refquota" "$volume_mountpoint" "$state" "$volume_readonly"
    fi
    if [ -e '{descendant}' ]; then
      printf 'tank/ployz/data/child\t536870912\t0\t0\t/var/lib/ployz-volumes/data/child\tno\toff\n'
    fi
    if [ -e '{sibling}' ]; then
      printf 'tank/ployz/sibling\t1073741824\t0\t0\t/var/lib/ployz-volumes/sibling\tno\toff\n'
    fi
    if [ -e '{slot}' ]; then
      printf 'tank/ployz-mirror\t0\t0\t0\t/var/lib/ployz-mirror\tno\ton\n'
      printf 'tank/ployz-mirror/copy\t0\t0\t0\t/var/lib/ployz-mirror/copy\tno\ton\n'
      printf 'tank/ployz-mirror/copy/fs\t{slot_bound}\t0\t0\t/var/lib/ployz-mirror/copy/fs\tno\ton\n'
    fi
    ;;
  'list -Hp -t snapshot -o guid,creation -S creation -d 1 '*)
    f='{props}'/"${{11}}/snapshots"
    [ ! -e "$f" ] || cat "$f"
    ;;
  'get -H -o value '*)
    check_property "$5"
    f='{props}'/"$6/$5"
    if [ -e "$f" ]; then cat "$f"; else echo '-'; fi
    ;;
  'set '*)
    check_property "${{2%%=*}}"
    f='{props}'/"$3/${{2%%=*}}"
    mkdir -p "${{f%/*}}"
    printf '%s\n' "${{2#*=}}" > "$f"
    ;;
  'create -o canmount=off -o mountpoint=/var/lib/ployz-volumes tank/ployz') touch '{root}' ;;
  'create -o refquota=1 tank/ployz/data') touch '{volume}' ;;
  'create -o refquota=1073741824 tank/ployz/data') touch '{volume}' ;;
  'mount tank/ployz/data') touch '{mounted}' ;;
  'destroy -r tank/ployz/data')
    if [ -e '{destroy_fails}' ]; then echo 'dataset is busy' >&2; exit 1; fi
    rm -f '{volume}' '{mounted}'
    ;;
  *) echo "unexpected fake zfs command: $*" >&2; exit 2 ;;
esac
"#,
        commands = commands.display(),
        pools = pools.escape_default(),
        root = root.display(),
        readonly_root = readonly_root.display(),
        incompatible_root = incompatible_root.display(),
        volume = volume.display(),
        readonly_volume = readonly_volume.display(),
        incompatible_volume = incompatible_volume.display(),
        unbounded_volume = unbounded_volume.display(),
        descendant = descendant.display(),
        sibling = sibling.display(),
        mounted = mounted.display(),
        destroy_fails = destroy_fails.display(),
        list_fails = list_fails.display(),
        slot = slot.display(),
        slot_bound = SLOT_BOUND_BYTES,
        props = props.display(),
    );
    fs::write(&script, script_body).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let zpool = directory.join("zpool");
    let zfs = directory.join("zfs");
    std::os::unix::fs::symlink(&script, &zpool).unwrap();
    std::os::unix::fs::symlink(&script, &zfs).unwrap();
    (zpool, zfs)
}
