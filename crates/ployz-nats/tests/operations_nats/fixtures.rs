use ployz_core::cert::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow,
};
use ployz_core::deploy::{
    DeployPlan, DeployPlanStep, DeployRequest, DeployServicePlan, ReplicaSlot,
};
use ployz_core::machine::IssuedJoinToken;
use ployz_core::ops::{DeployOperationFailure, DeployRunningStage};
use ployz_core::roles::InstallRolePolicy;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    CertOperationSubmission, DeployOperationSubmission, MachineAddOperationSubmission,
};

pub(super) use async_nats::jetstream;
pub(super) use ployz_test_support::fixtures::{deploy_target, machine_join_bundle};
pub(super) use ployz_test_support::ids::{
    cancellation_reason, cert_id, container_id, event_replay_limit, event_sequence,
    failure_message, idempotency_key, join_token_expires_at as expires_at,
    join_token_redeemed_at as joined_at, machine_id, machine_name, namespace_id,
    namespace_revision_id, operation_id, raw_join_token, service_id,
};
pub(super) use ployz_test_support::nats::TestNats;

pub(super) async fn test_nats() -> TestNats {
    let nats = TestNats::start().await;
    nats.bootstrap_resources().await;
    nats
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

pub(super) fn deploy_submission(operation_id: &str, service_id: &str) -> DeployOperationSubmission {
    DeployOperationSubmission {
        operation_id: self::operation_id(operation_id),
        target: deploy_target(service_id),
    }
}

pub(super) fn empty_deploy_submission(operation_id: &str) -> DeployOperationSubmission {
    DeployOperationSubmission {
        operation_id: self::operation_id(operation_id),
        target: DeployRequest {
            namespace_id: namespace_id("production"),
            namespace_revision_id: namespace_revision_id("rev_empty"),
            services: Vec::new(),
        },
    }
}

pub(super) fn cert_submission(operation_id: &str, cert_id: &str) -> CertOperationSubmission {
    CertOperationSubmission {
        operation_id: self::operation_id(operation_id),
        cert_id: self::cert_id(cert_id),
    }
}

pub(super) fn machine_add_submission(
    operation_id: &str,
    idempotency_key: &str,
    machine_id: &str,
    machine_name: &str,
) -> MachineAddOperationSubmission {
    MachineAddOperationSubmission {
        operation_id: self::operation_id(operation_id),
        machine_id: self::machine_id(machine_id),
        name: self::machine_name(machine_name),
        roles: InstallRolePolicy::install_all().without_gateway(),
        join_bundle: machine_join_bundle(),
        raw_join_token: raw_join_token("join_token"),
        join_token: issued_join_token_for_raw("join_token"),
        idempotency_key: self::idempotency_key(idempotency_key),
    }
}

/// A syntactically seed-shaped minted credential (not a real key).
pub(super) const TEST_MINTED_SEED: &str =
    "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM";

pub(super) fn machine_join_secret_delivery() -> ployz_core::install::MachineJoinSecretDelivery {
    ployz_core::install::MachineJoinSecretDelivery {
        nats_credentials: ployz_core::nats_config::NatsUserSeed::try_new(TEST_MINTED_SEED)
            .expect("valid nats credentials"),
    }
}

/// Simulates the mint worker reaching `material-ready` for a submission:
/// redeem hands out the secret only after this record exists.
pub(super) async fn store_minted_secret(
    repository: &ployz_nats::operations::AsyncNatsOperationRepository,
    operation: &str,
    idempotency: &str,
) {
    repository
        .records()
        .put_machine_add_secret_delivery_if_absent(
            &idempotency_key(idempotency),
            &ployz_nats::operations::StoredMachineAddSecretDelivery {
                operation_id: operation_id(operation),
                secret_delivery: machine_join_secret_delivery(),
            },
        )
        .await
        .expect("minted secret stores");
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

pub(super) fn deploy_plan() -> DeployPlan {
    deploy_plan_on("machine_a")
}

pub(super) fn deploy_plan_on(machine: &str) -> DeployPlan {
    DeployPlan {
        namespace_id: namespace_id("default"),
        namespace_revision_id: namespace_revision_id("rev_2"),
        services: vec![DeployServicePlan {
            service_id: service_id("svc_api"),
            steps: vec![DeployPlanStep::RunContainer {
                machine_id: machine_id(machine),
                slot: ReplicaSlot::try_new(1).expect("valid replica slot"),
            }],
        }],
        cleanup_containers: Vec::new(),
    }
}

pub(super) fn planning_failure(message: &str) -> DeployOperationFailure {
    DeployOperationFailure::PlanningFailed {
        service_id: service_id("svc_api"),
        namespace_revision_id: namespace_revision_id("rev_2"),
        message: failure_message(message),
    }
}

pub(super) fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ServingTargetCommit
}

pub(super) fn cert_challenge(hostname: &str) -> AcmeHttp01Challenge {
    AcmeHttp01Challenge::try_new(
        ployz_test_support::ids::route_hostname(hostname),
        AcmeChallengeToken::try_new("token_123").expect("valid challenge token"),
        AcmeChallengeValue::try_new("token_123.thumbprint_456").expect("valid challenge value"),
        AcmeChallengeTtlSeconds::try_new(60).expect("valid challenge ttl"),
    )
    .expect("valid challenge")
}

pub(super) fn active_cert(cert_id: &str, hostname: &str) -> ActiveCertState {
    ActiveCertState {
        cert_id: self::cert_id(cert_id),
        hostname: ployz_test_support::ids::route_hostname(hostname),
        bundle_ref: CertBundleRef::try_new(format!("obj://PLZ_CERTS/{cert_id}/rev_1"))
            .expect("valid bundle ref"),
        validity: CertValidityWindow::try_new(valid_at(1_700_000_000), valid_at(1_707_776_000))
            .expect("valid validity"),
    }
}

fn valid_at(value: u64) -> CertValidAt {
    CertValidAt::try_new(value).expect("valid cert timestamp")
}
