use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use ployz_core::cert::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow,
};
use ployz_core::deploy::{
    DeployPlan, DeployPlanStep, DeployRequest, ImageReference, ReplicaCount, ReplicaSlot,
};
use ployz_core::ids::{CertId, OperationId, OperationOwnerId, RevisionId, ServiceId};
use ployz_core::machine::{IssuedJoinToken, JoinTokenExpiresAt, MachineName, RawJoinToken};
use ployz_core::ops::{
    CancellationReason, DeployOperationFailure, DeployRunningStage, EventSequence, FailureMessage,
    OperationEventReplayLimit, OperationIdempotencyKey, OperationLeaseExpiresAt, RouteHostname,
};
use ployz_core::roles::FirstNodeGateway;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    BackupOperationSubmission, CertOperationSubmission, DeployOperationSubmission, KV_OPS_BUCKET,
    MachineAddOperationSubmission, OperationLeaseClaim, PLZ_JOBS_STREAM, PLZ_OPS_STREAM,
};
use ployz_sdk_types::{MachineJoinBundle, MachineJoinPloyzdArtifact};

pub(super) struct TestNats {
    _server: nats_server::Server,
    pub(super) jetstream: jetstream::Context,
}

pub(super) async fn test_nats() -> TestNats {
    let server = nats_server::run_server("tests/configs/jetstream.conf");
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let jetstream = jetstream::new(client);
    bootstrap_operation_resources(&jetstream).await;

    TestNats {
        _server: server,
        jetstream,
    }
}

pub(super) async fn bootstrap_operation_resources(jetstream: &jetstream::Context) {
    jetstream
        .create_stream(jetstream::stream::Config {
            name: PLZ_OPS_STREAM.to_owned(),
            subjects: vec!["plz.v1.op.>".to_owned()],
            storage: StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("create PLZ_OPS stream");
    jetstream
        .create_stream(jetstream::stream::Config {
            name: PLZ_JOBS_STREAM.to_owned(),
            subjects: vec!["plz.v1.job.>".to_owned()],
            storage: StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("create PLZ_JOBS stream");
    jetstream
        .create_key_value(jetstream::kv::Config {
            bucket: KV_OPS_BUCKET.to_owned(),
            storage: StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("create KV_OPS bucket");
}

pub(super) async fn operation_repository(
    jetstream: &jetstream::Context,
) -> AsyncNatsOperationRepository {
    AsyncNatsOperationRepository::new(
        AsyncNatsOperationEventLog::new(jetstream.clone()),
        AsyncNatsOperationStatusStore::from_jetstream(jetstream)
            .await
            .expect("open operation status store"),
    )
}

pub(super) fn deploy_submission(
    operation_id: &str,
    idempotency_key: &str,
    service_id: &str,
) -> DeployOperationSubmission {
    DeployOperationSubmission {
        operation_id: self::operation_id(operation_id),
        target: deploy_target(service_id),
        idempotency_key: self::idempotency_key(idempotency_key),
    }
}

pub(super) fn cert_submission(
    operation_id: &str,
    idempotency_key: &str,
    cert_id: &str,
) -> CertOperationSubmission {
    CertOperationSubmission {
        operation_id: self::operation_id(operation_id),
        cert_id: self::cert_id(cert_id),
        idempotency_key: self::idempotency_key(idempotency_key),
    }
}

pub(super) fn backup_submission(
    operation_id: &str,
    idempotency_key: &str,
) -> BackupOperationSubmission {
    BackupOperationSubmission {
        operation_id: self::operation_id(operation_id),
        idempotency_key: self::idempotency_key(idempotency_key),
    }
}

pub(super) fn machine_add_submission(
    operation_id: &str,
    idempotency_key: &str,
    node_id: &str,
    machine_name: &str,
) -> MachineAddOperationSubmission {
    MachineAddOperationSubmission {
        operation_id: self::operation_id(operation_id),
        node_id: self::node_id(node_id),
        name: MachineName::try_new(machine_name).expect("valid machine name"),
        gateway: FirstNodeGateway::Skip,
        join_bundle: machine_join_bundle(),
        secret_delivery: machine_join_secret_delivery(),
        raw_join_token: raw_join_token("join_token"),
        join_token: issued_join_token_for_raw("join_token"),
        idempotency_key: self::idempotency_key(idempotency_key),
    }
}

pub(super) fn machine_join_bundle() -> MachineJoinBundle {
    MachineJoinBundle {
        material: ployz_core::install::MachineJoinMaterial {
            cluster_name: ployz_core::install::MachineJoinClusterName::try_new("prod")
                .expect("valid cluster name"),
            runtime_nats_url: ployz_core::install::MachineJoinRuntimeNatsUrl::try_new(
                "nats://127.0.0.1:7422",
            )
            .expect("valid runtime nats url"),
            trusted_nats: ployz_core::install::MachineJoinTrustedNats {
                server_id: ployz_core::install::MachineJoinTrustedNatsServerId::try_new("server_1")
                    .expect("valid nats server id"),
                config_sha256: ployz_core::install::InstallSha256Digest::try_new(
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                )
                .expect("valid nats config digest"),
            },
            core_iroh: ployz_core::install::MachineJoinCoreIrohEndpoint {
                node_id: ployz_core::ids::NodeId::try_new("core_1").expect("valid core node id"),
                public_key: ployz_core::install::MachineJoinIrohPublicKey::try_new(
                    "core-public-key",
                )
                .expect("valid core iroh public key"),
                direct_addresses: Vec::new(),
                relay_url: None,
            },
            ployzd: MachineJoinPloyzdArtifact {
                version: ployz_core::install::InstallArtifactVersion::try_new("0.1.0")
                    .expect("valid version"),
                source: ployz_core::install::InstallArtifactSource::try_new("/tmp/ployzd")
                    .expect("valid source"),
                sha256: ployz_core::install::InstallSha256Digest::try_new(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("valid digest"),
                install_path: ployz_core::install::AbsoluteInstallPath::try_new(
                    "/usr/local/bin/ployzd",
                )
                .expect("valid install path"),
            },
        },
    }
}

pub(super) fn machine_join_secret_delivery() -> ployz_core::install::MachineJoinSecretDelivery {
    ployz_core::install::MachineJoinSecretDelivery {
        nats_credentials: ployz_core::install::MachineJoinNatsCredentials::try_new(
            "user-jwt-and-seed",
        )
        .expect("valid nats credentials"),
        core_iroh_ticket: ployz_core::install::MachineJoinIrohTicket::try_new("core-ticket")
            .expect("valid core iroh ticket"),
    }
}

pub(super) fn issued_join_token_for_raw(value: &str) -> IssuedJoinToken {
    issued_join_token_for_raw_with_expiry(value, 700)
}

pub(super) fn issued_join_token_for_raw_with_expiry(
    value: &str,
    expires_at: u64,
) -> IssuedJoinToken {
    IssuedJoinToken::new(
        raw_join_token(value)
            .fingerprint()
            .expect("test raw join token fingerprints"),
        self::expires_at(expires_at),
    )
}

pub(super) fn raw_join_token(value: &str) -> RawJoinToken {
    RawJoinToken::try_new(value).expect("valid raw join token")
}

pub(super) fn lease_claim(owner_id: &str, now: u64, expires_at: u64) -> OperationLeaseClaim {
    OperationLeaseClaim::try_new(
        OperationOwnerId::try_new(owner_id).expect("valid operation owner id"),
        lease_time(now),
        lease_time(expires_at),
    )
    .expect("valid lease claim")
}

pub(super) fn default_lease_claim() -> OperationLeaseClaim {
    lease_claim("control_a", 100, 160)
}

pub(super) fn deploy_plan() -> DeployPlan {
    deploy_plan_on("node_a")
}

pub(super) fn deploy_plan_on(node: &str) -> DeployPlan {
    DeployPlan {
        service_id: service_id("svc_api"),
        target_revision: ployz_core::ids::RevisionId::try_new("rev_2").expect("valid revision id"),
        steps: vec![DeployPlanStep::RunContainer {
            node_id: node_id(node),
            slot: ReplicaSlot::try_new(1).expect("valid replica slot"),
        }],
        cleanup_containers: Vec::new(),
    }
}

pub(super) fn planning_failure(message: &str) -> DeployOperationFailure {
    DeployOperationFailure::PlanningFailed {
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_2"),
        message: failure_message(message),
    }
}

pub(super) fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

pub(super) fn owner_id(value: &str) -> OperationOwnerId {
    OperationOwnerId::try_new(value).expect("valid operation owner id")
}

pub(super) fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

pub(super) fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

pub(super) fn cert_id(value: &str) -> CertId {
    CertId::try_new(value).expect("valid cert id")
}

pub(super) fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        service_id: self::service_id(service_id),
        target_revision: revision_id("rev_2"),
        image: image("ghcr.io/acme/api:rev-2"),
        replicas: replicas(1),
        route: None,
    }
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

pub(super) fn node_id(value: &str) -> ployz_core::ids::NodeId {
    ployz_core::ids::NodeId::try_new(value).expect("valid node id")
}

pub(super) fn container_id(value: &str) -> ployz_core::ids::ContainerId {
    ployz_core::ids::ContainerId::try_new(value).expect("valid container id")
}

pub(super) fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

pub(super) fn joined_at(value: u64) -> ployz_core::machine::JoinTokenRedeemedAt {
    ployz_core::machine::JoinTokenRedeemedAt::try_new(value).expect("valid join time")
}

pub(super) fn expires_at(value: u64) -> JoinTokenExpiresAt {
    JoinTokenExpiresAt::try_new(value).expect("valid join token expiry")
}

pub(super) fn event_replay_limit(value: u16) -> OperationEventReplayLimit {
    OperationEventReplayLimit::try_new(value).expect("valid event replay limit")
}

pub(super) fn lease_time(value: u64) -> OperationLeaseExpiresAt {
    OperationLeaseExpiresAt::try_new(value).expect("valid lease time")
}

pub(super) fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}

pub(super) fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}

pub(super) fn cancellation_reason(value: &str) -> CancellationReason {
    CancellationReason::try_new(value).expect("valid cancellation reason")
}

pub(super) fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ActiveServiceCommit
}

pub(super) fn cert_challenge(hostname: &str) -> AcmeHttp01Challenge {
    AcmeHttp01Challenge::try_new(
        RouteHostname::try_new(hostname).expect("valid hostname"),
        AcmeChallengeToken::try_new("token_123").expect("valid challenge token"),
        AcmeChallengeValue::try_new("token_123.thumbprint_456").expect("valid challenge value"),
        AcmeChallengeTtlSeconds::try_new(60).expect("valid challenge ttl"),
    )
    .expect("valid challenge")
}

pub(super) fn active_cert(cert_id: &str, hostname: &str) -> ActiveCertState {
    ActiveCertState {
        cert_id: self::cert_id(cert_id),
        hostname: RouteHostname::try_new(hostname).expect("valid hostname"),
        bundle_ref: CertBundleRef::try_new(format!("obj://PLZ_CERTS/{cert_id}/rev_1"))
            .expect("valid bundle ref"),
        validity: CertValidityWindow::try_new(valid_at(1_700_000_000), valid_at(1_707_776_000))
            .expect("valid validity"),
    }
}

fn valid_at(value: u64) -> CertValidAt {
    CertValidAt::try_new(value).expect("valid cert timestamp")
}
