use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes, VolumeName, ZfsPoolName};
use ployz_core::ids::{CorrosionNamespaceName, OperationId};
use ployz_core::machine::{DatasetQuotaFact, PoolCapacityFacts, VolumeUsageFacts};
use ployz_core::operation::FailureMessage;
use ployz_core::storage::{
    MACHINE_STORAGE_PREPARE_BUDGET, MachineStoragePreparationEvidence,
    PROVISIONED_VOLUME_MOUNTPOINT, StorageOperationEvidenceFile,
};

use super::dataset::{DatasetEnsureLock, dataset_ensure_lock_path};
use super::state::{imported_pools, persist_prepared_storage_state};
use super::*;
use crate::execution::{HostPlatformProfile, HostRunnerCommandOutput, HostRunnerCommandRunner};

mod recovery;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    program: String,
    args: Vec<String>,
    timeout: Duration,
}

struct RecordingRunner {
    outputs: VecDeque<HostRunnerCommandOutput>,
    invocations: Vec<Invocation>,
    running_evidence: Option<(PathBuf, OperationId)>,
}

impl RecordingRunner {
    fn new(outputs: impl IntoIterator<Item = HostRunnerCommandOutput>) -> Self {
        Self {
            outputs: outputs.into_iter().collect(),
            invocations: Vec::new(),
            running_evidence: None,
        }
    }

    fn expect_running_evidence(mut self, state: &Path, operation_id: OperationId) -> Self {
        self.running_evidence = Some((state.to_path_buf(), operation_id));
        self
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
        program: &str,
        args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        self.command_with_timeout(program, args, COMMAND_TIMEOUT)
    }

    fn command_with_timeout(
        &mut self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        if let Some((state, operation_id)) = self.running_evidence.take() {
            let file = StorageOperationEvidenceFile::in_state_directory(&state, operation_id);
            let evidence: MachineStoragePreparationEvidence = serde_json::from_slice(
                &std::fs::read(file.path()).expect("running evidence exists before first effect"),
            )
            .expect("running evidence decodes");
            assert!(matches!(
                evidence,
                MachineStoragePreparationEvidence::Running { .. }
            ));
        }
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
        stdout_truncated: false,
        failure: String::new(),
    }
}

fn stdout(value: &str) -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: true,
        exit_code: Some(0),
        stdout: value.to_owned(),
        stdout_truncated: false,
        failure: String::new(),
    }
}

fn truncated_stdout(value: &str) -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        stdout_truncated: true,
        ..stdout(value)
    }
}

fn pool_status(pool: &str, vdev: &str) -> HostRunnerCommandOutput {
    stdout(&format!(
        "  pool: {pool}\n state: ONLINE\nconfig:\n\n\tNAME                                      STATE     READ WRITE CKSUM\n\t{pool}                                     ONLINE       0     0     0\n\t  {vdev}  ONLINE       0     0     0\n\nerrors: No known data errors\n"
    ))
}

fn mounted_dataset_shape(dataset: &DatasetName) -> HostRunnerCommandOutput {
    let leaf = dataset.as_str().rsplit('/').next().expect("dataset leaf");
    stdout(&format!("{PROVISIONED_VOLUME_MOUNTPOINT}/{leaf}\nyes\n"))
}

fn failed(message: &str) -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: false,
        exit_code: Some(1),
        stdout: String::new(),
        stdout_truncated: false,
        failure: message.to_owned(),
    }
}

fn failed_owned_pool_create_outputs(
    observation: impl IntoIterator<Item = HostRunnerCommandOutput>,
) -> Vec<HostRunnerCommandOutput> {
    [
        success(),
        success(),
        success(),
        stdout(""),
        failed("backing file absent"),
        success(),
        stdout("1B-blocks Avail\n25769803776 23622320128\n"),
        success(),
        success(),
        stdout("1B-blocks Avail\n25769803776 10737418240\n"),
        failed("pool create failed"),
    ]
    .into_iter()
    .chain(observation)
    .collect()
}

fn profile(value: &str) -> HostPlatformProfile {
    super::super::detect_host_platform(value).expect("profile is supported")
}

fn dataset(pool: &str) -> DatasetName {
    DatasetName::for_volume(
        &ZfsPoolName::try_new(pool).expect("test pool"),
        &CorrosionNamespaceName::try_new("default").expect("test namespace"),
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

    let mut ready = RecordingRunner::new([
        stdout("tank\n"),
        stdout("ONLINE\n"),
        stdout("tank\n"),
        stdout(PROVISIONED_VOLUME_MOUNTPOINT),
        stdout("16384 8192\n"),
        stdout("0\n"),
        stdout("tank/ployz/volumes\tnone\n"),
    ]);
    assert_eq!(
        observe_storage_capability(&mut ready, state.path(), &module).unwrap(),
        ployz_core::machine::StorageCapability::Ready {
            pool,
            capacity: PoolCapacityFacts {
                total_bytes: 16_384,
                provisioned_used_bytes: 0,
                free_bytes: 8192,
                child_quotas: Vec::new(),
            },
        }
    );
    assert!(
        ready
            .invocations
            .iter()
            .all(|invocation| invocation.timeout == COMMAND_TIMEOUT)
    );
}

#[test]
fn storage_capability_does_not_report_ready_without_capacity() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let module = state.path().join("zfs");
    std::fs::create_dir(&module).unwrap();
    let mut runner = RecordingRunner::new([
        stdout("tank\n"),
        stdout("ONLINE\n"),
        stdout("tank\n"),
        stdout(PROVISIONED_VOLUME_MOUNTPOINT),
        failed("capacity unavailable"),
    ]);

    assert_eq!(
        observe_storage_capability(&mut runner, state.path(), &module).unwrap(),
        ployz_core::machine::StorageCapability::Unavailable {
            reason: ployz_core::machine::StorageUnavailableReason::CapacityFactsUnavailable,
        }
    );

    let mut parse_failure = RecordingRunner::new([
        stdout("tank\n"),
        stdout("ONLINE\n"),
        stdout("tank\n"),
        stdout(PROVISIONED_VOLUME_MOUNTPOINT),
        stdout("8192\n"),
        stdout("NAME QUOTA\ntank/ployz/volumes/not-a-child nope\n"),
    ]);
    assert_eq!(
        observe_storage_capability(&mut parse_failure, state.path(), &module).unwrap(),
        ployz_core::machine::StorageCapability::Unavailable {
            reason: ployz_core::machine::StorageUnavailableReason::CapacityFactsUnavailable,
        }
    );
}

#[test]
fn storage_capability_rejects_adopted_pool_with_wrong_dataset_root_mountpoint() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let module = state.path().join("zfs");
    std::fs::create_dir(&module).unwrap();
    let mut runner = RecordingRunner::new([
        stdout("tank\n"),
        stdout("ONLINE\n"),
        stdout("tank\n"),
        stdout("/wrong"),
    ]);

    let error = observe_storage_capability(&mut runner, state.path(), &module).unwrap_err();

    assert!(matches!(
        error,
        ZfsEffectError::PreparedStateMismatch { .. }
    ));
    assert_eq!(runner.invocations.len(), 4);
}

#[test]
fn storage_capability_rejects_owned_pool_with_wrong_backing_identity() {
    let state = tempfile::tempdir().unwrap();
    persist(
        state.path(),
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
        },
    );
    let module = state.path().join("zfs");
    std::fs::create_dir(&module).unwrap();
    let mut runner = RecordingRunner::new([
        stdout("ployz\n"),
        stdout("ONLINE\n"),
        stdout("ployz\n"),
        stdout(PROVISIONED_VOLUME_MOUNTPOINT),
        pool_status(PLOYZ_OWNED_ZFS_POOL, "/var/lib/ployz/zfs/other.img"),
    ]);

    let error = observe_storage_capability(&mut runner, state.path(), &module).unwrap_err();

    assert!(matches!(
        error,
        ZfsEffectError::PreparedStateMismatch { .. }
    ));
    assert_eq!(runner.invocations.len(), 5);
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
fn package_install_timeout_fits_within_storage_prepare_budget() {
    assert_eq!(INSTALL_TIMEOUT, Duration::from_secs(20 * 60));
    assert!(INSTALL_TIMEOUT < MACHINE_STORAGE_PREPARE_BUDGET);
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
fn operation_scoped_running_evidence_precedes_the_first_storage_effect() {
    let state = tempfile::tempdir().unwrap();
    let operation_id = OperationId::try_new("op_storage_running_first").unwrap();
    let mut runner = RecordingRunner::new([failed("stop after observing running evidence")])
        .expect_running_evidence(state.path(), operation_id.clone());

    let _ = prepare_storage_for_operation(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &operation_id,
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    );

    assert!(runner.running_evidence.is_none());
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
fn imported_pool_list_rejects_truncated_output_as_pool_list_uncertainty() {
    let mut runner = RecordingRunner::new([truncated_stdout("ployz\n")]);

    assert!(matches!(
        imported_pools(&mut runner),
        Err(ZfsEffectError::PoolList { .. })
    ));
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
        pool_status(PLOYZ_OWNED_ZFS_POOL, PLOYZ_OWNED_ZFS_BACKING_FILE),
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
            .all(|call| call.program != "fallocate")
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
    assert!(!state.path().join("ployz-owned-zfs-import.service").exists());
    assert!(runner.invocations.iter().all(|invocation| {
        !(invocation.program == "systemctl"
            && invocation.args.first().is_some_and(|arg| arg == "enable"))
    }));
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
fn zero_pools_physically_allocate_half_the_filesystem_for_owned_pool() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout(""),
        failed("backing file absent"),
        success(),
        stdout("1B-blocks Avail\n25769803776 23622320128\n"),
        success(),
        success(),
        stdout("1B-blocks Avail\n25769803776 10737418240\n"),
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
    assert_eq!(invocation(&runner, 7).program, "dd");
    assert_eq!(
        invocation(&runner, 7).args,
        vec![
            "if=/dev/null".to_owned(),
            format!("of={PLOYZ_OWNED_ZFS_BACKING_FILE}"),
            "status=none".to_owned(),
            "conv=excl".to_owned()
        ]
    );
    assert_eq!(invocation(&runner, 8).program, "fallocate");
    assert_eq!(
        invocation(&runner, 8).args,
        vec!["-l", "12884901888", PLOYZ_OWNED_ZFS_BACKING_FILE]
    );
    assert_eq!(invocation(&runner, 10).program, "zpool");
    assert_eq!(
        invocation(&runner, 10).args,
        vec![
            "create",
            "-f",
            PLOYZ_OWNED_ZFS_POOL,
            PLOYZ_OWNED_ZFS_BACKING_FILE
        ]
    );
    assert!(!state.path().join("ployz-owned-zfs-import.service").exists());
    assert_eq!(
        std::fs::read_to_string(state.path().join("docker.service.d/ployz-zfs.conf")).unwrap(),
        "[Unit]\nAfter=zfs.target\n"
    );
    let [.., daemon_reload] = runner.invocations.as_slice() else {
        panic!("expected daemon-reload invocation");
    };
    assert_eq!(daemon_reload.args, vec!["daemon-reload"]);
}

#[test]
fn owned_preparation_persists_authority_before_ordering_failure() {
    let outputs = || {
        [
            success(),
            success(),
            success(),
            stdout(""),
            failed("backing file absent"),
            success(),
            stdout("1B-blocks Avail\n25769803776 23622320128\n"),
            success(),
            success(),
            stdout("1B-blocks Avail\n25769803776 10737418240\n"),
            success(),
            success(),
            failed("dataset absent"),
            success(),
            failed("dataset absent"),
            success(),
        ]
    };

    let drop_in_write = tempfile::tempdir().unwrap();
    let docker_drop_in = drop_in_write.path().join("docker.service.d");
    std::fs::create_dir_all(docker_drop_in.join("ployz-zfs.conf")).unwrap();
    let mut runner = RecordingRunner::new(outputs());
    assert!(matches!(
        prepare_storage(
            &mut runner,
            &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
            &PoolSelection::Automatic,
            drop_in_write.path(),
            &docker_drop_in,
        ),
        Err(ZfsEffectError::Dataset { .. })
    ));
    assert!(load_prepared_storage_state(drop_in_write.path()).is_ok());
    assert!(
        runner
            .invocations
            .iter()
            .all(|call| call.program != "systemctl")
    );

    let daemon_reload = tempfile::tempdir().unwrap();
    let mut reload_outputs = outputs().to_vec();
    reload_outputs.push(failed("daemon reload failed"));
    let mut runner = RecordingRunner::new(reload_outputs);
    assert!(matches!(
        prepare_storage(
            &mut runner,
            &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
            &PoolSelection::Automatic,
            daemon_reload.path(),
            &daemon_reload.path().join("docker.service.d"),
        ),
        Err(ZfsEffectError::Dataset { .. })
    ));
    assert!(load_prepared_storage_state(daemon_reload.path()).is_ok());
    let Some(reload_invocation) = runner.invocations.last() else {
        panic!("expected daemon-reload invocation");
    };
    assert_eq!(reload_invocation.args, vec!["daemon-reload"]);
}

#[test]
fn owned_pool_sizing_preserves_headroom_after_allocation_overhead() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout(""),
        failed("backing file absent"),
        success(),
        stdout("1B-blocks Avail\n24566575104 15852343296\n"),
        success(),
        success(),
        stdout("1B-blocks Avail\n24566575104 5369753600\n"),
        success(),
        success(),
        failed("dataset absent"),
        success(),
        failed("dataset absent"),
        success(),
        success(),
    ]);

    prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap();

    assert_eq!(
        invocation(&runner, 8).args,
        vec!["-l", "10482585600", PLOYZ_OWNED_ZFS_BACKING_FILE]
    );
}

#[test]
fn owned_pool_refuses_less_than_eight_gibibytes_after_headroom() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout(""),
        failed("backing file absent"),
        success(),
        stdout("1B-blocks Avail\n25769803776 12884901888\n"),
    ]);

    let failure = prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap_err();

    assert_eq!(
        failure,
        ZfsEffectError::OwnedPoolTooSmall {
            total_bytes: 25_769_803_776,
            available_bytes: 12_884_901_888,
            required_headroom_bytes: 5_368_709_120,
            minimum_pool_bytes: 8_589_934_592,
        }
    );
    assert_eq!(runner.invocations.len(), 7);
}

#[test]
fn automatic_prepare_preserves_unimported_owned_backing_evidence() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([success(), success(), success(), stdout(""), success()]);

    let failure = prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap_err();

    assert_eq!(
        failure,
        ZfsEffectError::OwnedPoolEvidencePresent {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
        }
    );
    assert_eq!(invocation(&runner, 4).program, "test");
    assert!(
        runner
            .invocations
            .iter()
            .all(|call| call.program != "fallocate" && call.program != "rm")
    );
}

#[test]
fn owned_pool_allocation_rechecks_headroom_and_removes_new_backing_file() {
    let state = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        success(),
        success(),
        stdout(""),
        failed("backing file absent"),
        success(),
        stdout("1B-blocks Avail\n25769803776 23622320128\n"),
        success(),
        success(),
        stdout("1B-blocks Avail\n25769803776 4294967296\n"),
        success(),
    ]);

    let failure = prepare_storage(
        &mut runner,
        &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
        &PoolSelection::Automatic,
        state.path(),
        &state.path().join("docker.service.d"),
    )
    .unwrap_err();

    assert_eq!(
        failure,
        ZfsEffectError::OwnedPoolHeadroomNotPreserved {
            available_bytes: 4_294_967_296,
            required_headroom_bytes: 5_368_709_120,
        }
    );
    assert_eq!(invocation(&runner, 10).program, "rm");
    assert_eq!(
        invocation(&runner, 10).args,
        vec!["-f", "--", PLOYZ_OWNED_ZFS_BACKING_FILE]
    );
    assert!(runner.invocations.iter().all(|call| {
        !(call.program == "zpool" && call.args.first().is_some_and(|arg| arg == "create"))
    }));
}

#[test]
fn failed_allocation_and_pool_create_remove_only_the_new_backing_file() {
    for outputs in [
        vec![
            success(),
            success(),
            success(),
            stdout(""),
            failed("backing file absent"),
            success(),
            stdout("1B-blocks Avail\n25769803776 23622320128\n"),
            success(),
            failed("allocation failed"),
            success(),
        ],
        failed_owned_pool_create_outputs([stdout(""), success()]),
    ] {
        let state = tempfile::tempdir().unwrap();
        let mut runner = RecordingRunner::new(outputs);

        assert!(matches!(
            prepare_storage(
                &mut runner,
                &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
                &PoolSelection::Automatic,
                state.path(),
                &state.path().join("docker.service.d"),
            ),
            Err(ZfsEffectError::OwnedPool { .. })
        ));
        let cleanup = runner.invocations.last().expect("cleanup invocation");
        assert_eq!(cleanup.program, "rm");
        assert_eq!(cleanup.args, vec!["-f", "--", PLOYZ_OWNED_ZFS_BACKING_FILE]);
    }
}

#[test]
fn failed_pool_create_cleanup_requires_conclusive_unused_observation() {
    let command = |program: &str, args: &[&str]| Invocation {
        program: program.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        timeout: COMMAND_TIMEOUT,
    };
    let create = || {
        command(
            "zpool",
            &[
                "create",
                "-f",
                PLOYZ_OWNED_ZFS_POOL,
                PLOYZ_OWNED_ZFS_BACKING_FILE,
            ],
        )
    };
    let list = || command("zpool", &["list", "-H", "-o", "name"]);
    let status = || command("zpool", &["status", "-P", PLOYZ_OWNED_ZFS_POOL]);
    let remove = || command("rm", &["-f", "--", PLOYZ_OWNED_ZFS_BACKING_FILE]);
    let cases = [
        (
            "canonical backing vdev",
            vec![
                stdout("ployz\n"),
                stdout(&format!(
                    "config:\n\nNAME STATE READ WRITE CKSUM\n{PLOYZ_OWNED_ZFS_POOL} ONLINE 0 0 0\n  mirror-0 ONLINE 0 0 0\n    {PLOYZ_OWNED_ZFS_BACKING_FILE} ONLINE 0 0 0\n    /dev/loop1 ONLINE 0 0 0\n\nerrors: No known data errors\n"
                )),
            ],
            vec![create(), list(), status()],
            Some("backing file retained"),
        ),
        (
            "failed pool listing",
            vec![failed("pool observation failed")],
            vec![create(), list()],
            Some("ownership observation was inconclusive"),
        ),
        (
            "malformed pool identity",
            vec![stdout("ployz\ninvalid/pool/name\n")],
            vec![create(), list()],
            Some("ownership observation was inconclusive"),
        ),
        (
            "truncated pool listing",
            vec![truncated_stdout("ployz\n")],
            vec![create(), list()],
            Some("ownership observation was inconclusive"),
        ),
        (
            "empty successful status",
            vec![stdout("ployz\n"), stdout("")],
            vec![create(), list(), status()],
            Some("ownership observation was inconclusive"),
        ),
        (
            "malformed successful status",
            vec![stdout("ployz\n"), stdout("pool: ployz\nstate: ONLINE\n")],
            vec![create(), list(), status()],
            Some("ownership observation was inconclusive"),
        ),
        (
            "wrong canonical pool row",
            vec![
                stdout("ployz\n"),
                stdout(
                    "config:\n\nNAME STATE READ WRITE CKSUM\nother ONLINE 0 0 0\n/dev/loop0 ONLINE 0 0 0\n",
                ),
            ],
            vec![create(), list(), status()],
            Some("ownership observation was inconclusive"),
        ),
        (
            "root-only successful status",
            vec![
                stdout("ployz\n"),
                stdout(
                    "config:\n\nNAME STATE READ WRITE CKSUM\nployz ONLINE 0 0 0\n\nerrors: No known data errors\n",
                ),
            ],
            vec![create(), list(), status()],
            Some("ownership observation was inconclusive"),
        ),
        (
            "malformed vdev row",
            vec![
                stdout("ployz\n"),
                stdout(
                    "config:\n\nNAME STATE READ WRITE CKSUM\nployz ONLINE 0 0 0\n/dev/loop0 ONLINE 0\n\nerrors: No known data errors\n",
                ),
            ],
            vec![create(), list(), status()],
            Some("ownership observation was inconclusive"),
        ),
        (
            "truncated status",
            vec![
                stdout("ployz\n"),
                truncated_stdout("config:\n\nNAME STATE READ WRITE CKSUM\nployz ONLINE 0 0 0\n"),
            ],
            vec![create(), list(), status()],
            Some("ownership observation was inconclusive"),
        ),
        (
            "other canonical-pool vdev",
            vec![
                stdout("ployz\n"),
                pool_status(PLOYZ_OWNED_ZFS_POOL, "/dev/loop0"),
                success(),
            ],
            vec![create(), list(), status(), remove()],
            None,
        ),
    ];

    for (name, observation, expected_commands, evidence) in cases {
        let state = tempfile::tempdir().unwrap();
        let mut runner = RecordingRunner::new(failed_owned_pool_create_outputs(observation));

        let failure = prepare_storage(
            &mut runner,
            &profile("ID=ubuntu\nVERSION_ID=24.04\n"),
            &PoolSelection::Automatic,
            state.path(),
            &state.path().join("docker.service.d"),
        )
        .unwrap_err();

        let ZfsEffectError::OwnedPool { message } = failure else {
            panic!("{name}: expected owned-pool failure")
        };
        match evidence {
            Some(evidence) => {
                assert!(message.contains("pool create failed"), "{name}: {message}");
                assert!(message.contains(evidence), "{name}: {message}");
            }
            None => assert_eq!(message, "pool create failed", "{name}"),
        }
        assert_eq!(
            runner
                .invocations
                .get(10..)
                .expect("create-failure command suffix"),
            expected_commands,
            "{name}"
        );
    }
}

#[test]
fn owned_pool_failure_reads_legacy_sparse_pool_evidence() {
    let failure: ZfsEffectError =
        serde_json::from_str(r#"{"kind":"sparse_pool","message":"legacy preparation failure"}"#)
            .unwrap();

    assert_eq!(
        failure,
        ZfsEffectError::OwnedPool {
            message: "legacy preparation failure".to_owned(),
        }
    );
    let serialized = serde_json::to_value(failure).unwrap();
    assert_eq!(serialized.get("kind").unwrap(), "owned_pool");
}

#[test]
fn owned_origin_survives_restart_and_selects_zpool_capacity() {
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
        pool_status(PLOYZ_OWNED_ZFS_POOL, PLOYZ_OWNED_ZFS_BACKING_FILE),
        stdout("8192 4096\n"),
        stdout("1024\n"),
        stdout("ployz/ployz/volumes\tnone\n"),
    ]);
    let facts = gather_pool_capacity(&mut runner, state.path()).unwrap();
    assert_eq!(facts.total_bytes, 8192);
    assert_eq!(facts.provisioned_used_bytes, 1024);
    assert_eq!(facts.free_bytes, 4096);
    assert_eq!(invocation(&runner, 3).program, "zpool");
    assert_eq!(
        invocation(&runner, 3).args,
        vec!["list", "-H", "-p", "-o", "size,free", PLOYZ_OWNED_ZFS_POOL]
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
        pool_status(PLOYZ_OWNED_ZFS_POOL, PLOYZ_OWNED_ZFS_BACKING_FILE),
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
            .all(|call| call.program != "fallocate")
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
        stdout("16384 8192\n"),
        stdout("2048\n"),
        stdout("tank/ployz/volumes\tnone\n"),
    ]);
    let facts = gather_pool_capacity(&mut runner, state.path()).unwrap();
    assert_eq!(facts.total_bytes, 16_384);
    assert_eq!(facts.provisioned_used_bytes, 2048);
    assert_eq!(facts.free_bytes, 8192);
    assert_eq!(invocation(&runner, 2).program, "zpool");
    assert_eq!(
        invocation(&runner, 2).args,
        vec!["list", "-H", "-p", "-o", "size,free", "tank"]
    );
}

#[test]
fn pool_capacity_parses_and_orders_direct_child_quotas() {
    let state = tempfile::tempdir().expect("temporary state");
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let alpha = DatasetName::for_volume(
        &ZfsPoolName::try_new("tank").expect("pool"),
        &CorrosionNamespaceName::try_new("alpha").expect("namespace"),
        &VolumeName::try_new("data").expect("volume"),
    )
    .expect("dataset");
    let zeta = DatasetName::for_volume(
        &ZfsPoolName::try_new("tank").expect("pool"),
        &CorrosionNamespaceName::try_new("zeta").expect("namespace"),
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
        stdout("16384 8192\n"),
        stdout("3072\n"),
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
    assert_eq!(facts.provisioned_used_bytes, 3072);
}

#[test]
fn pool_capacity_rejects_invalid_child_quota_rows() {
    let state = tempfile::tempdir().expect("temporary state");
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("16384 8192\n"),
        stdout("0\n"),
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
fn dataset_ensure_creates_with_quota_without_creating_mountpoint() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("4096 4096\n"),
        stdout("0\n"),
        stdout("tank/ployz/volumes\tnone\n"),
        success(),
    ]);
    ensure_dataset(
        &mut runner,
        state.path(),
        &dataset("tank"),
        VolumeMaxSizeBytes::try_new(1024).unwrap(),
    )
    .unwrap();
    assert_eq!(invocation(&runner, 5).program, "zfs");
    assert_eq!(
        invocation(&runner, 5).args.first().map(String::as_str),
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
    let rows = format!(
        "tank/ployz/volumes\tnone\n{}\t2048\n",
        dataset("tank").as_str()
    );
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("4096 4096\n"),
        stdout("0\n"),
        stdout(&rows),
        mounted_dataset_shape(&dataset("tank")),
    ]);
    let error = ensure_dataset(
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
    assert_eq!(runner.invocations.len(), 6);
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
        stdout("5120 4096\n"),
        stdout("1024\n"),
        stdout(&rows),
        mounted_dataset_shape(&dataset),
        success(),
    ]);
    ensure_dataset(
        &mut runner,
        state.path(),
        &dataset,
        VolumeMaxSizeBytes::try_new(2048).unwrap(),
    )
    .unwrap();
    assert_eq!(
        invocation(&runner, 6).args,
        vec!["set", "quota=2048", dataset.as_str()]
    );
    assert_eq!(
        invocation(&runner, 5).args,
        vec![
            "get",
            "-H",
            "-o",
            "value",
            "mountpoint,mounted",
            dataset.as_str(),
        ]
    );

    let equal_rows = format!("tank/ployz/volumes\tnone\n{}\t2048\n", dataset.as_str());
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("5120 4096\n"),
        stdout("1024\n"),
        stdout(&equal_rows),
        mounted_dataset_shape(&dataset),
    ]);
    ensure_dataset(
        &mut runner,
        state.path(),
        &dataset,
        VolumeMaxSizeBytes::try_new(2048).unwrap(),
    )
    .unwrap();
    assert_eq!(runner.invocations.len(), 6);
}

#[test]
fn equal_dataset_quota_rejects_a_noncanonical_mountpoint() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let dataset = dataset("tank");
    let rows = format!("tank/ployz/volumes\tnone\n{}\t2048\n", dataset.as_str());
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("5120 4096\n"),
        stdout("1024\n"),
        stdout(&rows),
        stdout("/wrong/path\nyes\n"),
    ]);

    let error = ensure_dataset(
        &mut runner,
        state.path(),
        &dataset,
        VolumeMaxSizeBytes::try_new(2048).unwrap(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ZfsEffectError::PreparedStateMismatch { .. }
    ));
    assert_eq!(runner.invocations.len(), 6);
}

#[test]
fn dataset_quota_growth_rejects_an_unmounted_canonical_child() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let dataset = dataset("tank");
    let rows = format!("tank/ployz/volumes\tnone\n{}\t1024\n", dataset.as_str());
    let leaf = dataset.as_str().rsplit('/').next().unwrap();
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("5120 4096\n"),
        stdout("1024\n"),
        stdout(&rows),
        stdout(&format!("{PROVISIONED_VOLUME_MOUNTPOINT}/{leaf}\nno\n")),
    ]);

    let error = ensure_dataset(
        &mut runner,
        state.path(),
        &dataset,
        VolumeMaxSizeBytes::try_new(2048).unwrap(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ZfsEffectError::PreparedStateMismatch { .. }
    ));
    assert_eq!(runner.invocations.len(), 6);
}

#[test]
fn dataset_quota_admission_rejects_total_above_available_capacity() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("200 100\n"),
        stdout("0\n"),
        stdout("tank/ployz/volumes\tnone\n"),
    ]);
    let error = ensure_dataset(
        &mut runner,
        state.path(),
        &dataset("tank"),
        VolumeMaxSizeBytes::try_new(101).unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ZfsEffectError::QuotaCapacityExceeded {
            total_bytes: 200,
            provisioned_used_bytes: 0,
            free_bytes: 100,
            required_headroom_bytes: 101,
            requested_total_bytes: 101,
        }
    );
    assert_eq!(runner.invocations.len(), 5);
}

#[test]
fn dataset_ensure_lock_serializes_one_prepared_pool() {
    use std::fs::OpenOptions;
    use std::sync::mpsc;

    let state = tempfile::tempdir().unwrap();
    let path = dataset_ensure_lock_path(state.path(), "tank");
    let holder = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    holder.lock().unwrap();
    let state_path = state.path().to_owned();
    let (sender, receiver) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let lock = DatasetEnsureLock::acquire(&state_path, "tank").unwrap();
        sender.send(()).unwrap();
        drop(lock);
    });

    assert!(receiver.recv_timeout(Duration::from_millis(75)).is_err());
    holder.unlock().unwrap();
    receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    waiter.join().unwrap();
}

#[test]
fn dataset_facts_use_zfs_used_bytes_and_latest_recursive_entry_time() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("4096\n"),
        stdout("1699999999.2500000000\n1700000000.7500000000\n"),
    ]);

    let facts = gather_dataset_facts(&mut runner, state.path(), &dataset("tank")).unwrap();

    assert_eq!(
        facts,
        VolumeUsageFacts {
            used_bytes: 4096,
            last_write_unix_seconds: 1_700_000_000,
        }
    );
    assert_eq!(invocation(&runner, 3).program, "find");
    let expected_path = format!(
        "/var/lib/ployz/volumes/{}",
        dataset("tank").as_str().rsplit('/').next().unwrap()
    );
    assert_eq!(
        invocation(&runner, 3).args,
        vec![
            expected_path,
            "-xdev".to_owned(),
            "-printf".to_owned(),
            "%T@\n".to_owned(),
        ]
    );
}

#[test]
fn dataset_facts_reject_malformed_or_empty_recursive_timestamps() {
    for timestamps in ["not-a-time\n", ""] {
        let state = tempfile::tempdir().unwrap();
        persist(state.path(), PreparedStorageOrigin::Adopted);
        let mut runner = RecordingRunner::new([
            success(),
            stdout(VOLUME_MOUNTPOINT),
            stdout("4096\n"),
            stdout(timestamps),
        ]);

        assert!(matches!(
            gather_dataset_facts(&mut runner, state.path(), &dataset("tank")),
            Err(ZfsEffectError::GatherParse { .. })
        ));
    }
}

#[test]
fn dataset_facts_reject_truncated_recursive_timestamps() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("4096\n"),
        truncated_stdout("1700000000.1\n"),
    ]);

    assert!(matches!(
        gather_dataset_facts(&mut runner, state.path(), &dataset("tank")),
        Err(ZfsEffectError::GatherParse { message })
            if message.contains("exceeded the capture limit")
    ));
}

#[test]
fn destroy_lists_direct_children_then_destroys_present_exact_dataset_non_recursively() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let dataset = dataset("tank");
    let listing = format!("tank/ployz/volumes\n{}\n", dataset.as_str());
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout(&listing),
        success(),
    ]);
    destroy_dataset(&mut runner, state.path(), &dataset).unwrap();
    assert_eq!(
        invocation(&runner, 2),
        &Invocation {
            program: "zfs".to_owned(),
            args: vec![
                "list".to_owned(),
                "-H".to_owned(),
                "-d".to_owned(),
                "1".to_owned(),
                "-o".to_owned(),
                "name".to_owned(),
                "tank/ployz/volumes".to_owned(),
            ],
            timeout: DATASET_DESTROY_INNER_COMMAND_TIMEOUT,
        }
    );
    assert_eq!(
        invocation(&runner, 3),
        &Invocation {
            program: "zfs".to_owned(),
            args: vec!["destroy".to_owned(), dataset.as_str().to_owned()],
            timeout: DATASET_DESTROY_INNER_COMMAND_TIMEOUT,
        }
    );
    assert!(
        runner
            .invocations
            .iter()
            .all(|invocation| invocation.timeout == DATASET_DESTROY_INNER_COMMAND_TIMEOUT)
    );
}

#[test]
fn owned_image_destroy_uses_five_inner_commands_with_the_destroy_budget() {
    let state = tempfile::tempdir().unwrap();
    persist(
        state.path(),
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
        },
    );
    let dataset = dataset(PLOYZ_OWNED_ZFS_POOL);
    let root = format!("{PLOYZ_OWNED_ZFS_POOL}/ployz/volumes");
    let listing = format!("{root}\n{}\n", dataset.as_str());
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        pool_status(PLOYZ_OWNED_ZFS_POOL, PLOYZ_OWNED_ZFS_BACKING_FILE),
        stdout(&listing),
        success(),
    ]);

    destroy_dataset(&mut runner, state.path(), &dataset).unwrap();

    assert_eq!(
        runner.invocations.len(),
        ployz_core::storage::DATASET_DESTROY_MAX_INNER_COMMANDS as usize
    );
    assert!(
        runner
            .invocations
            .iter()
            .all(|invocation| invocation.timeout == DATASET_DESTROY_INNER_COMMAND_TIMEOUT)
    );
}

#[test]
fn destroy_treats_valid_listing_omission_as_no_op() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([
        success(),
        stdout(VOLUME_MOUNTPOINT),
        stdout("tank/ployz/volumes\n"),
    ]);

    destroy_dataset(&mut runner, state.path(), &dataset("tank")).unwrap();

    assert_eq!(runner.invocations.len(), 3);
    assert_eq!(
        invocation(&runner, 2).args.first().map(String::as_str),
        Some("list")
    );
}

#[test]
fn destroy_rejects_wrong_root_before_listing_or_destroy() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([success(), stdout(VOLUME_MOUNTPOINT)]);

    assert!(matches!(
        destroy_dataset(&mut runner, state.path(), &dataset("other")),
        Err(ZfsEffectError::DestructiveEffect { .. })
    ));

    assert_eq!(runner.invocations.len(), 2);
}

#[test]
fn destroy_retains_failed_malformed_foreign_and_ambiguous_listing_testimony() {
    let requested = dataset("tank");
    let foreign = dataset("other");
    let cases = [
        (
            failed("listing failed"),
            "failed",
            ZfsEffectError::DestructiveEffect {
                message: "listing failed".to_owned(),
            },
        ),
        (
            stdout("tank/ployz/volumes\n\n"),
            "malformed",
            ZfsEffectError::GatherParse {
                message: "invalid direct-child dataset row \"\"".to_owned(),
            },
        ),
        (
            stdout(&format!("tank/ployz/volumes\n{}\n", foreign.as_str())),
            "foreign",
            ZfsEffectError::DestructiveEffect {
                message: format!(
                    "dataset {} is not a direct child of tank/ployz/volumes",
                    foreign.as_str()
                ),
            },
        ),
        (
            stdout(&format!(
                "tank/ployz/volumes\n{}\n{}\n",
                requested.as_str(),
                requested.as_str()
            )),
            "ambiguous",
            ZfsEffectError::GatherParse {
                message: format!(
                    "duplicate direct-child dataset row {:?}",
                    requested.as_str()
                ),
            },
        ),
        (
            stdout(&format!(
                "tank/ployz/volumes\n{}\ntank/ployz/volumes\n",
                requested.as_str()
            )),
            "duplicate-root",
            ZfsEffectError::GatherParse {
                message: "duplicate direct-child dataset root row \"tank/ployz/volumes\""
                    .to_owned(),
            },
        ),
        (
            stdout(requested.as_str()),
            "missing-root",
            ZfsEffectError::GatherParse {
                message: "direct-child dataset listing omitted root tank/ployz/volumes".to_owned(),
            },
        ),
        (
            truncated_stdout("tank/ployz/volumes\n"),
            "capture-limited",
            ZfsEffectError::GatherParse {
                message: "direct-child dataset listing exceeded the command output capture limit"
                    .to_owned(),
            },
        ),
    ];

    for (listing, label, expected) in cases {
        let state = tempfile::tempdir().unwrap();
        persist(state.path(), PreparedStorageOrigin::Adopted);
        let mut runner = RecordingRunner::new([success(), stdout(VOLUME_MOUNTPOINT), listing]);

        assert_eq!(
            destroy_dataset(&mut runner, state.path(), &requested),
            Err(expected),
            "{label} listing"
        );
        assert_eq!(runner.invocations.len(), 3, "{label} listing");
    }
}

#[test]
fn destroy_validates_but_does_not_deduplicate_unrelated_siblings() {
    let state = tempfile::tempdir().unwrap();
    persist(state.path(), PreparedStorageOrigin::Adopted);
    let requested = dataset("tank");
    let sibling = DatasetName::for_volume(
        &ZfsPoolName::try_new("tank").unwrap(),
        &CorrosionNamespaceName::try_new("default").unwrap(),
        &VolumeName::try_new("logs").unwrap(),
    )
    .unwrap();
    let listing = format!(
        "tank/ployz/volumes\n{}\n{}\n",
        sibling.as_str(),
        sibling.as_str()
    );
    let mut runner = RecordingRunner::new([success(), stdout(VOLUME_MOUNTPOINT), stdout(&listing)]);

    destroy_dataset(&mut runner, state.path(), &requested).unwrap();

    assert_eq!(runner.invocations.len(), 3);
}
