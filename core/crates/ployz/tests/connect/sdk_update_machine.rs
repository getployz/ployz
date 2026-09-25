//! Façade tests for Machine policy edits (Machine Roles and build concurrency).

use std::time::Duration;

use ployz_core::{BuildConcurrency, BuildConcurrencyUpdate, MachineUpdate, RpcErrorCode};
use tokio::time::timeout;

use super::support::{DiscoveryService, machine};
use super::unix_session::{self, UnixSession};

#[tokio::test]
async fn update_machine_returns_the_updated_record_and_rejects_non_targets() {
    let description = super::sdk::advertised_description();
    let worker = machine('b', "worker");
    let session = UnixSession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![worker];
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = timeout(
        Duration::from_secs(5),
        unix_session::connect(&session.directory, description.machine_id.as_str()),
    )
    .await
    .expect("connect must not hang")
    .unwrap();

    let two = BuildConcurrency::parse("2").unwrap();
    let updated = client
        .update_machine(
            "worker",
            MachineUpdate {
                accepts_builds: Some(false),
                build_concurrency: BuildConcurrencyUpdate::Set(two),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!updated.machine.accepts_builds);
    assert_eq!(updated.machine.build_concurrency, Some(two));

    let invalid = client
        .update_machine("*", MachineUpdate::default())
        .await
        .unwrap_err();
    assert_eq!(invalid.code, RpcErrorCode::InvalidArgument);
}
