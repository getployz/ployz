use futures_util::StreamExt;
use ployz_core::deploy::VolumeName;
use ployz_core::machine::{IssuedJoinToken, JoinTokenExpiresAt, RawJoinToken};
use ployz_core::roles::InstallRolePolicy;
use ployz_core::state::{
    ActiveMachineState, ControlPlaneEpoch, IntentSnapshot, MachineLifecycle,
    PendingMachineJoinRecoverySnapshot, RouteBindingState, StagedMachineDataplaneState,
    VolumePinState,
};
use ployz_core::subjects::{INTENT_CHANGED, PENDING_MACHINE_JOINS_CHANGED};
use ployz_test_support::fixtures::{machine_join_bundle, serving_target_entry};
use ployz_test_support::ids::{
    idempotency_key, machine_id, machine_name, operation_id, service_id,
};
use ployzd::core_store::CoreStore;
use ployzd::intent::machine_roster::MachineRosterStore;
use ployzd::intent::namespace_intent::NamespaceIntentStore;
use ployzd::intent::service::{NatsIntentReader, start_intent_service};
use ployzd::operations::log::{
    MachineAddOperationSubmission, MachineJoinIdentity, OperationRepository,
};
use std::time::Duration;

mod support;

#[tokio::test]
async fn intent_runtime_rebroadcasts_full_intent_on_the_drumbeat() {
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store opens");
    support::intent::initialize_disabled_ingress(&core_store).await;
    let machine_roster = MachineRosterStore::new(core_store.clone());
    machine_roster
        .replace_active_machine(&ActiveMachineState {
            control_endpoints: Vec::new(),
            mesh_endpoints: Vec::new(),
            machine_id: machine_id("machine_a"),
            name: ployz_core::machine::MachineName::try_new("machine_a")
                .expect("valid machine name"),
            activated_by: operation_id("op_machine_add"),
            roles: ployz_core::roles::InstallRolePolicy::install_all(),
            lifecycle: MachineLifecycle::Active,
            endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new("10.198.0.0/24")
                .expect("valid endpoint subnet"),
            wireguard_public_key: wireguard_public_key("public-machine-a"),
        })
        .await
        .expect("active machine stores");
    let mut changed = nats
        .machine_client(&machine_id("machine_a"))
        .await
        .subscribe(INTENT_CHANGED)
        .await
        .expect("subscribe intent changes");
    let _runtime = start_intent_service(
        nats.controller.clone(),
        machine_id("machine_a"),
        NamespaceIntentStore::new(core_store.clone()),
        core_store,
        Duration::from_millis(10),
    )
    .await
    .expect("intent runtime starts");

    let message = tokio::time::timeout(Duration::from_secs(1), changed.next())
        .await
        .expect("intent rebroadcast arrives")
        .expect("intent message exists");
    let intent: IntentSnapshot =
        serde_json::from_slice(&message.payload).expect("intent snapshot decodes");

    assert_eq!(intent.active_machines.len(), 1);
    assert_eq!(
        intent
            .active_machines
            .first()
            .expect("one active machine")
            .machine_id,
        machine_id("machine_a")
    );
}

#[tokio::test]
async fn intent_reader_gets_current_intent() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store opens");
    support::intent::initialize_disabled_ingress(&core_store).await;
    let _runtime = start_intent_service(
        nats.controller.clone(),
        machine_id("machine_a"),
        temp_namespace_intent().await,
        core_store,
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    let intent = NatsIntentReader::new(nats.controller)
        .with_request_timeout(Duration::from_secs(1))
        .intent()
        .await
        .expect("intent reads");

    assert!(intent.active_machines.is_empty());
    assert!(intent.route_bindings.is_empty());
    assert!(intent.serving_target_entries.is_empty());
}

#[tokio::test]
async fn machine_reads_core_stamped_projection_with_one_staged_joiner() {
    let machine = machine_id("machine_a");
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(std::slice::from_ref(&machine))
            .await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    support::intent::initialize_disabled_ingress(&core_store).await;
    let repository = OperationRepository::open(core_store.clone(), nats.controller.clone());
    let raw_join_token = RawJoinToken::try_new("join_machine_a").expect("join token");
    let operation = operation_id("op_add_machine_a");
    repository
        .submit_machine_add(MachineAddOperationSubmission {
            operation_id: operation.clone(),
            idempotency_key: idempotency_key("idem_add_machine_a"),
            identity: MachineJoinIdentity {
                machine_id: machine.clone(),
                name: machine_name("machine-a"),
                roles: InstallRolePolicy::install_all(),
                host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
                endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new(
                    "10.198.1.0/24",
                )
                .expect("subnet"),
                join_bundle: machine_join_bundle(),
                join_token: IssuedJoinToken::new(
                    raw_join_token.fingerprint().expect("fingerprint"),
                    JoinTokenExpiresAt::try_new(4_102_444_800).expect("expiry"),
                ),
                raw_join_token,
            },
        })
        .await
        .expect("submit");
    repository
        .record_machine_add_joined(
            &operation,
            &machine,
            ployz_core::machine::JoinTokenRedeemedAt::try_new(1).expect("joined at"),
        )
        .await
        .expect("joining");
    repository
        .stage_machine_dataplane(StagedMachineDataplaneState {
            operation_id: operation,
            machine_id: machine.clone(),
            endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new("10.198.1.0/24")
                .expect("subnet"),
            mesh_endpoints: vec!["192.0.2.1:51820".parse().expect("endpoint")],
            wireguard_public_key: wireguard_public_key("public-machine-a"),
        })
        .await
        .expect("stage");
    let _runtime = start_intent_service(
        nats.controller.clone(),
        machine.clone(),
        NamespaceIntentStore::new(core_store.clone()),
        core_store,
        Duration::from_secs(30),
    )
    .await
    .expect("intent service");

    let projection = NatsIntentReader::new(nats.machine_client(&machine).await)
        .with_request_timeout(Duration::from_secs(1))
        .intent()
        .await
        .expect("intent")
        .dataplane_projection;

    assert!(projection.declared_members().is_empty());
    assert_eq!(projection.target_members().len(), 1);
    assert_ne!(projection.declared_revision(), projection.target_revision());

    repository
        .record_machine_add_failed(
            &operation_id("op_add_machine_a"),
            &machine,
            ployz_core::machine::MachineAddFailure::BootstrapFailed {
                message: ployz_core::ops::FailureMessage::try_new("failed")
                    .expect("failure message"),
            },
        )
        .await
        .expect("fail operation");
    let projection = NatsIntentReader::new(nats.machine_client(&machine).await)
        .with_request_timeout(Duration::from_secs(1))
        .intent()
        .await
        .expect("intent after failure")
        .dataplane_projection;
    assert!(projection.target_members().is_empty());
}

#[tokio::test]
async fn intent_runtime_publishes_redeemable_pending_machine_joins() {
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store opens");
    support::intent::initialize_disabled_ingress(&core_store).await;
    let repository = OperationRepository::open(core_store.clone(), nats.controller.clone());
    let raw_join_token = RawJoinToken::try_new("join_pending_machine_a").expect("valid token");
    let join_token = IssuedJoinToken::new(
        raw_join_token.fingerprint().expect("fingerprint"),
        JoinTokenExpiresAt::try_new(4_102_444_800).expect("expiry"),
    );
    let operation_id = operation_id("op_add_machine_a");
    let idempotency_key = idempotency_key("idem_add_machine_a");
    repository
        .submit_machine_add(MachineAddOperationSubmission {
            operation_id: operation_id.clone(),
            idempotency_key: idempotency_key.clone(),
            identity: MachineJoinIdentity {
                machine_id: machine_id("machine_a"),
                name: machine_name("machine-a"),
                roles: InstallRolePolicy::install_all(),
                host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
                endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new(
                    "10.198.2.0/24",
                )
                .expect("valid subnet"),
                join_bundle: machine_join_bundle(),
                join_token,
                raw_join_token,
            },
        })
        .await
        .expect("machine-add submits");
    let mut pending_changed = nats
        .machine_client(&machine_id("machine_a"))
        .await
        .subscribe(PENDING_MACHINE_JOINS_CHANGED)
        .await
        .expect("subscribe pending joins");
    let _runtime = start_intent_service(
        nats.controller.clone(),
        machine_id("machine_a"),
        temp_namespace_intent().await,
        core_store,
        Duration::from_millis(10),
    )
    .await
    .expect("intent runtime starts");

    let message = tokio::time::timeout(Duration::from_secs(1), pending_changed.next())
        .await
        .expect("pending join signal arrives")
        .expect("pending join message exists");
    let snapshot: PendingMachineJoinRecoverySnapshot =
        serde_json::from_slice(&message.payload).expect("pending joins decode");

    assert_eq!(snapshot.epoch, ControlPlaneEpoch::initial());
    assert_eq!(snapshot.pending.len(), 1);
    assert_eq!(
        snapshot
            .pending
            .first()
            .expect("one pending join")
            .machine_id,
        machine_id("machine_a")
    );
}

#[tokio::test]
async fn intent_reader_overlays_machine_lifecycle_evidence() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store opens");
    support::intent::initialize_disabled_ingress(&core_store).await;
    let machine_roster = MachineRosterStore::new(core_store.clone());
    machine_roster
        .replace_active_machine(&ActiveMachineState {
            control_endpoints: Vec::new(),
            mesh_endpoints: Vec::new(),
            machine_id: machine_id("machine_a"),
            name: ployz_core::machine::MachineName::try_new("machine_a")
                .expect("valid machine name"),
            activated_by: operation_id("op_machine_add"),
            roles: ployz_core::roles::InstallRolePolicy::install_all(),
            lifecycle: MachineLifecycle::Draining,
            endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new("10.198.0.0/24")
                .expect("valid endpoint subnet"),
            wireguard_public_key: wireguard_public_key("public-machine-a"),
        })
        .await
        .expect("active machine stores");
    let _runtime = start_intent_service(
        nats.controller.clone(),
        machine_id("machine_a"),
        NamespaceIntentStore::new(core_store.clone()),
        core_store,
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    let intent = NatsIntentReader::new(nats.controller)
        .with_request_timeout(Duration::from_secs(1))
        .intent()
        .await
        .expect("intent reads");

    assert_eq!(
        intent
            .active_machines
            .first()
            .expect("one active machine")
            .lifecycle,
        MachineLifecycle::Draining
    );
}

#[tokio::test]
async fn intent_reader_gets_namespace_intent_from_file() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let namespace_intent = temp_namespace_intent().await;
    namespace_intent
        .replace_serving_target_entry(serving_target_entry("svc_api", "entry_api"))
        .await
        .expect("serving target stores");
    namespace_intent
        .replace_route_binding(RouteBindingState {
            id: route_binding_id("api.example.com"),
            namespace_id: ployz_test_support::ids::namespace_id("default"),
            target: route_target("api.example.com", 443),
            endpoint_port: ployz_core::ops::RoutePort::try_new(8080).expect("valid route port"),
            service_id: service_id("svc_api"),
            origin: ployz_core::ingress::RouteBindingOrigin::Declared,
        })
        .await
        .expect("route binding stores");
    namespace_intent
        .replace_volume_pin(VolumePinState {
            namespace_id: ployz_test_support::ids::namespace_id("default"),
            volume_name: volume_name("postgres_data"),
            machine_id: machine_id("machine_a"),
        })
        .await
        .expect("volume pin stores");
    let core_store = CoreStore::open_in_memory().await.expect("core store opens");
    support::intent::initialize_disabled_ingress(&core_store).await;
    let _runtime = start_intent_service(
        nats.controller.clone(),
        machine_id("machine_a"),
        namespace_intent,
        core_store,
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    let intent = NatsIntentReader::new(nats.controller)
        .with_request_timeout(Duration::from_secs(1))
        .intent()
        .await
        .expect("intent reads");

    assert_eq!(intent.route_bindings.len(), 1);
    assert_eq!(intent.serving_target_entries.len(), 1);
    assert_eq!(
        intent.volume_pins,
        vec![VolumePinState {
            namespace_id: ployz_test_support::ids::namespace_id("default"),
            volume_name: volume_name("postgres_data"),
            machine_id: machine_id("machine_a"),
        }]
    );
}

#[tokio::test]
async fn namespace_intent_store_persists_volume_pins() {
    let namespace_intent = temp_namespace_intent().await;
    let state = VolumePinState {
        namespace_id: ployz_test_support::ids::namespace_id("default"),
        volume_name: volume_name("postgres_data"),
        machine_id: machine_id("machine_a"),
    };

    namespace_intent
        .replace_volume_pin(state.clone())
        .await
        .expect("volume pin stores");

    assert_eq!(
        namespace_intent
            .load()
            .await
            .expect("namespace intent loads")
            .volume_pins,
        vec![state]
    );
}

#[tokio::test]
async fn namespace_intent_store_commits_a_deploy_phase_atomically() {
    let namespace_intent = temp_namespace_intent().await;
    let route = RouteBindingState {
        id: route_binding_id("api.example.com"),
        namespace_id: ployz_test_support::ids::namespace_id("default"),
        target: route_target("api.example.com", 443),
        endpoint_port: ployz_core::ops::RoutePort::try_new(8080).expect("valid route port"),
        service_id: service_id("svc_api"),
        origin: ployz_core::ingress::RouteBindingOrigin::Declared,
    };
    let old_route = RouteBindingState {
        id: route_binding_id("old.example.com"),
        namespace_id: ployz_test_support::ids::namespace_id("default"),
        target: route_target("old.example.com", 443),
        endpoint_port: ployz_core::ops::RoutePort::try_new(8080).expect("valid route port"),
        service_id: service_id("svc_api"),
        origin: ployz_core::ingress::RouteBindingOrigin::Declared,
    };
    let serving = serving_target_entry("svc_api", "entry_api");

    namespace_intent
        .replace_route_binding(old_route.clone())
        .await
        .expect("old route stores");

    namespace_intent
        .commit_deploy_phase(
            vec![route.clone()],
            vec![old_route.target],
            vec![serving.clone()],
        )
        .await
        .expect("phase commits");

    let stored = namespace_intent
        .load()
        .await
        .expect("namespace intent loads");
    assert_eq!(stored.route_bindings, vec![route]);
    assert_eq!(stored.serving_target_entries, vec![serving]);
}

async fn temp_namespace_intent() -> NamespaceIntentStore {
    NamespaceIntentStore::new(
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    )
}

fn wireguard_public_key(value: &str) -> ployz_core::dataplane::WireGuardPublicKey {
    ployz_core::dataplane::WireGuardPublicKey::try_new(value).expect("wireguard public key")
}

fn route_target(hostname: &str, _port: u16) -> ployz_core::ops::RouteTarget {
    ployz_core::ops::RouteTarget::new(
        ployz_core::ops::RouteHostname::try_new(hostname).expect("valid route hostname"),
    )
}

fn route_binding_id(hostname: &str) -> ployz_core::ids::RouteBindingId {
    ployz_core::ids::RouteBindingId::try_new(format!("route_{}", hostname.replace('.', "_")))
        .expect("valid route binding id")
}

fn volume_name(value: &str) -> VolumeName {
    VolumeName::try_new(value).expect("valid volume name")
}
