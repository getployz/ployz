
use std::collections::VecDeque;

use ployz_core::deploy::{VolumeName, ZfsPoolName};
use ployz_core::ids::NamespaceId;
use ployz_core::operation::FailureMessage;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    program: String,
    args: Vec<String>,
    timeout: Duration,
}

struct RecordingRunner {
    outputs: VecDeque<HostRunnerCommandOutput>,
    invocations: Vec<Invocation>,
}

impl RecordingRunner {
    fn new(outputs: impl IntoIterator<Item = HostRunnerCommandOutput>) -> Self {
        Self {
            outputs: outputs.into_iter().collect(),
            invocations: Vec::new(),
        }
    }
}

fn invocation(runner: &RecordingRunner, index: usize) -> &Invocation {
    runner
        .invocations
        .get(index)
        .expect("expected recorded invocation")
}

impl HostRunnerCommandRunner for RecordingRunner {
    fn command(
        &mut self,
        _program: &str,
        _args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        unreachable!("ZFS effects always use the bounded command seam")
    }

    fn command_with_timeout(
        &mut self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        self.invocations.push(Invocation {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            timeout,
        });
        if program == "zfs" && args.first() == Some(&"list") {
            return Ok(success());
        }
        Ok(self.outputs.pop_front().unwrap_or_else(success))
    }

    fn is_linux(&mut self) -> bool {
        true
    }
    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(0)
    }
    fn download(&mut self, _: &str, _: &Path) -> Result<(), FailureMessage> {
        Ok(())
    }
    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        Ok(())
    }
    fn docker_is_installed(&mut self) -> bool {
        true
    }
    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        Ok(false)
    }
    fn docker_has_insecure_registry(&mut self, _: &str) -> Result<bool, FailureMessage> {
        Ok(false)
    }
}

fn success() -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: true,
        exit_code: Some(0),
        stdout: String::new(),
        failure: String::new(),
    }
}

fn stdout(value: &str) -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: true,
        exit_code: Some(0),
        stdout: value.to_owned(),
        failure: String::new(),
    }
}

fn failed(message: &str) -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: false,
        exit_code: Some(1),
        stdout: String::new(),
        failure: message.to_owned(),
    }
}

fn profile(value: &str) -> HostPlatformProfile {
    super::super::detect_host_platform(value).expect("profile is supported")
}

fn dataset(pool: &str) -> DatasetName {
    DatasetName::for_volume(
        &ZfsPoolName::try_new(pool).unwrap(),
        &NamespaceId::try_new("default").unwrap(),
        &VolumeName::try_new("data").unwrap(),
    )
    .unwrap()
}

fn persist(directory: &Path, origin: PreparedStorageOrigin) {
    let pool = match &origin {
        PreparedStorageOrigin::OwnedImage { .. } => OWNED_POOL,
        PreparedStorageOrigin::Adopted => "tank",
    };
    persist_prepared_storage_state(
        directory,
        &PreparedStorageState {
            pool: pool.to_owned(),
            origin,
            dataset_root: format!("{pool}/ployz/volumes"),
        },
    )
    .unwrap();
}

#[test]
fn unsupported_profile_refuses_before_mutation() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([]);
    let error = prepare_storage(
        &mut runner,
        &profile("ID=arch\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap_err();
    assert_eq!(error, ZfsEffectError::UnsupportedPlatform);
    assert!(runner.invocations.is_empty());
}

#[test]
fn package_failure_is_typed_and_the_invocation_is_bounded() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([failed("apt unavailable")]);

    let error = prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap_err();

    assert_eq!(
        error,
        ZfsEffectError::Installation {
            message: "apt unavailable".to_owned()
        }
    );
    assert_eq!(invocation(&runner, 0).timeout, INSTALL_TIMEOUT);
}

#[test]
fn operation_scoped_terminal_failure_is_final_on_replay() {
    let state = tempfile::tempdir().unwrap();
    let operation_id = OperationId::try_new("op_storage_prepare").unwrap();
    let mut first = RecordingRunner::new([]);
    let first_error = prepare_storage_for_operation(
        &mut first,
        &profile("ID=arch\n"),
        &operation_id,
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap_err();
    let mut replay = RecordingRunner::new([]);
    let replay_error = prepare_storage_for_operation(
        &mut replay,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &operation_id,
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap_err();

    assert_eq!(replay_error, first_error);
    assert!(replay.invocations.is_empty());
}

#[test]
fn automatic_multiple_pools_are_sorted_and_refused() {
    let state = tempfile::tempdir().unwrap();
    let mut runner =
        RecordingRunner::new([success(), success(), success(), stdout("zeta\nalpha\n")]);
    let error = prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ZfsEffectError::AmbiguousPools {
            candidates: vec!["alpha".to_owned(), "zeta".to_owned()],
        }
    );
}

#[test]
fn owned_image_pool_without_descriptor_is_refused_as_ambiguous_half_state() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout("ployz\n"),
        stdout(&format!("  {OWNED_BACKING_FILE}\n")),
    ]);

    let error = prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ZfsEffectError::PreparedStateUnavailable { .. }
    ));
    assert!(
        runner
            .invocations
            .iter()
            .all(|call| call.program != "truncate")
    );
}

#[test]
fn ubuntu_one_pool_is_adopted_with_exact_bounded_setup() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout("tank\n"),
        success(),
        success(),
        success(),
    ]);
    let prepared = prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap();

    assert_eq!(prepared.origin, PreparedStorageOrigin::Adopted);
    assert_eq!(load_prepared_storage_state(state.path()).unwrap(), prepared);
    assert_eq!(invocation(&runner, 0).program, "env");
    assert_eq!(
        invocation(&runner, 0).args,
        vec!["DEBIAN_FRONTEND=noninteractive", "apt-get", "update"]
    );
    assert_eq!(
        invocation(&runner, 1).args,
        vec![
            "DEBIAN_FRONTEND=noninteractive",
            "apt-get",
            "install",
            "-y",
            "zfsutils-linux"
        ]
    );
    assert_eq!(invocation(&runner, 2).args, vec!["zfs"]);
    assert!(
        runner
            .invocations
            .iter()
            .all(|invocation| invocation.timeout > Duration::ZERO)
    );
    assert!(runner.invocations.iter().all(|invocation| {
        invocation.program != "mkdir"
            && !(invocation.program == "install"
                && invocation.args.contains(&VOLUME_MOUNTPOINT.to_owned()))
    }));
    assert_eq!(
        std::fs::read_to_string(state.path().join("docker.service.d/ployz-zfs.conf")).unwrap(),
        "[Unit]\nAfter=zfs.target\n"
    );
    assert_eq!(runner.invocations.last().unwrap().program, "systemctl");
    assert_eq!(
        runner.invocations.last().unwrap().args,
        vec!["daemon-reload"]
    );
}

#[test]
fn rocky_installs_repo_epel_matching_kernel_and_zfs() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        stdout("5.14.0-570.el9.x86_64\n"),
        success(),
        success(),
        stdout("tank\n"),
        success(),
        success(),
        success(),
    ]);
    prepare_storage(
        &mut runner,
        &profile("ID=rocky\nVERSION_ID=9.8\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap();

    assert_eq!(invocation(&runner, 0).program, "dnf");
    assert_eq!(
        invocation(&runner, 0).args,
        vec![
            "install",
            "-y",
            "https://zfsonlinux.org/epel/zfs-release-3-0.el9.noarch.rpm",
            "epel-release"
        ]
    );
    assert_eq!(invocation(&runner, 1).program, "uname");
    assert_eq!(
        invocation(&runner, 2).args,
        vec!["install", "-y", "kernel-devel-5.14.0-570.el9.x86_64", "zfs"]
    );
    assert_eq!(invocation(&runner, 3).program, "modprobe");
}

#[test]
fn zero_pools_create_sparse_owned_pool_from_filesystem_total() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout(""),
        success(),
        stdout("1B-blocks\n1048576\n"),
        success(),
        success(),
        success(),
        success(),
        success(),
    ]);
    let prepared = prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap();

    assert_eq!(
        prepared.origin,
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(OWNED_BACKING_FILE)
        }
    );
    assert_eq!(invocation(&runner, 6).program, "truncate");
    assert_eq!(
        invocation(&runner, 6).args,
        vec!["-s", "1048576", OWNED_BACKING_FILE]
    );
    assert_eq!(invocation(&runner, 7).program, "zpool");
    assert_eq!(
        invocation(&runner, 7).args,
        vec!["create", "-f", OWNED_POOL, OWNED_BACKING_FILE]
    );
}

#[test]
fn owned_origin_survives_restart_and_selects_filesystem_capacity() {
    let state = tempfile::tempdir().unwrap();
    persist(
        state.path(),
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(OWNED_BACKING_FILE),
        },
    );
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout(&format!("  {OWNED_BACKING_FILE}\n")),
        stdout("Available\n4096\n"),
        stdout("ployz/ployz/volumes\tnone\n"),
    ]);
    let facts = gather_pool_capacity(&mut runner, state.path()).unwrap();
    assert_eq!(facts.available_bytes, 4096);
    assert_eq!(invocation(&runner, 3).program, "df");
    assert_eq!(
        invocation(&runner, 3).args,
        vec!["-B1", "--output=avail", OWNED_BACKING_FILE]
    );
}

#[test]
fn repeated_prepare_verifies_and_preserves_owned_origin() {
    let state = tempfile::tempdir().unwrap();
    persist(
        state.path(),
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(OWNED_BACKING_FILE),
        },
    );
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout("ployz\n"),
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout(&format!("  {OWNED_BACKING_FILE}\n")),
        success(),
        success(),
        success(),
    ]);
    let prepared = prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap();

    assert!(matches!(
        prepared.origin,
        PreparedStorageOrigin::OwnedImage { .. }
    ));
    assert!(
        runner
            .invocations
            .iter()
            .all(|call| call.program != "truncate")
    );
}

#[test]
fn adopted_origin_survives_restart_and_selects_zpool_capacity() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("8192\n"),
        stdout("tank/ployz/volumes\tnone\n"),
    ]);
    let facts = gather_pool_capacity(&mut runner, state.path()).unwrap();
    assert_eq!(facts.available_bytes, 8192);
    assert_eq!(invocation(&runner, 2).program, "zpool");
    assert_eq!(
        invocation(&runner, 2).args,
        vec!["list", "-H", "-p", "-o", "free", "tank"]
    );
}

#[test]
fn missing_and_mismatched_descriptors_fail_typed() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([]);
    assert!(matches!(
        gather_pool_capacity(&mut runner, state.path()),
        Err(ZfsEffectError::PreparedStateUnavailable { .. })
    ));
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([success(), stdout("/wrong")]);
    assert!(matches!(
        gather_pool_capacity(&mut runner, state.path()),
        Err(ZfsEffectError::PreparedStateMismatch { .. })
    ));
}

#[test]
fn dataset_create_uses_quota_without_creating_mountpoint() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([success(), stdout(VOLUME_MOUNTPOINT), success()]);
    create_dataset(
        &mut runner,
        state.path(),
        &dataset("tank"),
        VolumeMaxSizeBytes::try_new(1024).unwrap(),
    )
    .unwrap();
    assert_eq!(invocation(&runner, 2).program, "zfs");
    assert_eq!(
        invocation(&runner, 2).args.first().map(String::as_str),
        Some("create")
    );
    assert!(runner.invocations.iter().all(|call| {
        call.program != "mkdir"
            && !(call.program == "install" && call.args.contains(&VOLUME_MOUNTPOINT.to_owned()))
    }));
}

#[test]
fn dataset_quota_is_grow_only() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([success(), stdout(VOLUME_MOUNTPOINT), stdout("2048\n")]);
    let error = grow_dataset_quota(
        &mut runner,
        state.path(),
        &dataset("tank"),
        VolumeMaxSizeBytes::try_new(1024).unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ZfsEffectError::QuotaShrink {
            current: 2048,
            requested: 1024,
            ..
        }
    ));
    assert_eq!(runner.invocations.len(), 3);
}

#[test]
fn dataset_facts_use_zfs_used_bytes_and_mount_last_write_time() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("4096\n"),
        stdout("1700000000\n"),
    ]);

    let facts = gather_dataset_facts(&mut runner, state.path(), &dataset("tank")).unwrap();

    assert_eq!(
        facts,
        DatasetFacts {
            used_bytes: 4096,
            last_write_unix_seconds: 1_700_000_000,
        }
    );
    assert_eq!(invocation(&runner, 3).program, "stat");
    let expected_path = format!(
        "/var/lib/ployz/volumes/{}",
        dataset("tank").as_str().rsplit('/').next().unwrap()
    );
    assert_eq!(
        invocation(&runner, 3).args,
        vec!["-c".to_owned(), "%Y".to_owned(), expected_path]
    );
}

#[test]
fn destroy_is_bounded_and_non_recursive() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([success(), stdout(VOLUME_MOUNTPOINT), success()]);
    let dataset = dataset("tank");
    destroy_dataset(&mut runner, state.path(), &dataset).unwrap();
    assert_eq!(
        invocation(&runner, 2),
        &Invocation {
            program: "zfs".to_owned(),
            args: vec!["destroy".to_owned(), dataset.as_str().to_owned()],
            timeout: COMMAND_TIMEOUT,
        }
    );
}
