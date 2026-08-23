//! First-Pool creation behavior through the Docker plugin boundary.

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::super::pool::{POOL_BACKING_FILE, PoolStorage};
use super::*;

#[tokio::test]
async fn first_create_builds_the_pool_and_bounded_volume_in_one_request() {
    let test = TestDir::new();
    let backing = test.0.join(POOL_BACKING_FILE);
    assert!(!backing.exists());

    assert_eq!(
        create_first_volume(&test, 8192, "1g").await,
        json!({"Err":""})
    );
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("fallocate -l 2147483648 ").count(), 1);
    assert_eq!(log.matches("zpool create ").count(), 1);
    assert!(log.contains("-o ashift=13 -O canmount=off ployz"));
    assert_eq!(
        log.matches("zfs create -o canmount=off -o mountpoint=/var/lib/ployz-volumes ployz/ployz")
            .count(),
        1
    );
    assert_eq!(
        log.matches("zfs create -o refquota=1073741824 ployz/ployz/data")
            .count(),
        1
    );
    assert!(backing.exists());
}

#[tokio::test]
async fn initial_capacity_uses_ten_percent_when_it_exceeds_one_gibibyte() {
    let test = TestDir::new();

    assert_eq!(
        create_first_volume(&test, 512, "20g").await,
        json!({"Err":""})
    );
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("fallocate -l 23622320128 ").count(), 1);
    assert!(log.contains("-o ashift=12 -O canmount=off ployz"));
}

#[tokio::test]
async fn first_create_refuses_before_mutation_when_root_reserve_would_be_broken() {
    let test = TestDir::new();
    fs::write(test.0.join("insufficient"), "").unwrap();

    let response = create_first_volume(&test, 4096, "1g").await;

    assert!(error(&response).contains("host-root reserve"));
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(!log.contains("fallocate"));
    assert!(!log.contains("zpool create"));
    assert!(!test.0.join(POOL_BACKING_FILE).exists());
    assert!(!test.0.join("pool").exists());
}

#[tokio::test]
async fn first_create_rejects_sparse_allocation_without_creating_a_pool() {
    let test = TestDir::new();
    fs::write(test.0.join("sparse"), "").unwrap();

    let response = create_first_volume(&test, 4096, "1g").await;

    assert!(error(&response).contains("is sparse"));
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(!log.contains("zpool create"));
    assert!(!test.0.join(POOL_BACKING_FILE).exists());
}

#[tokio::test]
async fn failed_pool_creation_removes_the_partial_pool_and_backing_file() {
    let test = TestDir::new();
    fs::write(test.0.join("fail-pool"), "").unwrap();

    let response = create_first_volume(&test, 4096, "1g").await;

    assert!(!error(&response).is_empty());
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("zpool destroy -f ployz").count(), 1);
    assert!(!test.0.join(POOL_BACKING_FILE).exists());
    assert!(!test.0.join("pool").exists());
}

#[tokio::test]
async fn failed_first_volume_removes_the_new_pool_and_backing_file() {
    let test = TestDir::new();
    fs::write(test.0.join("fail-volume"), "").unwrap();

    let response = create_first_volume(&test, 4096, "1g").await;

    assert!(!error(&response).is_empty());
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("zpool destroy -f ployz").count(), 1);
    assert!(!test.0.join(POOL_BACKING_FILE).exists());
    assert!(!test.0.join("pool").exists());
}

async fn create_first_volume(test: &TestDir, physical_block_size: u64, size: &str) -> Value {
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(
        listener,
        fake_first_pool(&test.0, physical_block_size),
    ));
    let response = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name":"data","Opts":{"size":size}}),
    )
    .await;
    server.abort();
    response
}

fn fake_first_pool(directory: &Path, physical_block_size: u64) -> VolumeStorage {
    let script = directory.join("fake-storage");
    let commands = directory.join("commands");
    let pool = directory.join("pool");
    let root = directory.join("root");
    let volume = directory.join("volume");
    let allocated = directory.join("allocated");
    let insufficient = directory.join("insufficient");
    let sparse = directory.join("sparse");
    let fail_pool = directory.join("fail-pool");
    let fail_volume = directory.join("fail-volume");
    let script_body = format!(
        r#"#!/bin/sh
set -eu
name=${{0##*/}}
printf '%s %s\n' "$name" "$*" >> '{commands}'
case "$name" in
  df)
    if [ -e '{insufficient}' ]; then
      printf 'Size Avail\n42949672960 11811160064\n'
    else
      printf 'Size Avail\n107374182400 96636764160\n'
    fi
    ;;
  fallocate) touch "$3"; printf '%s\n' "$2" > '{allocated}' ;;
  stat)
    if [ -e '{sparse}' ]; then
      printf '0 512\n'
    else
      bytes=$(cat '{allocated}')
      printf '%s 512\n' "$(((bytes + 511) / 512))"
    fi
    ;;
  zpool)
    case "$*" in
      'list -Hp -o name,health,readonly') [ ! -e '{pool}' ] || printf 'ployz\tONLINE\toff\n' ;;
      'list -H -o name') [ ! -e '{pool}' ] || printf 'ployz\n' ;;
      create*) touch '{pool}'; [ ! -e '{fail_pool}' ] || exit 2 ;;
      'destroy -f ployz') rm -f '{pool}' '{root}' '{volume}' ;;
      *) echo "unexpected fake zpool command: $*" >&2; exit 2 ;;
    esac
    ;;
  zfs)
    case "$*" in
      'list -Hp -o name,refquota,referenced,available,mountpoint,mounted,readonly -r ployz')
        available=$(cat '{allocated}')
        printf 'ployz\t0\t24576\t%s\t/ployz\tyes\toff\n' "$available"
        [ ! -e '{root}' ] || printf 'ployz/ployz\t0\t24576\t%s\t/var/lib/ployz-volumes\tno\toff\n' "$available"
        ;;
      'create -o canmount=off -o mountpoint=/var/lib/ployz-volumes ployz/ployz') touch '{root}' ;;
      'create -o refquota='*' ployz/ployz/data')
        [ ! -e '{fail_volume}' ] || exit 2
        touch '{volume}'
        ;;
      *) echo "unexpected fake zfs command: $*" >&2; exit 2 ;;
    esac
    ;;
  *) echo "unexpected fake storage program: $name" >&2; exit 2 ;;
esac
"#,
        commands = commands.display(),
        allocated = allocated.display(),
        pool = pool.display(),
        root = root.display(),
        volume = volume.display(),
        insufficient = insufficient.display(),
        sparse = sparse.display(),
        fail_pool = fail_pool.display(),
        fail_volume = fail_volume.display(),
    );
    fs::write(&script, script_body).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let program = |name: &str| {
        let path = directory.join(name);
        std::os::unix::fs::symlink(&script, &path).unwrap();
        path
    };
    let host = fs::metadata(directory).unwrap();
    let sys_dev_block = directory.join("sys-dev-block");
    let device = format!(
        "{}:{}",
        nix::sys::stat::major(host.dev()),
        nix::sys::stat::minor(host.dev())
    );
    let disk = directory.join("sys-devices/vda");
    fs::create_dir_all(disk.join("vda2")).unwrap();
    fs::create_dir(&sys_dev_block).unwrap();
    std::os::unix::fs::symlink(disk.join("vda2"), sys_dev_block.join(device)).unwrap();
    let queue = disk.join("queue");
    fs::create_dir_all(&queue).unwrap();
    fs::write(
        queue.join("physical_block_size"),
        format!("{physical_block_size}\n"),
    )
    .unwrap();
    VolumeStorage {
        pool: PoolStorage::with_environment(
            program("zpool"),
            program("fallocate"),
            program("stat"),
            program("df"),
            directory.join(POOL_BACKING_FILE),
            directory.to_owned(),
            sys_dev_block,
        ),
        zfs: program("zfs"),
        mutation: Arc::new(Mutex::new(())),
    }
}
