//! Façade tests for Drain: its preflight refusals and its scope.

use std::time::Duration;

use ployz::drain::{DrainOutcome, DrainScope, Remaining, ServicesRole, StayReason};
use ployz_core::{
    ContainerKind, ContainerRuntimeObservation, HealthObservation, QualifiedService, RpcErrorCode,
};
use tokio::time::timeout;

use super::support::{DiscoveryService, machine};
use super::unix_session::{self, UnixSession};

#[tokio::test]
async fn an_empty_owned_scope_turns_the_role_off_and_touches_nothing() {
    let description = super::sdk::advertised_description();
    let mut cordoned = machine('a', "one");
    cordoned.machine.accepts_services = false;
    let session = UnixSession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![cordoned];
    service
        .listed_containers
        .lock()
        .unwrap()
        .push(super::listing_container(
            'c',
            'c',
            "api",
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
        ));
    let inspected = service.inspect_calls.clone();
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = timeout(
        Duration::from_secs(5),
        unix_session::connect(&session.directory, description.machine_id.as_str()),
    )
    .await
    .expect("connect must not hang")
    .unwrap();
    let before = inspected.load(std::sync::atomic::Ordering::SeqCst);

    let report = client
        .drain_machine(
            "one",
            &DrainScope::Owned {
                namespaces: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(report.services_role, ServicesRole::AlreadyOff);
    assert!(report.services.is_empty(), "{report:?}");
    assert_eq!(report.stopped, None);
    assert_eq!(
        report.remaining,
        Remaining::Observed {
            services: vec![QualifiedService::parse("app/api").unwrap()]
        }
    );
    // Neither convergence nor Global slot convergence ran: both inspect a Machine first.
    assert_eq!(inspected.load(std::sync::atomic::Ordering::SeqCst), before);

    let every = client
        .drain_machine("one", &DrainScope::EveryNamespace)
        .await
        .unwrap();
    let [entry] = every.services.as_slice() else {
        panic!("{every:?}");
    };
    assert!(
        matches!(
            entry.outcome,
            DrainOutcome::Stays {
                reason: StayReason::NoDestination { .. }
            }
        ),
        "{entry:?}"
    );
    assert!(inspected.load(std::sync::atomic::Ordering::SeqCst) > before);

    let missing = client
        .drain_machine("nowhere", &DrainScope::EveryNamespace)
        .await
        .unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::NotFound);
    let invalid = client
        .drain_machine("*", &DrainScope::EveryNamespace)
        .await
        .unwrap_err();
    assert_eq!(invalid.code, RpcErrorCode::InvalidArgument);

    client.close().await;
    let closed = client
        .drain_machine("one", &DrainScope::EveryNamespace)
        .await
        .unwrap_err();
    assert_eq!(closed.code, RpcErrorCode::Unavailable);
}
