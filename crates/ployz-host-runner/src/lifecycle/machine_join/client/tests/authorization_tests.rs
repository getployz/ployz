use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ployz_core::ids::OperationId;
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{NatsConnectError, connect_authenticated};
use ployz_nats::permissions::{parse_authorized_users, render_authorized_users};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_nats::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_sdk_types::{
    MachineJoinRedeemError, MachineJoinRedeemResponse, MachineJoinToken, OperationApiResponse,
};
use ployz_test_support::nats::SecuredTestNats;

use super::redeemed_machine;
use crate::lifecycle::machine_join::client::authorization::redeem_across_authorization_reload;

#[tokio::test]
async fn redemption_retries_fresh_join_transport_across_authorization_reload() {
    let nats = SecuredTestNats::start().await.expect("secured NATS");
    let expected = redeemed_machine();
    let calls = Arc::new(AtomicUsize::new(0));
    let service = redeem_service(&nats, {
        let expected = expected.clone();
        let calls = Arc::clone(&calls);
        move || {
            calls.fetch_add(1, Ordering::Relaxed);
            OperationApiResponse::Ok {
                value: expected.clone(),
            }
        }
    })
    .await;
    let authorization_path = nats.authorized_users_path().to_path_buf();
    let original = std::fs::read_to_string(&authorization_path).expect("authorization file");
    remove_join_authority(&nats, &original);
    let config = nats.join_config();
    wait_for_join_rejection(&config).await;

    let retries = Arc::new(AtomicUsize::new(0));
    let retry_observed = Arc::new(tokio::sync::Notify::new());
    let restore = async {
        while retries.load(Ordering::Acquire) == 0 {
            let observed = retry_observed.notified();
            if retries.load(Ordering::Acquire) != 0 {
                break;
            }
            observed.await;
        }
        std::fs::write(authorization_path, original).expect("restore Join principal");
        signal_reload(nats.server_pid());
    };
    let redeem = redeem_across_authorization_reload(
        &config,
        MachineJoinToken::try_new("join_once_123").expect("join token"),
        {
            let retries = Arc::clone(&retries);
            let retry_observed = Arc::clone(&retry_observed);
            move |retry| {
                retries.store(retry.attempt, Ordering::Release);
                retry_observed.notify_waiters();
            }
        },
    );

    let (redeemed, ()) = tokio::join!(redeem, restore);
    assert_eq!(redeemed.expect("redemption crosses reload"), expected);
    assert!(retries.load(Ordering::Acquire) >= 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    service.shutdown().await.expect("service shuts down");
}

#[tokio::test]
async fn redemption_reports_typed_authorization_exhaustion() {
    let nats = SecuredTestNats::start().await.expect("secured NATS");
    let original =
        std::fs::read_to_string(nats.authorized_users_path()).expect("authorization file");
    remove_join_authority(&nats, &original);
    let config = nats.join_config();
    wait_for_join_rejection(&config).await;
    let retries = AtomicUsize::new(0);

    let failure = redeem_across_authorization_reload(
        &config,
        MachineJoinToken::try_new("join_once_123").expect("join token"),
        |_| {
            retries.fetch_add(1, Ordering::Relaxed);
        },
    )
    .await
    .expect_err("missing Join authority exhausts its bounded retry");

    assert_eq!(retries.load(Ordering::Relaxed), 9);
    assert!(failure.as_str().contains("after 10 attempts"));
    assert!(failure.as_str().contains("NATS authorization was rejected"));
}

#[tokio::test]
async fn redemption_does_not_retry_non_authorization_domain_failure() {
    let nats = SecuredTestNats::start().await.expect("secured NATS");
    let calls = Arc::new(AtomicUsize::new(0));
    let service = redeem_service(&nats, {
        let calls = Arc::clone(&calls);
        move || {
            calls.fetch_add(1, Ordering::Relaxed);
            OperationApiResponse::DomainError {
                error: MachineJoinRedeemError::UnknownJoinToken,
            }
        }
    })
    .await;
    let retries = AtomicUsize::new(0);

    let failure = redeem_across_authorization_reload(
        &nats.join_config(),
        MachineJoinToken::try_new("join_once_123").expect("join token"),
        |_| {
            retries.fetch_add(1, Ordering::Relaxed);
        },
    )
    .await
    .expect_err("domain rejection is terminal");

    assert!(failure.as_str().contains("failed to redeem join token"));
    assert!(!failure.as_str().contains("authorization"));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(retries.load(Ordering::Relaxed), 0);
    service.shutdown().await.expect("service shuts down");
}

#[tokio::test]
async fn material_not_ready_uses_the_existing_budget_without_reconnecting() {
    let nats = SecuredTestNats::start().await.expect("secured NATS");
    let expected = redeemed_machine();
    let calls = Arc::new(AtomicUsize::new(0));
    let service = redeem_service(&nats, {
        let expected = expected.clone();
        let calls = Arc::clone(&calls);
        move || {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                OperationApiResponse::DomainError {
                    error: MachineJoinRedeemError::MaterialNotReady {
                        operation_id: OperationId::try_new("op_machine").expect("operation id"),
                    },
                }
            } else {
                OperationApiResponse::Ok {
                    value: expected.clone(),
                }
            }
        }
    })
    .await;
    let retries = AtomicUsize::new(0);

    let redeemed = redeem_across_authorization_reload(
        &nats.join_config(),
        MachineJoinToken::try_new("join_once_123").expect("join token"),
        |_| {
            retries.fetch_add(1, Ordering::Relaxed);
        },
    )
    .await
    .expect("material becomes ready");

    assert_eq!(redeemed, expected);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(retries.load(Ordering::Relaxed), 0);
    service.shutdown().await.expect("service shuts down");
}

async fn redeem_service(
    nats: &SecuredTestNats,
    response: impl Fn() -> MachineJoinRedeemResponse + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let endpoint = OperationApiEndpoint::MachineJoinRedeem;
    let endpoint = NatsServiceEndpointSpec::new(
        endpoint.name(),
        endpoint.subject(),
        endpoint_execution(endpoint.execution()),
    );
    let spec = NatsServiceSpec::new(
        "join-reload-test",
        "plz-api",
        ServiceVersion::new(0, 1, 0),
        "join reload test",
        ServiceMetadata::empty(),
        vec![endpoint.clone()],
    );
    let controller = connect_authenticated(&nats.controller_config(), Duration::from_secs(2))
        .await
        .expect("controller connects");
    let mut service = start_nats_service(controller, &spec)
        .await
        .expect("service starts");
    service
        .bind_endpoint(&endpoint, move |_request| {
            let response = response();
            async move {
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            }
        })
        .await
        .expect("redeem endpoint binds");
    service
}

const fn endpoint_execution(execution: OperationApiEndpointExecution) -> EndpointExecution {
    match execution {
        OperationApiEndpointExecution::AcceptsOperation => EndpointExecution::AcceptsOperation,
        OperationApiEndpointExecution::MutatesOperation => EndpointExecution::MutatesOperation,
        OperationApiEndpointExecution::Query => EndpointExecution::Query,
    }
}

fn remove_join_authority(nats: &SecuredTestNats, original: &str) {
    let without_join = parse_authorized_users(original)
        .expect("authorization parses")
        .into_iter()
        .filter(|grant| grant.principal() != NatsPrincipal::Join)
        .collect::<Vec<_>>();
    std::fs::write(
        nats.authorized_users_path(),
        render_authorized_users(&without_join),
    )
    .expect("temporarily remove Join principal");
    signal_reload(nats.server_pid());
}

async fn wait_for_join_rejection(config: &ployz_nats::connect::NatsConnectConfig) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match connect_authenticated(config, Duration::from_millis(100)).await {
                Err(NatsConnectError::AuthorizationViolation { .. }) => return,
                Ok(_) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected Join rejection probe failure: {error}"),
            }
        }
    })
    .await
    .expect("NATS applies removed Join authority");
}

fn signal_reload(pid: u32) {
    assert!(
        std::process::Command::new("kill")
            .args(["-HUP", &pid.to_string()])
            .status()
            .expect("signal NATS")
            .success()
    );
}
