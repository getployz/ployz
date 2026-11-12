//! A mutation the session sends once, and whose reply never comes back, says so: the
//! error is marked `reply_lost` and names the entry Machine that may have done the work.

use std::{future::Future, sync::Arc, time::Duration};

use ployz::sdk::Session;
use ployz_core::{MintBuildGrantRequest, RpcError};
use tokio::{sync::Notify, time::timeout};

use super::sdk::advertised_description;
use super::support::DiscoveryService;
use super::unix_session::{self, UnixSession};

/// The error's code, message and details as they cross the wire. Its causes name the
/// socket error the operating system gave, so they are only required to be there.
fn wire(error: &RpcError) -> String {
    assert!(!error.cause.is_empty(), "{error:?}");
    serde_json::to_string(&serde_json::json!({
        "code": error.code,
        "message": error.message,
        "details": error.details,
    }))
    .unwrap()
}

/// The error `call` returns when the entry Machine's connection drops after it took the
/// call and before it answered.
async fn lost<T, E>(
    call: impl FnOnce(Session) -> std::pin::Pin<Box<dyn Future<Output = Result<T, E>> + Send>>,
) -> E
where
    T: Send + 'static,
    E: Send + 'static,
{
    let description = advertised_description();
    let sessions = UnixSession::start().await;
    let received = Arc::new(Notify::new());
    let mut service = DiscoveryService::new(description.clone());
    service.advertises_enrollment = true;
    service.register_blocked = Some(received.clone());
    service.mutation_blocked = Some(received.clone());
    let machine = sessions
        .spawn_machine(description.machine_id, service)
        .await;
    let session = unix_session::connect(&sessions.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let pending = tokio::spawn(call(session.clone()));
    timeout(Duration::from_secs(5), received.notified())
        .await
        .expect("the entry took the call");
    machine.disconnect();
    let result = timeout(Duration::from_secs(5), pending)
        .await
        .expect("a dropped connection ends the call")
        .unwrap();
    session.close().await;
    match result {
        Err(error) => error,
        Ok(_) => panic!("the call was answered"),
    }
}

fn assignment(snapshot: &ployz_core::EnrollmentSnapshot) -> ployz_core::EnrollmentAssignment {
    let identity = ployz_core::RegisterRequest {
        join_approval: None,
        join_confirmation: None,
        machine_id: ployz_core::MachineId::parse("b".repeat(32)).unwrap(),
        assigned_subnet: Some("10.210.1.0/24".parse().unwrap()),
        initial_policy: Default::default(),
        name: ployz_core::MachineName::parse("joiner").unwrap(),
        storage: ployz_core::StorageChoice::Zfs,
        public_key: ployz_core::WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
            "192.0.2.9:51820".parse().unwrap(),
        )],
        runtime: Default::default(),
        capabilities: Vec::new(),
    };
    ployz_core::allocate_enrollment(&identity, snapshot, &[]).unwrap()
}

// Rung 2: the exact error each unretried mutation returns when its reply is lost.
#[tokio::test]
async fn register_whose_reply_is_lost_keeps_its_uncertain_outcome_message() {
    let error = lost(|session| {
        Box::pin(async move {
            let snapshot = session.observe_enrollment().await?;
            session.register(&assignment(&snapshot)).await
        })
    })
    .await;
    assert_eq!(
        wire(&error),
        r#"{"code":"internal","details":{"machine_id":"0123456789abcdef0123456789abcdef","reply_lost":true},"message":"Register response lost; outcome may be uncertain: transport error"}"#
    );
}

#[tokio::test]
async fn clear_management_client_whose_reply_is_lost_answers_with_the_transport_error() {
    let label = ployz_core::ManagementClientLabel::parse("cloud").unwrap();
    let error =
        lost(|session| Box::pin(async move { session.clear_management_client(label).await })).await;
    assert_eq!(
        wire(&error),
        r#"{"code":"internal","details":{"machine_id":"0123456789abcdef0123456789abcdef","reply_lost":true},"message":"transport error"}"#
    );
}

#[tokio::test]
async fn set_management_client_whose_reply_is_lost_answers_with_the_transport_error() {
    let label = ployz_core::ManagementClientLabel::parse("cloud").unwrap();
    let failure =
        lost(|session| Box::pin(async move { session.set_management_client(label).await })).await;
    assert_eq!(
        wire(&failure.error),
        r#"{"code":"internal","details":{"machine_id":"0123456789abcdef0123456789abcdef","reply_lost":true},"message":"transport error"}"#
    );
    assert!(failure.unanswered, "{failure:?}");
}

#[tokio::test]
async fn mint_build_grant_whose_reply_is_lost_answers_with_the_transport_error() {
    let error = lost(|session| {
        Box::pin(async move {
            session
                .mint_build_grant(MintBuildGrantRequest {
                    preview: None,
                    repository: ployz_core::BuildGrantRepository::parse("ployz-build/app").unwrap(),
                    holder: None,
                    sealed: None,
                    rule: None,
                })
                .await
        })
    })
    .await;
    assert_eq!(
        wire(&error),
        r#"{"code":"internal","details":{"machine_id":"0123456789abcdef0123456789abcdef","reply_lost":true},"message":"transport error"}"#
    );
}
