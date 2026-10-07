//! Fake ZFS command adapter for volume-plugin route tests.
//!
//! Marker files in the fixture directory stand for datasets; `props/<dataset>/<property>`
//! files stand for properties, and `props/<dataset>/snapshots` for a dataset's snapshots
//! (`name<TAB>guid<TAB>creation` per line, newest first). A `zfs receive` reads one line
//! of stream text: `snapshot <name> <guid>` lands that snapshot; `break` leaves a resume
//! token behind and fails like an interrupted stream. The `readonly-lost` marker stands for
//! a ZFS that does not keep `readonly=on` on the received copy, however it is set. Like ZFS,
//! a command naming a dataset no marker stands for fails with `dataset does not exist`.
//! The `rename-fails` and `inherit-fails` markers make those commands fail while they exist.

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
    let rename_fails = directory.join("rename-fails");
    let inherit_fails = directory.join("inherit-fails");
    let list_fails = directory.join("list-fails");
    let slot = directory.join("slot");
    let mirror_root = directory.join("mirror-root");
    let mirror = directory.join("mirror");
    let mirror_fs = directory.join("mirror-fs");
    let readonly_lost = directory.join("readonly-lost");
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
prop() {{
  check_property "$2"
  f='{props}'/"$1/$2"
  if [ -e "$f" ]; then cat "$f"; else printf '%s\n' "$3"; fi
}}
exists() {{
  case "$1" in
    tank) ;;
    tank/ployz) [ -e '{root}' ] ;;
    tank/ployz/data) [ -e '{volume}' ] ;;
    tank/ployz/data/child) [ -e '{descendant}' ] ;;
    tank/ployz/sibling) [ -e '{sibling}' ] ;;
    tank/ployz-mirror) [ -e '{slot}' ] || [ -e '{mirror_root}' ] || [ -e '{mirror}' ] ;;
    tank/ployz-mirror/copy|tank/ployz-mirror/copy/fs) [ -e '{slot}' ] ;;
    tank/ployz-mirror/data) [ -e '{mirror}' ] ;;
    tank/ployz-mirror/data/fs) [ -e '{mirror}' ] && [ -e '{mirror_fs}' ] ;;
    *) false ;;
  esac
}}
require() {{
  exists "${{1%%@*}}" || {{ echo "cannot open '${{1%%@*}}': dataset does not exist" >&2; exit 1; }}
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
    if [ -e '{slot}' ] || [ -e '{mirror_root}' ] || [ -e '{mirror}' ]; then
      printf 'tank/ployz-mirror\t0\t0\t0\t/var/lib/ployz-mirror\tno\ton\n'
    fi
    if [ -e '{slot}' ]; then
      printf 'tank/ployz-mirror/copy\t{slot_bound}\t0\t0\t/var/lib/ployz-mirror/copy\tno\ton\n'
      printf 'tank/ployz-mirror/copy/fs\t{slot_bound}\t0\t0\t/var/lib/ployz-mirror/copy/fs\tno\ton\n'
    fi
    if [ -e '{mirror}' ]; then
      printf 'tank/ployz-mirror/data\t%s\t0\t0\t/var/lib/ployz-mirror/data\tno\ton\n' "$(prop tank/ployz-mirror/data refquota {slot_bound})"
      if [ -e '{mirror_fs}' ]; then
        printf 'tank/ployz-mirror/data/fs\t%s\t0\t0\t/var/lib/ployz-mirror/data/fs\tno\t%s\n' "$(prop tank/ployz-mirror/data/fs refquota {slot_bound})" "$(prop tank/ployz-mirror/data/fs readonly on)"
      fi
    fi
    ;;
  'list -Hp -t snapshot -o name,guid,creation -S createtxg -d 1 '*)
    require "${{11}}"
    f='{props}'/"${{11}}/snapshots"
    [ ! -e "$f" ] || cat "$f"
    ;;
  'get -H -o property,value -s local all '*)
    d='{props}'/"$8"
    [ -d "$d" ] || exit 0
    for f in "$d"/*; do
      [ -f "$f" ] || continue
      case "${{f##*/}}" in snapshots) continue ;; esac
      printf '%s\t%s\n' "${{f##*/}}" "$(cat "$f")"
    done
    ;;
  'get -H -o value '*)
    require "$6"
    prop "$6" "$5" -
    ;;
  'get -Hp -o value written@'*)
    require "$6"
    prop "$6" "$5" 1
    ;;
  'get -Hp -o value '*)
    require "$6"
    prop "$6" "$5" -
    ;;
  'set readonly=on tank/ployz-mirror/data/fs')
    if [ -e '{props}/fail-properties' ]; then echo 'property unavailable' >&2; exit 1; fi
    require tank/ployz-mirror/data/fs
    [ -e '{readonly_lost}' ] || {{ mkdir -p '{props}/tank/ployz-mirror/data/fs'; echo on > '{props}/tank/ployz-mirror/data/fs/readonly'; }}
    ;;
  'set '*)
    require "$3"
    check_property "${{2%%=*}}"
    f='{props}'/"$3/${{2%%=*}}"
    mkdir -p "${{f%/*}}"
    printf '%s\n' "${{2#*=}}" > "$f"
    ;;
  'inherit '*)
    if [ -e '{inherit_fails}' ]; then echo 'property unavailable' >&2; exit 1; fi
    require "$3"
    rm -f '{props}'/"$3/$2"
    ;;
  'snapshot '*)
    require "$2"
    f='{props}'/"${{2%%@*}}/snapshots"
    mkdir -p "${{f%/*}}"
    if [ -e "$f" ] && grep -q "^$2	" "$f"; then echo "cannot create snapshot '$2': dataset already exists" >&2; exit 1; fi
    if [ -e "$f" ]; then count=$(wc -l < "$f"); else count=0; fi
    {{ printf '%s\t%s\t%s\n' "$2" "$((1000 + count))" "$((1700000000 + count))"; [ ! -e "$f" ] || cat "$f"; }} > "$f.new"
    mv "$f.new" "$f"
    ;;
  'create -o canmount=off -o mountpoint=/var/lib/ployz-volumes tank/ployz') touch '{root}' ;;
  'create -o refquota=1 tank/ployz/data') touch '{volume}' ;;
  'create -o refquota=1073741824 tank/ployz/data') touch '{volume}' ;;
  'create -o canmount=off -o mountpoint=/var/lib/ployz-mirror -o readonly=on tank/ployz-mirror') touch '{mirror_root}' ;;
  'create -o canmount=off -o readonly=on -o refquota='*' tank/ployz-mirror/data')
    if [ -e '{mirror}' ]; then echo "cannot create 'tank/ployz-mirror/data': dataset already exists" >&2; exit 1; fi
    touch '{mirror}'
    mkdir -p '{props}/tank/ployz-mirror/data'
    printf '%s\n' "${{7#refquota=}}" > '{props}/tank/ployz-mirror/data/refquota'
    ;;
  'mount tank/ployz/data') touch '{mounted}' ;;
  'unmount tank/ployz/data') rm -f '{mounted}' ;;
  'rename tank/ployz/data tank/ployz-mirror/data/fs')
    if [ -e '{rename_fails}' ]; then echo 'rename interrupted' >&2; exit 1; fi
    require tank/ployz/data
    require tank/ployz-mirror/data
    if [ -e '{readonly_volume}' ]; then moved_readonly=on; else moved_readonly=off; fi
    rm -f '{volume}' '{mounted}'
    touch '{mirror_fs}'
    rm -rf '{props}/tank/ployz-mirror/data/fs'
    mkdir -p '{props}/tank/ployz-mirror/data/fs'
    if [ -d '{props}/tank/ployz/data' ]; then
      for f in '{props}'/tank/ployz/data/*; do
        [ -f "$f" ] || continue
        sed 's#^tank/ployz/data@#tank/ployz-mirror/data/fs@#' "$f" > '{props}'/tank/ployz-mirror/data/fs/"${{f##*/}}"
      done
      rm -rf '{props}/tank/ployz/data'
    fi
    echo 1073741824 > '{props}/tank/ployz-mirror/data/fs/refquota'
    echo "$moved_readonly" > '{props}/tank/ployz-mirror/data/fs/readonly'
    ;;
  'destroy -r tank/ployz/data')
    if [ -e '{destroy_fails}' ]; then echo 'dataset is busy' >&2; exit 1; fi
    rm -f '{volume}' '{mounted}'
    ;;
  'destroy -r tank/ployz-mirror/data')
    rm -rf '{mirror}' '{mirror_fs}' '{props}/tank/ployz-mirror/data'
    ;;
  'destroy '*'@'*)
    require "$2"
    f='{props}'/"${{2%%@*}}/snapshots"
    grep -v "^$2	" "$f" > "$f.new" || true
    mv "$f.new" "$f"
    ;;
  'receive -A tank/ployz-mirror/data/fs')
    require tank/ployz-mirror/data/fs
    rm -f '{props}/tank/ployz-mirror/data/fs/receive_resume_token'
    ;;
  'receive -u -s -o readonly=on -o refquota='*' tank/ployz-mirror/data/fs')
    echo $$ > '{props}/receive-pid'
    if [ -e '{props}/stall-read' ]; then exec sleep 30; fi
    read -r verb arg guid || verb=empty
    if [ -e '{props}/stall-exit' ]; then cat >/dev/null; exec sleep 30; fi
    touch '{mirror_fs}'
    d='{props}/tank/ployz-mirror/data/fs'
    mkdir -p "$d"
    case "$verb" in
      snapshot)
        rm -f "$d/receive_resume_token"
        count=0; [ ! -e "$d/snapshots" ] || count=$(wc -l < "$d/snapshots")
        {{ printf 'tank/ployz-mirror/data/fs@%s\t%s\t%s\n' "$arg" "$guid" "$((1700000000 + count))"; [ ! -e "$d/snapshots" ] || cat "$d/snapshots"; }} > "$d/snapshots.new"
        mv "$d/snapshots.new" "$d/snapshots"
        # A first receive lands the stream's properties; a lost connection loses them.
        if [ -e '{readonly_lost}' ]; then echo off > "$d/readonly.new"; else echo on > "$d/readonly.new"; fi
        mv "$d/readonly.new" "$d/readonly"
        printf '%s\n' "${{7#refquota=}}" > "$d/refquota"
        ;;
      break)
        cat >/dev/null
        echo 'cannot receive: connection reset' >&2
        echo 'token-1' > "$d/receive_resume_token"
        echo off > "$d/readonly.new"
        mv "$d/readonly.new" "$d/readonly"
        exit 1
        ;;
      *) echo "fake zfs receive: unexpected stream $verb" >&2; exit 1 ;;
    esac
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
        rename_fails = rename_fails.display(),
        inherit_fails = inherit_fails.display(),
        list_fails = list_fails.display(),
        slot = slot.display(),
        mirror_root = mirror_root.display(),
        mirror = mirror.display(),
        mirror_fs = mirror_fs.display(),
        readonly_lost = readonly_lost.display(),
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
