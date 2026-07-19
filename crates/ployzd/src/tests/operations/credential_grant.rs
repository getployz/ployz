use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::connection::State;
use futures_util::StreamExt;
use ployz_core::ids::{BuildExecutorId, BuildPoolId};
use ployz_core::nats_config::{
    BuildExecutorCredentialExpiresAt, CredentialGrant, CredentialName, CredentialRole,
    MintedNatsUser, NatsUserPublicKey,
};
use ployz_core::operation::{
    CredentialGrantFailure, CredentialGrantOperationState, OperationStatus,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{NatsConnectConfig, connect_authenticated};
use ployz_nats::subjects::{
    BuildExecutorServiceEndpoint, MachineServiceEndpoint, build_executor_service, machine_service,
};
use ployz_sdk_types::{
    ControlNatsAuthorizationHealth, CredentialAddRequest, CredentialListRequest,
    CredentialRemoveRequest, RuntimeSnapshotRequest,
};
use ployz_test_support::ids::{machine_id, operation_id};
use ployz_test_support::ops::{poll_until, wait_for_terminal_status};

use crate::tests::support::control::{RecordingReload, TestNats};

const LIFECYCLE_BUDGET: Duration = Duration::from_secs(15);
const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[tokio::test]
async fn operator_can_add_list_rename_and_remove_a_live_credential() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let minted = MintedNatsUser::generate().expect("credential mints");
    let client_config = nats.server().config_with_seed(
        ployz_core::security::NatsPrincipal::Operator,
        minted.seed.clone(),
    );

    api.credential_add(&CredentialAddRequest {
        operation_id: operation_id("op_credential_add"),
        grant: credential(&minted.public, "Nick laptop"),
    })
    .await
    .expect("credential add accepts");
    assert_completed(&api, "op_credential_add").await;

    let added_client = connect_authenticated(&client_config, Duration::from_secs(2))
        .await
        .expect("added credential connects");
    api.credential_add(&CredentialAddRequest {
        operation_id: operation_id("op_credential_rename"),
        grant: credential(&minted.public, "Nick workstation"),
    })
    .await
    .expect("credential rename accepts");
    assert_completed(&api, "op_credential_rename").await;

    let listed = api
        .credential_list(&CredentialListRequest {})
        .await
        .expect("credentials list");
    assert_eq!(
        listed
            .credentials
            .iter()
            .filter(|grant| grant.public_key == minted.public)
            .collect::<Vec<_>>(),
        vec![&credential(&minted.public, "Nick workstation")]
    );

    api.credential_remove(&CredentialRemoveRequest {
        operation_id: operation_id("op_credential_remove"),
        public_key: minted.public.clone(),
    })
    .await
    .expect("credential remove accepts");
    assert_completed(&api, "op_credential_remove").await;

    poll_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        async || (added_client.connection_state() != State::Connected).then_some(()),
    )
    .await
    .expect("removed live credential disconnects");
    assert!(
        connect_authenticated(&client_config, Duration::from_secs(1))
            .await
            .is_err(),
        "removed credential cannot reconnect"
    );
    api.credential_list(&CredentialListRequest {})
        .await
        .expect("remaining Operator still works");

    runtime.shutdown().await.expect("runtime shuts down");
}

#[tokio::test]
async fn final_operator_removal_is_a_typed_terminal_failure() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let founder_public_key = public_key_from_seed(nats.server().user_seed());

    api.credential_remove(&CredentialRemoveRequest {
        operation_id: operation_id("op_remove_final_operator"),
        public_key: founder_public_key,
    })
    .await
    .expect("final removal accepts as operation work");
    let status = wait_for_terminal_status(
        &api,
        &operation_id("op_remove_final_operator"),
        Duration::from_secs(5),
    )
    .await;
    assert!(matches!(
        status,
        OperationStatus::CredentialGrant {
            state: CredentialGrantOperationState::Failed {
                failure: CredentialGrantFailure::LastOperator,
            },
            ..
        }
    ));

    runtime.shutdown().await.expect("runtime shuts down");
}

#[tokio::test]
async fn removing_build_executor_revokes_the_connected_client() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let minted = MintedNatsUser::generate().expect("executor credential mints");
    let public_key = minted.public.clone();
    let pool_id = BuildPoolId::try_new("pool_ci").expect("pool id");
    let executor_id = BuildExecutorId::try_new("executor_revoke").expect("executor id");
    let grant = executor_credential(
        &minted.public,
        "Revoked executor",
        &pool_id,
        &executor_id,
        current_unix_seconds() + 300,
    );
    add_credential(&api, "op_executor_revoke_add", grant).await;

    let client_config = executor_connect_config(&nats, minted, &pool_id, &executor_id);
    let executor = connect_authenticated(&client_config, Duration::from_secs(2))
        .await
        .expect("enrolled executor connects");
    let endpoint = build_executor_service(
        &pool_id,
        &executor_id,
        BuildExecutorServiceEndpoint::ReadinessGet,
    );
    let mut requests = executor
        .subscribe(endpoint.clone())
        .await
        .expect("executor binds its readiness endpoint");
    executor
        .flush()
        .await
        .expect("executor readiness subscription flushes");
    nats.connected
        .controller
        .publish(endpoint, "readiness".into())
        .await
        .expect("controller publishes readiness request");
    nats.connected
        .controller
        .flush()
        .await
        .expect("controller flushes readiness request");
    tokio::time::timeout(Duration::from_secs(2), requests.next())
        .await
        .expect("executor receives readiness request")
        .expect("readiness subscription stays open");

    api.credential_remove(&CredentialRemoveRequest {
        operation_id: operation_id("op_executor_revoke_remove"),
        public_key,
    })
    .await
    .expect("credential remove accepts");
    assert_completed(&api, "op_executor_revoke_remove").await;

    wait_for_disconnection(&executor, "removed executor").await;
    assert!(
        connect_authenticated(&client_config, Duration::from_secs(1))
            .await
            .is_err(),
        "removed executor cannot reconnect"
    );
    api.credential_list(&CredentialListRequest {})
        .await
        .expect("founder Operator remains usable");

    runtime.shutdown().await.expect("runtime shuts down");
}

#[tokio::test]
async fn expiring_build_executor_loses_live_authority_without_disturbing_operator_or_machine() {
    let retained_machine_id = machine_id("edge_retained");
    let nats = TestNats::start_with_machines(std::slice::from_ref(&retained_machine_id)).await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let machine = nats.machine_client(&retained_machine_id).await;
    let minted = MintedNatsUser::generate().expect("executor credential mints");
    let public_key = minted.public.clone();
    let pool_id = BuildPoolId::try_new("pool_ci").expect("pool id");
    let executor_id = BuildExecutorId::try_new("executor_expiry").expect("executor id");
    let expires_at = current_unix_seconds() + 8;
    let grant = executor_credential(
        &public_key,
        "Expiring executor",
        &pool_id,
        &executor_id,
        expires_at,
    );
    add_credential(&api, "op_executor_expiry_add", grant.clone()).await;
    wait_for_authorization_health(&api, "scheduled executor expiry", |health| {
        (health.next_expiry_at_unix_seconds == Some(expires_at)).then_some(())
    })
    .await;

    let client_config = executor_connect_config(&nats, minted, &pool_id, &executor_id);
    let executor = connect_authenticated(&client_config, Duration::from_secs(2))
        .await
        .expect("executor connects before expiry");
    executor
        .subscribe(build_executor_service(
            &pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::BuildStart,
        ))
        .await
        .expect("executor binds before expiry");

    wait_for_authorization_health(&api, "successful executor expiry projection", |health| {
        (health.next_expiry_at_unix_seconds.is_none() && health.consecutive_failures == 0)
            .then_some(())
    })
    .await;
    wait_for_disconnection(&executor, "expired executor").await;
    assert!(
        connect_authenticated(&client_config, Duration::from_secs(1))
            .await
            .is_err(),
        "expired executor cannot reconnect"
    );

    let listed = api
        .credential_list(&CredentialListRequest {})
        .await
        .expect("founder Operator remains usable");
    assert!(
        listed.credentials.contains(&grant),
        "expired grant remains durable"
    );
    assert_eq!(machine.connection_state(), State::Connected);
    machine
        .subscribe(machine_service(
            &retained_machine_id,
            MachineServiceEndpoint::FactsGet,
        ))
        .await
        .expect("retained Machine can still bind its endpoint");
    machine
        .flush()
        .await
        .expect("retained Machine remains usable");

    runtime.shutdown().await.expect("runtime shuts down");
}

#[tokio::test]
async fn failed_expiry_reload_surfaces_health_then_recovers_and_revokes() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let initial_runtime = nats.start_control(&config).await;
    let api = nats.api();
    let minted = MintedNatsUser::generate().expect("executor credential mints");
    let public_key = minted.public.clone();
    let pool_id = BuildPoolId::try_new("pool_ci").expect("pool id");
    let executor_id = BuildExecutorId::try_new("executor_retry").expect("executor id");
    let expires_at = current_unix_seconds() + 12;
    let grant = executor_credential(
        &public_key,
        "Retry executor",
        &pool_id,
        &executor_id,
        expires_at,
    );
    add_credential(&api, "op_executor_retry_add", grant.clone()).await;
    let client_config = executor_connect_config(&nats, minted, &pool_id, &executor_id);
    let executor = connect_authenticated(&client_config, Duration::from_secs(2))
        .await
        .expect("executor connects before expiry");
    initial_runtime
        .shutdown()
        .await
        .expect("initial runtime shuts down");

    let reload = RecordingReload::fail_then_gated_signal(nats.server().server_pid());
    let runtime = nats
        .start_control_with_reload(&config, reload.clone())
        .await;
    wait_for_authorization_health(&api, "rescheduled executor expiry", |health| {
        (health.next_expiry_at_unix_seconds == Some(expires_at)).then_some(())
    })
    .await;
    assert!(
        reload.outcomes().is_empty(),
        "fixture startup reload bypasses the mutation runner"
    );
    wait_for_authorization_health(&api, "failed expiry reload evidence", |health| {
        (health.consecutive_failures == 1
            && health
                .last_failure
                .as_deref()
                .is_some_and(|failure| failure.contains("reload refused by test")))
        .then_some(())
    })
    .await;
    assert_eq!(executor.connection_state(), State::Connected);
    assert!(
        matches!(
            reload.outcomes().as_slice(),
            [crate::control::authorization::NatsReloadOutcome::Failed(_)]
        ),
        "expiry is the mutation runner's first call and retry remains held"
    );

    reload.release();
    let retry_outcomes = poll_until(LIFECYCLE_BUDGET, LIFECYCLE_POLL_INTERVAL, async || {
        (reload.outcomes().len() == 2).then(|| reload.outcomes())
    })
    .await
    .expect("retry reload completes after its gate is released");
    assert!(
        matches!(
            retry_outcomes.as_slice(),
            [
                crate::control::authorization::NatsReloadOutcome::Failed(_),
                crate::control::authorization::NatsReloadOutcome::Reloaded(_)
            ]
        ),
        "retry reload succeeds: {retry_outcomes:?}"
    );
    wait_for_authorization_health(&api, "recovered expiry reload", |health| {
        (health.consecutive_failures == 0
            && health.last_failure.is_none()
            && health.next_expiry_at_unix_seconds.is_none())
        .then_some(())
    })
    .await;
    wait_for_disconnection(&executor, "executor after recovered expiry reload").await;
    assert!(
        connect_authenticated(&client_config, Duration::from_secs(1))
            .await
            .is_err(),
        "expired executor cannot reconnect after retry"
    );
    let listed = api
        .credential_list(&CredentialListRequest {})
        .await
        .expect("credentials remain queryable after retry");
    assert!(
        listed.credentials.contains(&grant),
        "expired grant remains durable"
    );

    runtime.shutdown().await.expect("runtime shuts down");
}

fn credential(public_key: &NatsUserPublicKey, name: &str) -> CredentialGrant {
    CredentialGrant {
        public_key: public_key.clone(),
        name: CredentialName::try_new(name).expect("credential name"),
        role: CredentialRole::Operator,
    }
}

fn executor_credential(
    public_key: &NatsUserPublicKey,
    name: &str,
    pool_id: &BuildPoolId,
    executor_id: &BuildExecutorId,
    expires_at: u64,
) -> CredentialGrant {
    CredentialGrant {
        public_key: public_key.clone(),
        name: CredentialName::try_new(name).expect("credential name"),
        role: CredentialRole::BuildExecutor {
            pool_id: pool_id.clone(),
            executor_id: executor_id.clone(),
            expires_at: BuildExecutorCredentialExpiresAt::try_new(expires_at).expect("expiry"),
        },
    }
}

fn executor_connect_config(
    nats: &TestNats,
    minted: MintedNatsUser,
    pool_id: &BuildPoolId,
    executor_id: &BuildExecutorId,
) -> NatsConnectConfig {
    nats.server().config_with_seed(
        NatsPrincipal::BuildExecutor {
            pool_id: pool_id.clone(),
            executor_id: executor_id.clone(),
        },
        minted.seed,
    )
}

async fn add_credential(
    api: &ployz_nats::operation_api_client::OperationApiClient,
    operation: &str,
    grant: CredentialGrant,
) {
    api.credential_add(&CredentialAddRequest {
        operation_id: operation_id(operation),
        grant,
    })
    .await
    .expect("credential add accepts");
    assert_completed(api, operation).await;
}

async fn wait_for_disconnection(client: &async_nats::Client, label: &str) {
    poll_until(LIFECYCLE_BUDGET, LIFECYCLE_POLL_INTERVAL, async || {
        (client.connection_state() != State::Connected).then_some(())
    })
    .await
    .unwrap_or_else(|| panic!("{label} stays connected"));
}

async fn wait_for_authorization_health<T>(
    api: &ployz_nats::operation_api_client::OperationApiClient,
    label: &str,
    mut accept: impl FnMut(&ControlNatsAuthorizationHealth) -> Option<T>,
) -> T {
    poll_until(LIFECYCLE_BUDGET, LIFECYCLE_POLL_INTERVAL, async || {
        let health = api
            .runtime_snapshot(&RuntimeSnapshotRequest {})
            .await
            .ok()?
            .control_health?
            .nats_authorization;
        accept(&health)
    })
    .await
    .unwrap_or_else(|| panic!("timed out waiting for {label}"))
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is after Unix epoch")
        .as_secs()
}

fn public_key_from_seed(seed: &ployz_core::nats_config::NatsUserSeed) -> NatsUserPublicKey {
    let pair = nkeys::KeyPair::from_seed(seed.secret()).expect("fixture seed parses");
    NatsUserPublicKey::try_new(pair.public_key()).expect("fixture public key parses")
}

async fn assert_completed(api: &ployz_nats::operation_api_client::OperationApiClient, id: &str) {
    let status = wait_for_terminal_status(api, &operation_id(id), Duration::from_secs(5)).await;
    assert!(matches!(
        status,
        OperationStatus::CredentialGrant {
            state: CredentialGrantOperationState::Completed,
            ..
        }
    ));
}
