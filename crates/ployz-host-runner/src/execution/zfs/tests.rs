use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes, VolumeName, ZfsPoolName};
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_core::operation::FailureMessage;

use super::state::persist_prepared_storage_state;
use super::*;
use crate::execution::{HostPlatformProfile, HostRunnerCommandOutput, HostRunnerCommandRunner};

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
        &ZfsPoolName::try_new(pool).expect("test pool"),
        &NamespaceId::try_new("default").expect("test namespace"),
        &VolumeName::try_new("data").expect("test volume"),
    )
    .expect("test dataset")
}

fn persist(directory: &Path, origin: PreparedStorageOrigin) {
    let pool = match &origin {
        PreparedStorageOrigin::OwnedImage { .. } => PLOYZ_OWNED_ZFS_POOL,
        PreparedStorageOrigin::Adopted => "tank",
    };
    persist_prepared_storage_state(
        directory,
        &PreparedStorageState::try_new(
            ZfsPoolName::try_new(pool).expect("test pool"),
            origin,
            ZfsDatasetRoot::try_from(format!("{pool}/ployz/volumes")).expect("test dataset root"),
        )
        .expect("test prepared state"),
    )
    .unwrap();
}

#[test]
fn storage_capability_distinguishes_unprepared_and_unavailable_hosts() {
    let unprepared = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([]);
    assert_eq!(
        observe_storage_capability(
            &mut runner,
            unprepared.path(),
            &unprepared.path().join("zfs")
        )
        .unwrap(),
        ployz_core::machine::StorageCapability::Unprepared
    );
    assert!(runner.invocations.is_empty());

    let prepared = tempfile::tempdir().unwrap();
    persist(prepared.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([]);
    assert_eq!(
        observe_storage_capability(
            &mut runner,
            prepared.path(),
            &prepared.path().join("missing")
        )
        .unwrap(),
        ployz_core::machine::StorageCapability::Unavailable {
            reason: ployz_core::machine::StorageUnavailableReason::ZfsModuleMissing,
        }
    );
    assert!(runner.invocations.is_empty());
}

#[test]
fn storage_capability_reports_pool_absence_fault_and_readiness() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let module = state.path().join("zfs");
    std::fs::create_dir(&module).unwrap();
    let pool = ZfsPoolName::try_new("tank").unwrap();

    let mut absent = RecordingRunner::new([stdout("other\n")]);
    assert_eq!(
        observe_storage_capability(&mut absent, state.path(), &module).unwrap(),
        ployz_core::machine::StorageCapability::Unavailable {
            reason: ployz_core::machine::StorageUnavailableReason::PoolNotImported {
                pool: pool.clone(),
            },
        }
    );

    let mut faulted = RecordingRunner::new([stdout("tank\n"), stdout("FAULTED\n")]);
    assert_eq!(
        observe_storage_capability(&mut faulted, state.path(), &module).unwrap(),
        ployz_core::machine::StorageCapability::Unavailable {
            reason: ployz_core::machine::StorageUnavailableReason::PoolFaulted {
                pool: pool.clone(),
            },
        }
    );

    let mut ready = RecordingRunner::new([stdout("tank\n"), stdout("ONLINE\n")]);
    assert_eq!(
        observe_storage_capability(&mut ready, state.path(), &module).unwrap(),
        ployz_core::machine::StorageCapability::Ready { pool }
    );
    assert!(
        ready
            .invocations
            .iter()
            .all(|invocation| invocation.timeout == COMMAND_TIMEOUT)
    );
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
            candidates: vec![
                ZfsPoolName::try_new("alpha").expect("test pool"),
                ZfsPoolName::try_new("zeta").expect("test pool"),
            ],
        }
    );
}

#[test]
fn explicit_pool_must_be_imported() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([success(), success(), success(), stdout("tank\n")]);
    let requested = ZfsPoolName::try_new("archive").unwrap();
    let error = prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Explicit(requested.clone()),
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ZfsEffectError::ExplicitPoolAbsent { pool: requested }
    );
}

#[test]
fn existing_parent_dataset_with_wrong_mountpoint_is_rejected() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout("tank\n"),
        success(),
        stdout("tank/ployz\t/tank/ployz\n"),
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
        ZfsEffectError::PreparedStateMismatch { .. }
    ));
}

#[test]
fn owned_image_pool_without_descriptor_is_refused_as_ambiguous_half_state() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout("ployz\n"),
        stdout(&format!("  {PLOYZ_OWNED_ZFS_BACKING_FILE}\n")),
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
        failed("dataset absent"),
        success(),
        failed("dataset absent"),
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

    assert_eq!(prepared.origin(), &PreparedStorageOrigin::Adopted);
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
        failed("dataset absent"),
        success(),
        failed("dataset absent"),
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
        failed("dataset absent"),
        success(),
        failed("dataset absent"),
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
        prepared.origin(),
        &PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE)
        }
    );
    assert_eq!(invocation(&runner, 6).program, "truncate");
    assert_eq!(
        invocation(&runner, 6).args,
        vec!["-s", "1048576", PLOYZ_OWNED_ZFS_BACKING_FILE]
    );
    assert_eq!(invocation(&runner, 7).program, "zpool");
    assert_eq!(
        invocation(&runner, 7).args,
        vec![
            "create",
            "-f",
            PLOYZ_OWNED_ZFS_POOL,
            PLOYZ_OWNED_ZFS_BACKING_FILE
        ]
    );
}

#[test]
fn owned_origin_survives_restart_and_selects_filesystem_capacity() {
    let state = tempfile::tempdir().unwrap();
    persist(
        state.path(),
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
        },
    );
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout(&format!("  {PLOYZ_OWNED_ZFS_BACKING_FILE}\n")),
        stdout("Available\n4096\n"),
        stdout("ployz/ployz/volumes\tnone\n"),
    ]);
    let facts = gather_pool_capacity(&mut runner, state.path()).unwrap();
    assert_eq!(facts.available_bytes, 4096);
    assert_eq!(invocation(&runner, 3).program, "df");
    assert_eq!(
        invocation(&runner, 3).args,
        vec!["-B1", "--output=avail", PLOYZ_OWNED_ZFS_BACKING_FILE]
    );
}

#[test]
fn repeated_prepare_verifies_and_preserves_owned_origin() {
    let state = tempfile::tempdir().unwrap();
    persist(
        state.path(),
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
        },
    );
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout("ployz\n"),
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout(&format!("  {PLOYZ_OWNED_ZFS_BACKING_FILE}\n")),
        success(),
        stdout("ployz/ployz\tnone\n"),
        stdout(&format!("ployz/ployz/volumes\t{VOLUME_MOUNTPOINT}\n")),
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
        prepared.origin(),
        PreparedStorageOrigin::OwnedImage { .. }
    ));
    assert!(
        runner
            .invocations
            .iter()
            .all(|call| call.program != "truncate")
    );
    assert!(runner.invocations.iter().all(|call| {
        !(call.program == "zfs" && call.args.first().is_some_and(|arg| arg == "create"))
    }));
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
fn pool_capacity_parses_and_orders_direct_child_quotas() {
    let state = tempfile::tempdir().expect("temporary state");
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let alpha = DatasetName::for_volume(
        &ZfsPoolName::try_new("tank").expect("pool"),
        &NamespaceId::try_new("alpha").expect("namespace"),
        &VolumeName::try_new("data").expect("volume"),
    )
    .expect("dataset");
    let zeta = DatasetName::for_volume(
        &ZfsPoolName::try_new("tank").expect("pool"),
        &NamespaceId::try_new("zeta").expect("namespace"),
        &VolumeName::try_new("data").expect("volume"),
    )
    .expect("dataset");
    let rows = format!(
        "tank/ployz/volumes\tnone\n{}\t2048\n{}\t1024\n",
        zeta.as_str(),
        alpha.as_str()
    );
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("8192\n"),
        stdout(&rows),
    ]);

    let facts = gather_pool_capacity(&mut runner, state.path()).expect("capacity facts");

    let mut expected = vec![
        DatasetQuotaFact {
            dataset: alpha,
            quota_bytes: 1024,
        },
        DatasetQuotaFact {
            dataset: zeta,
            quota_bytes: 2048,
        },
    ];
    expected.sort_by(|left, right| left.dataset.cmp(&right.dataset));
    assert_eq!(facts.child_quotas, expected);
}

#[test]
fn pool_capacity_rejects_invalid_child_quota_rows() {
    let state = tempfile::tempdir().expect("temporary state");
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("8192\n"),
        stdout("tank/ployz/volumes\tnone\ninvalid-row\n"),
    ]);

    assert!(matches!(
        gather_pool_capacity(&mut runner, state.path()),
        Err(ZfsEffectError::GatherParse { .. })
    ));
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
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("4096\n"),
        stdout("tank/ployz/volumes\tnone\n"),
        success(),
    ]);
    create_dataset(
        &mut runner,
        state.path(),
        &dataset("tank"),
        VolumeMaxSizeBytes::try_new(1024).unwrap(),
    )
    .unwrap();
    assert_eq!(invocation(&runner, 4).program, "zfs");
    assert_eq!(
        invocation(&runner, 4).args.first().map(String::as_str),
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
fn dataset_quota_growth_is_admitted_against_total_capacity_and_equal_is_a_no_op() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let dataset = dataset("tank");
    let rows = format!("tank/ployz/volumes\tnone\n{}\t1024\n", dataset.as_str());
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("1024\n"),
        stdout("4096\n"),
        stdout(&rows),
        success(),
    ]);
    grow_dataset_quota(
        &mut runner,
        state.path(),
        &dataset,
        VolumeMaxSizeBytes::try_new(2048).unwrap(),
    )
    .unwrap();
    assert_eq!(
        invocation(&runner, 5).args,
        vec!["set", "quota=2048", dataset.as_str()]
    );

    let mut runner = RecordingRunner::new([success(), stdout(VOLUME_MOUNTPOINT), stdout("2048\n")]);
    grow_dataset_quota(
        &mut runner,
        state.path(),
        &dataset,
        VolumeMaxSizeBytes::try_new(2048).unwrap(),
    )
    .unwrap();
    assert_eq!(runner.invocations.len(), 3);
}

#[test]
fn dataset_quota_admission_rejects_total_above_available_capacity() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("100\n"),
        stdout("tank/ployz/volumes\tnone\n"),
    ]);
    let error = create_dataset(
        &mut runner,
        state.path(),
        &dataset("tank"),
        VolumeMaxSizeBytes::try_new(101).unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ZfsEffectError::QuotaCapacityExceeded {
            available: 100,
            requested_total: 101,
        }
    );
    assert_eq!(runner.invocations.len(), 4);
}

#[test]
fn dataset_facts_use_zfs_used_bytes_and_mount_directory_metadata_time() {
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
            mount_directory_modified_unix_seconds: 1_700_000_000,
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
