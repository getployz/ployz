use ployz_core::backup::{
    BackupArtifactKind, BackupBundle, BackupManifest, ControlPlaneKvSnapshot,
};
use ployz_core::ids::{NodeId, OperationId, OperationOwnerId};
use ployz_core::install::{MachineBootstrapUrl, MachineJoinTemplate};
use ployz_core::ops::{
    BackupOperationState, EventSequence, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayLimit, OperationEventReplayRequest, OperationIdempotencyKey,
    OperationStatus,
};
use ployz_nats::bootstrap::{BootstrapPlan, assure_nats_resources};
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_nats::operations::{AsyncNatsOperationEventLog, AsyncNatsOperationStatusStore};
use ployz_sdk_types::{BackupCreateRequest, OpsStatusRequest};
use ployzd::config::{ControlProcessConfig, DEFAULT_MACHINE_BOOTSTRAP_URL};
use ployzd::controllers::{BackupCreateCommand, MachineAddBootstrapConfig, OperationControllers};
use ployzd::nats_process::NatsServerRuntime;
use tokio::io::AsyncReadExt;

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
        BackupManifest::single_core_control_plane().format_version
    );
    assert_eq!(
        manifest.scope,
        BackupManifest::single_core_control_plane().scope
    );
    assert_eq!(
        manifest.restore_contract,
        BackupManifest::single_core_control_plane().restore_contract
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
async fn control_runtime_recovers_accepted_backup_create_from_nats() {
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

    let status = wait_for_terminal_backup_status(&api, operation_id("op_recovered_backup")).await;
    let manifest = completed_backup_manifest(&status, "op_recovered_backup");
    let [artifact] = manifest.artifacts.as_slice() else {
        panic!("expected one backup artifact");
    };
    assert_backup_artifact_exists(&nats, artifact).await;
    let bundle = read_backup_bundle(&nats, artifact).await;
    assert_snapshot_contains_control_buckets(&bundle.control_plane);

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
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
        "public_key": "core-public-key"
      },
      "ployzd": {
        "version": "0.1.0",
        "source": "/tmp/ployzd",
        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "install_path": "/usr/local/bin/ployzd"
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
    let config = config();
    let plan = BootstrapPlan::for_single_server_client_and_topology(
        &nats.client,
        &config.core_topology,
        config.core_node_id,
    )
    .expect("bootstrap plan builds");
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

async fn read_backup_bundle(
    nats: &TestNats,
    artifact: &ployz_core::backup::BackupArtifact,
) -> BackupBundle {
    let bucket = nats
        .jetstream
        .get_object_store(&artifact.bucket)
        .await
        .expect("backup object bucket exists");
    let mut object = bucket
        .get(&artifact.object_name)
        .await
        .expect("backup artifact object is readable");
    let mut payload = Vec::new();
    object
        .read_to_end(&mut payload)
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
