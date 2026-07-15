use crate::control::operation_evidence::{
    ManagedDnsReconcileOperationSubmission, OperationRepository,
};
use crate::control::operator_api::ops_list;
use crate::control::sequencer::{MachineAddBootstrapConfig, OperationControllers};
use crate::control::store::CoreStore;
use ployz_core::install::{DEFAULT_MACHINE_BOOTSTRAP_URL, MachineBootstrapUrl};
use ployz_core::operation::{
    ManagedDnsReconcileSubject, ManagedDnsReconcileTransition, OperationStatusSnapshot,
};
use ployz_sdk_types::{OpsListError, OpsListRequest};
use ployz_test_support::ids::operation_id;

#[tokio::test]
async fn operations_before_boundary_are_returned_newest_first() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let directory = tempfile::tempdir().expect("operation store directory");
    let store = CoreStore::open(directory.path().join("core.db"))
        .await
        .expect("open operation store");
    let repository = OperationRepository::open(store, nats.controller);
    for id in ["op_oldest", "op_older", "op_boundary", "op_newest"] {
        repository
            .submit_managed_dns_reconcile(ManagedDnsReconcileOperationSubmission {
                operation_id: operation_id(id),
                subject: ManagedDnsReconcileSubject::Acquire,
            })
            .await
            .expect("submit operation");
    }
    let controllers = OperationControllers::new(
        repository,
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("valid bootstrap URL"),
        ),
    );

    let result = ops_list(
        &controllers,
        OpsListRequest {
            active_only: false,
            before: Some(operation_id("op_boundary")),
        },
    )
    .await
    .expect("list operations before boundary");

    assert_eq!(operation_ids(&result.operations), ["op_older", "op_oldest"]);
    assert!(!result.has_more);

    controllers
        .repository()
        .record_managed_dns_reconcile_transition(
            &operation_id("op_older"),
            &ManagedDnsReconcileSubject::Acquire,
            ManagedDnsReconcileTransition::Completed,
        )
        .await
        .expect("complete older operation");
    let active = ops_list(
        &controllers,
        OpsListRequest {
            active_only: true,
            before: Some(operation_id("op_boundary")),
        },
    )
    .await
    .expect("list active operations before boundary");
    assert_eq!(operation_ids(&active.operations), ["op_oldest"]);
}

#[tokio::test]
async fn missing_before_boundary_is_a_typed_error() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let directory = tempfile::tempdir().expect("operation store directory");
    let store = CoreStore::open(directory.path().join("core.db"))
        .await
        .expect("open operation store");
    let controllers = OperationControllers::new(
        OperationRepository::open(store, nats.controller),
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("valid bootstrap URL"),
        ),
    );

    let error = ops_list(
        &controllers,
        OpsListRequest {
            active_only: false,
            before: Some(operation_id("op_missing")),
        },
    )
    .await
    .expect_err("missing boundary is rejected");

    assert_eq!(
        error,
        OpsListError::NoSuchOperation {
            operation_id: operation_id("op_missing"),
        }
    );
}

fn operation_ids(operations: &[OperationStatusSnapshot]) -> Vec<&str> {
    operations
        .iter()
        .map(|snapshot| snapshot.status.id().as_str())
        .collect()
}
