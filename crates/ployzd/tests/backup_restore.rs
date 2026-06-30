use ployz_core::backup::{
    BackupArtifactKind, BackupArtifactLocation, BackupBundle, BackupManifest, BackupRestoreSource,
    BackupTarget, BackupTargetValidationFailure, BackupTargetValidationField,
    ControlPlaneKvSnapshot, S3AddressingStyle, S3BackupRestoreSource, S3BackupTarget,
};
use ployz_core::install::MachineBootstrapUrl;
use ployz_core::ops::{
    BackupOperationState, OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationStatus,
};
use ployz_core::state::{ActiveServiceCommitRequest, ExpectedActiveService};
use ployz_core::subjects::OperationApiEndpoint;
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_nats::operations::{AsyncNatsOperationEventLog, AsyncNatsOperationStatusStore};
use ployz_sdk_types::{BackupCreateError, BackupCreateRequest, OpsStatusError, OpsStatusRequest};
use ployz_test_support::ids::{event_sequence, machine_id, operation_id, revision_id, service_id};
use ployz_test_support::ops::wait_for_terminal_status;
use ployzd::backup_adapters::{BackupAdapterError, InMemoryBackupAdapter, backup_object_key};
use ployzd::backup_restore::{BackupRestoreError, BackupRestoreRuntime, RestoreObservationState};
use ployzd::config::{ControlProcessConfig, DEFAULT_MACHINE_BOOTSTRAP_URL};
use ployzd::controllers::{BackupCreateCommand, MachineAddBootstrapConfig, OperationControllers};
use ployzd::nats_process::NatsServerRuntime;
use std::time::Duration;

#[tokio::test]
async fn backup_create_is_a_durable_operation_against_real_control_runtime() {
    let nats = TestNats::start().await;
    let backups = InMemoryBackupAdapter::default();
    let runtime = ployzd::control_runtime::start_control_runtime_with_client_and_backup_adapters(
        nats.client.clone(),
        &config(),
        backups.registry(),
    )
    .await
    .expect("control runtime starts");
    let api = OperationApiClient::new(nats.user_client.clone());
    let target = backup_target("clusters/dev");

    let accepted = api
        .backup_create(&BackupCreateRequest {
            operation_id: operation_id("op_backup"),
            target: target.clone(),
        })
        .await
        .expect("backup create is accepted");

    assert_eq!(accepted.operation_id, operation_id("op_backup"));
    assert_eq!(accepted.start_sequence, event_sequence(1));
    let status =
        wait_for_terminal_status(&api, &operation_id("op_backup"), Duration::from_secs(4)).await;
    let manifest = completed_backup_manifest(&status, "op_backup");
    assert_eq!(
        manifest.format_version,
        BackupManifest::current_control_plane_kv_only().format_version
    );
    assert_eq!(
        manifest.scope,
        BackupManifest::current_control_plane_kv_only().scope
    );
    assert_eq!(
        manifest.restore_contract,
        BackupManifest::current_control_plane_kv_only().restore_contract
    );
    let [artifact] = manifest.artifacts.as_slice() else {
        panic!("expected one backup artifact");
    };
    assert_backup_artifact_exists(&backups, artifact);
    assert_eq!(
        backups.writes(),
        vec![
            "clusters/dev/op_backup/control-plane-bundle.json".to_owned(),
            "clusters/dev/op_backup/manifest.json".to_owned(),
        ]
    );
    let bundle = read_backup_bundle(&backups, artifact).await;
    assert_snapshot_contains_control_buckets(&bundle.control_plane);
    assert_eq!(status.last_event_sequence(), event_sequence(4));

    let events = api
        .ops_watch(&OperationEventReplayRequest {
            operation_id: operation_id("op_backup"),
            start_sequence: event_sequence(1),
            limit: OperationEventReplayLimit::try_new(10).expect("valid replay limit"),
        })
        .await
        .expect("events replay");
    let [submitted, snapshotting, writing_manifest, completed] = events.events.as_slice() else {
        panic!("expected backup lifecycle events");
    };
    assert_eq!(
        submitted.event,
        OperationEvent::BackupCreateSubmitted {
            operation_id: operation_id("op_backup"),
            target,
        }
    );
    assert!(matches!(
        snapshotting.event,
        OperationEvent::BackupRunning { .. }
    ));
    assert!(matches!(
        writing_manifest.event,
        OperationEvent::BackupRunning { .. }
    ));
    assert!(matches!(
        completed.event,
        OperationEvent::BackupCompleted { .. }
    ));
    assert_eq!(events.cursor, OperationEventReplayCursor::Terminal);

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn backup_create_rejects_empty_s3_target_before_recording_operation() {
    let nats = TestNats::start().await;
    let backups = InMemoryBackupAdapter::default();
    let runtime = ployzd::control_runtime::start_control_runtime_with_client_and_backup_adapters(
        nats.client.clone(),
        &config(),
        backups.registry(),
    )
    .await
    .expect("control runtime starts");
    let api = OperationApiClient::new(nats.user_client.clone());
    let operation_id = operation_id("op_bad_backup");

    let error = api
        .backup_create(&BackupCreateRequest {
            operation_id: operation_id.clone(),
            target: BackupTarget::s3(S3BackupTarget::new(
                "",
                "clusters/dev",
                "us-east-1",
                None,
                S3AddressingStyle::VirtualHosted,
            )),
        })
        .await
        .expect_err("empty bucket is rejected");

    assert_eq!(
        error,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::BackupCreate,
            error: BackupCreateError::InvalidTarget {
                operation_id: operation_id.clone(),
                field: BackupTargetValidationField::Bucket,
                failure: BackupTargetValidationFailure::Empty,
            },
        }
    );
    assert!(backups.writes().is_empty());
    let status_error = api
        .ops_status(&OpsStatusRequest {
            operation_id: operation_id.clone(),
        })
        .await
        .expect_err("invalid request leaves no operation status");
    assert!(matches!(
        status_error,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::OpsStatus,
            error: OpsStatusError::NoSuchOperation { operation_id: missing },
        } if missing == operation_id
    ));

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_does_not_resume_seeded_backup_without_accepting_command() {
    let nats = TestNats::start().await;
    assure_control_resources(&nats).await;
    let backups = InMemoryBackupAdapter::default();
    let seed = seed_controllers(&nats).await;
    seed.submit_backup(BackupCreateCommand {
        operation_id: operation_id("op_recovered_backup"),
        target: backup_target("clusters/dev"),
    })
    .await
    .expect("accepted backup is seeded");

    let runtime = ployzd::control_runtime::start_control_runtime_with_client_and_backup_adapters(
        nats.client.clone(),
        &config(),
        backups.registry(),
    )
    .await
    .expect("control runtime starts");
    let api = OperationApiClient::new(nats.user_client.clone());

    let status = api
        .ops_status(&OpsStatusRequest {
            operation_id: operation_id("op_recovered_backup"),
        })
        .await
        .expect("backup status reads")
        .status;
    assert!(matches!(
        status,
        OperationStatus::Backup {
            state: BackupOperationState::Accepted,
            ..
        }
    ));
    assert!(backups.writes().is_empty());

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn backup_restore_recreates_single_core_control_plane_kv_state() {
    let source = TestNats::start().await;
    let backups = InMemoryBackupAdapter::default();
    let runtime = ployzd::control_runtime::start_control_runtime_with_client_and_backup_adapters(
        source.client.clone(),
        &config(),
        backups.registry(),
    )
    .await
    .expect("control runtime starts");
    let source_core = AsyncNatsCoreStateStore::from_jetstream(&source.jetstream)
        .await
        .expect("source core state opens");
    source_core
        .commit_active_service(&ActiveServiceCommitRequest {
            service_id: service_id("svc_api"),
            expected_current: ExpectedActiveService::Absent,
            target_revision: revision_id("rev_2"),
        })
        .await
        .expect("active service stores before backup");
    let api = OperationApiClient::new(source.user_client.clone());
    api.backup_create(&BackupCreateRequest {
        operation_id: operation_id("op_backup_restore"),
        target: backup_target("clusters/dev"),
    })
    .await
    .expect("backup create is accepted");
    let source_status = wait_for_terminal_status(
        &api,
        &operation_id("op_backup_restore"),
        Duration::from_secs(4),
    )
    .await;
    let source_manifest = completed_backup_manifest(&source_status, "op_backup_restore");
    let [artifact] = source_manifest.artifacts.as_slice() else {
        panic!("expected one backup artifact");
    };
    assert_backup_artifact_digest_mismatch_rejected(&backups, artifact).await;
    runtime
        .shutdown()
        .await
        .expect("source control runtime shuts down");

    let target = TestNats::start().await;
    assure_control_resources(&target).await;
    let restore = BackupRestoreRuntime::new(target.jetstream.clone(), backups.registry());

    let report = restore
        .restore_source(&backup_restore_source("clusters/dev", "op_backup_restore"))
        .await
        .expect("control-plane source restores");

    assert_eq!(report.buckets.len(), 1);
    assert!(matches!(
        report.observations,
        RestoreObservationState::RebuildableAfterMachineReconnect { .. }
    ));
    let target_core = AsyncNatsCoreStateStore::from_jetstream(&target.jetstream)
        .await
        .expect("target core state opens");
    assert_eq!(
        target_core
            .active_service(&service_id("svc_api"))
            .await
            .expect("restored active service reads")
            .expect("restored active service exists")
            .active_revision,
        revision_id("rev_2")
    );
    let target_controllers = seed_controllers(&target).await;
    assert!(
        target_controllers
            .repository()
            .records()
            .get(&operation_id("op_backup_restore"))
            .await
            .expect("restored backup status reads")
            .is_none()
    );

    let duplicate_restore = restore
        .restore_source(&backup_restore_source("clusters/dev", "op_backup_restore"))
        .await
        .expect_err("restore refuses a non-empty target");
    assert!(matches!(
        duplicate_restore,
        BackupRestoreError::DestinationNotEmpty { .. }
    ));
}

fn config() -> ControlProcessConfig {
    ControlProcessConfig::new(
        NatsServerRuntime::External(
            NatsClientUrl::try_new("nats://127.0.0.1:4222").expect("valid nats url"),
        ),
        machine_id("core_1"),
        ployz_nats::connect::NatsConnectConfig {
            url: NatsClientUrl::try_new("nats://127.0.0.1:4222").expect("valid nats url"),
            auth: ployz_nats::connect::NatsClientAuth::NkeySeed(
                ployz_core::nats_config::NatsUserSeed::try_new(
                    "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM",
                )
                .expect("test seed is valid"),
            ),
            trust: ployz_nats::connect::NatsTlsTrust::ClusterCa("/tmp/ployz-test-ca.pem".into()),
            principal: ployz_core::security::NatsPrincipal::Controller,
        },
    )
    .with_machine_bootstrap(machine_bootstrap_config())
}

fn machine_bootstrap_config() -> MachineAddBootstrapConfig {
    MachineAddBootstrapConfig::new(
        MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
            .expect("default bootstrap URL is valid"),
    )
    .with_join_material(
        ployz_test_support::fixtures::machine_join_template(),
        ployz_core::install::MachineJoinSecretDelivery {
            nats_credentials: ployz_core::nats_config::NatsUserSeed::try_new(
                "SUAFKRGZQV3CDWR46WYP6WR43T34AL5BN4BAGVGIP34YFSBESCD6FU4HHA",
            )
            .expect("valid seed"),
        },
    )
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    /// Controller principal: the control-runtime side.
    client: async_nats::Client,
    /// User principal: the operator driving API commands.
    user_client: async_nats::Client,
    jetstream: async_nats::jetstream::Context,
}

impl TestNats {
    async fn start() -> Self {
        let nats = ployz_test_support::nats::TestNats::start().await;
        let client = nats.controller.clone();
        let user_client = nats.user.clone();
        let jetstream = nats.jetstream.clone();

        Self {
            _nats: nats,
            client,
            user_client,
            jetstream,
        }
    }
}

async fn assure_control_resources(nats: &TestNats) {
    nats._nats.bootstrap_resources().await;
}

async fn seed_controllers(nats: &TestNats) -> OperationControllers {
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("status store opens");
    OperationControllers::new(
        event_log,
        status_store,
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
    )
}

fn completed_backup_manifest(status: &OperationStatus, expected_id: &str) -> BackupManifest {
    let OperationStatus::Backup {
        id,
        state: BackupOperationState::Completed { manifest },
        ..
    } = status
    else {
        panic!("expected completed backup status");
    };
    assert_eq!(id, &operation_id(expected_id));

    manifest.clone()
}

fn assert_backup_artifact_exists(
    backups: &InMemoryBackupAdapter,
    artifact: &ployz_core::backup::BackupArtifact,
) {
    let BackupArtifactLocation::S3 { key, .. } = &artifact.location;
    let payload = backups.object(key).expect("backup artifact object exists");
    assert_eq!(artifact.kind, BackupArtifactKind::ControlPlaneBundle);
    assert_eq!(payload.len() as u64, artifact.byte_count);
}

async fn assert_backup_artifact_digest_mismatch_rejected(
    backups: &InMemoryBackupAdapter,
    artifact: &ployz_core::backup::BackupArtifact,
) {
    let mut mismatched = artifact.clone();
    mismatched.sha256_digest = "not-the-recorded-digest".to_owned();

    assert!(matches!(
        backups.registry().read_artifact(&mismatched).await,
        Err(BackupAdapterError::ArtifactMismatch { .. })
    ));
}

async fn read_backup_bundle(
    backups: &InMemoryBackupAdapter,
    artifact: &ployz_core::backup::BackupArtifact,
) -> BackupBundle {
    let payload = backups
        .registry()
        .read_artifact(artifact)
        .await
        .expect("backup artifact body is readable");

    serde_json::from_slice(&payload).expect("backup artifact is a control-plane bundle")
}

fn assert_snapshot_contains_control_buckets(snapshot: &ControlPlaneKvSnapshot) {
    let bucket_names = snapshot
        .buckets
        .iter()
        .map(|bucket| bucket.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(bucket_names, vec!["KV_CORE"]);
}

fn backup_target(prefix: &str) -> BackupTarget {
    BackupTarget::s3(S3BackupTarget::new(
        "ployz-backups",
        prefix,
        "us-east-1",
        None,
        S3AddressingStyle::VirtualHosted,
    ))
}

fn backup_restore_source(prefix: &str, operation: &str) -> BackupRestoreSource {
    BackupRestoreSource::s3(S3BackupRestoreSource::new(
        "ployz-backups",
        backup_object_key(prefix, &operation_id(operation), "manifest.json"),
        "us-east-1",
        None,
        S3AddressingStyle::VirtualHosted,
    ))
}
