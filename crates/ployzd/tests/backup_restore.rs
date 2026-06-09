use ployz_core::backup::{
    BackupArtifactKind, BackupBundle, BackupManifest, ControlPlaneKvSnapshot,
};
use ployz_core::ids::{NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId};
use ployz_core::install::{MachineBootstrapUrl, MachineJoinTemplate};
use ployz_core::ops::{
    BackupOperationState, EventSequence, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayLimit, OperationEventReplayRequest, OperationIdempotencyKey,
    OperationStatus,
};
use ployz_core::state::{ActiveServiceCommitRequest, ExpectedActiveService};
use ployz_nats::bootstrap::{BootstrapPlan, assure_nats_resources};
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::objects::{
    AsyncNatsBackupObjectStore, BackupObjectStoreError, PLZ_BACKUPS_BUCKET,
    control_plane_bundle_object_name,
};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_nats::operations::{AsyncNatsOperationEventLog, AsyncNatsOperationStatusStore};
use ployz_sdk_types::{BackupCreateRequest, OpsStatusRequest};
use ployzd::backup_runtime::{BackupRestoreError, BackupRestoreRuntime, RestoreObservationState};
use ployzd::config::{ControlProcessConfig, DEFAULT_MACHINE_BOOTSTRAP_URL};
use ployzd::controllers::{BackupCreateCommand, MachineAddBootstrapConfig, OperationControllers};
use ployzd::nats_process::NatsServerRuntime;

#[tokio::test]
async fn backup_create_is_a_durable_operation_against_real_control_runtime() {
    let nats = TestNats::start().await;
    let runtime =
        ployzd::control_runtime::start_control_runtime_with_client(nats.client.clone(), &config())
            .await
            .expect("control runtime starts");
    let api = OperationApiClient::new(nats.client.clone());

    let accepted = api
        .backup_create(&BackupCreateRequest {
            operation_id: operation_id("op_backup"),
            idempotency_key: idempotency_key("idem_backup"),
        })
        .await
        .expect("backup create is accepted");

    assert_eq!(accepted.operation_id, operation_id("op_backup"));
    assert_eq!(accepted.start_sequence, event_sequence(1));
    let status = wait_for_terminal_backup_status(&api, operation_id("op_backup")).await;
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
    assert_backup_artifact_exists(&nats, artifact).await;
    let bundle = read_backup_bundle(&nats, artifact).await;
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
async fn control_runtime_does_not_resume_seeded_backup_without_accepting_command() {
    let nats = TestNats::start().await;
    assure_control_resources(&nats).await;
    let seed = seed_controllers(&nats).await;
    seed.submit_backup(BackupCreateCommand {
        operation_id: operation_id("op_recovered_backup"),
        idempotency_key: idempotency_key("idem_recovered_backup"),
    })
    .await
    .expect("accepted backup is seeded");

    let runtime =
        ployzd::control_runtime::start_control_runtime_with_client(nats.client.clone(), &config())
            .await
            .expect("control runtime starts");
    let api = OperationApiClient::new(nats.client.clone());

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
    assert_backup_artifact_absent(&nats, "op_recovered_backup").await;

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn backup_restore_recreates_single_core_control_plane_kv_state() {
    let source = TestNats::start().await;
    let runtime = ployzd::control_runtime::start_control_runtime_with_client(
        source.client.clone(),
        &config(),
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
    let api = OperationApiClient::new(source.client.clone());
    api.backup_create(&BackupCreateRequest {
        operation_id: operation_id("op_backup_restore"),
        idempotency_key: idempotency_key("idem_backup_restore"),
    })
    .await
    .expect("backup create is accepted");
    let source_status =
        wait_for_terminal_backup_status(&api, operation_id("op_backup_restore")).await;
    let source_manifest = completed_backup_manifest(&source_status, "op_backup_restore");
    let [artifact] = source_manifest.artifacts.as_slice() else {
        panic!("expected one backup artifact");
    };
    assert_backup_artifact_digest_mismatch_rejected(&source, artifact).await;
    let bundle = read_backup_bundle(&source, artifact).await;
    runtime
        .shutdown()
        .await
        .expect("source control runtime shuts down");

    let target = TestNats::start().await;
    assure_control_resources(&target).await;
    let backups = AsyncNatsBackupObjectStore::from_jetstream(&target.jetstream)
        .await
        .expect("target backup store opens");
    let restore = BackupRestoreRuntime::new(target.jetstream.clone(), backups);

    let report = restore
        .restore_bundle(&bundle)
        .await
        .expect("control-plane bundle restores");

    assert_eq!(report.buckets.len(), 4);
    assert!(matches!(
        report.observations,
        RestoreObservationState::RebuildableAfterNodeReconnect { .. }
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
    assert!(matches!(
        target_controllers
            .operation_status(&operation_id("op_backup_restore"))
            .await
            .expect("restored backup status reads")
            .expect("restored backup status exists"),
        OperationStatus::Backup {
            state: BackupOperationState::Running {
                stage: ployz_core::ops::BackupRunningStage::SnapshottingControlPlane,
            },
            ..
        }
    ));

    let duplicate_restore = restore
        .restore_bundle(&bundle)
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
        node_id("core_1"),
    )
    .with_machine_bootstrap(machine_bootstrap_config())
}

fn machine_bootstrap_config() -> MachineAddBootstrapConfig {
    MachineAddBootstrapConfig::new(
        MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
            .expect("default bootstrap URL is valid"),
    )
    .with_join_template(machine_join_template())
}

fn machine_join_template() -> MachineJoinTemplate {
    serde_json::from_str(
        r#"{
  "join_bundle": {
    "material": {
      "cluster_name": "prod",
      "runtime_nats_url": "nats://127.0.0.1:7422",
      "trusted_nats": {
        "server_id": "server_1",
        "config_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      },
      "core_iroh": {
        "node_id": "core_1",
        "public_key": "core-public-key",
        "direct_addresses": [],
        "relay_url": null
      },
      "ployzd": {
        "version": "0.1.0",
        "source": "/tmp/ployzd",
        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "install_path": "/usr/local/bin/ployzd"
      },
      "ebpf_bytecode": {
        "version": "0.1.0",
        "source": "/tmp/ployz-ebpf-tc",
        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
      },
      "ebpf_ctl": {
        "version": "0.1.0",
        "source": "/tmp/ployz-ebpf-ctl",
        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "install_path": "/usr/local/bin/ployz-ebpf-ctl"
      }
    }
  },
  "secret_delivery": {
    "nats_credentials": "user-jwt-and-seed",
    "core_iroh_ticket": "core-ticket"
  }
}
"#,
    )
    .expect("test join template is valid")
}

struct TestNats {
    _server: nats_server::Server,
    client: async_nats::Client,
    jetstream: async_nats::jetstream::Context,
}

impl TestNats {
    async fn start() -> Self {
        let config = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ployz-nats/tests/configs/jetstream.conf"
        );
        let server = nats_server::run_server(config);
        let client = async_nats::connect(server.client_url())
            .await
            .expect("connect to test nats");
        let jetstream = async_nats::jetstream::new(client.clone());

        Self {
            _server: server,
            client,
            jetstream,
        }
    }
}

async fn assure_control_resources(nats: &TestNats) {
    let plan =
        BootstrapPlan::for_single_server_client(&nats.client).expect("bootstrap plan builds");
    assure_nats_resources(&nats.jetstream, &plan)
        .await
        .expect("bootstrap resources are assured");
}

async fn seed_controllers(nats: &TestNats) -> OperationControllers {
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("status store opens");
    OperationControllers::with_owner(
        event_log,
        status_store,
        operation_owner_id("control"),
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

async fn assert_backup_artifact_exists(
    nats: &TestNats,
    artifact: &ployz_core::backup::BackupArtifact,
) {
    let bucket = nats
        .jetstream
        .get_object_store(&artifact.bucket)
        .await
        .expect("backup object bucket exists");
    let info = bucket
        .info(&artifact.object_name)
        .await
        .expect("backup artifact object exists");

    assert_eq!(info.name, artifact.object_name);
    assert_eq!(info.bucket, artifact.bucket);
    assert_eq!(artifact.kind, BackupArtifactKind::ControlPlaneBundle);
    assert_eq!(info.size as u64, artifact.byte_count);
    assert_eq!(info.digest.as_deref(), Some(artifact.digest.as_str()));
}

async fn assert_backup_artifact_absent(nats: &TestNats, operation_id_value: &str) {
    let bucket = nats
        .jetstream
        .get_object_store(PLZ_BACKUPS_BUCKET)
        .await
        .expect("backup object bucket exists");
    let object_name = control_plane_bundle_object_name(&operation_id(operation_id_value));

    assert!(bucket.info(&object_name).await.is_err());
}

async fn assert_backup_artifact_digest_mismatch_rejected(
    nats: &TestNats,
    artifact: &ployz_core::backup::BackupArtifact,
) {
    let backups = AsyncNatsBackupObjectStore::from_jetstream(&nats.jetstream)
        .await
        .expect("backup object store opens");
    let mut mismatched = artifact.clone();
    mismatched.digest = "SHA-256=not-the-recorded-digest".to_owned();

    assert!(matches!(
        backups.read_control_plane_bundle(&mismatched).await,
        Err(BackupObjectStoreError::ArtifactMismatch { .. })
    ));
}

async fn read_backup_bundle(
    nats: &TestNats,
    artifact: &ployz_core::backup::BackupArtifact,
) -> BackupBundle {
    let backups = AsyncNatsBackupObjectStore::from_jetstream(&nats.jetstream)
        .await
        .expect("backup object store opens");
    let payload = backups
        .read_control_plane_bundle(artifact)
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

    assert_eq!(
        bucket_names,
        vec!["KV_CORE", "KV_OPS", "KV_OBS", "KV_LOCKS"]
    );
    assert!(
        snapshot
            .buckets
            .iter()
            .any(|bucket| bucket.name == "KV_OPS" && !bucket.entries.is_empty())
    );
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}

fn operation_owner_id(value: &str) -> OperationOwnerId {
    OperationOwnerId::try_new(value).expect("valid operation owner id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

async fn wait_for_terminal_backup_status(
    api: &OperationApiClient,
    operation_id: OperationId,
) -> OperationStatus {
    for _ in 0..80 {
        let status = api
            .ops_status(&OpsStatusRequest {
                operation_id: operation_id.clone(),
            })
            .await
            .expect("status is readable")
            .status;
        let OperationStatus::Backup { state, .. } = &status else {
            panic!("expected backup status");
        };
        if state.is_terminal() {
            return status;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    panic!("backup did not reach terminal status");
}
