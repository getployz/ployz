//! Which moves a signed-out Drain can't make: the copies it would start on a Server that
//! enforces Device approval.

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerObservationParts,
    ContainerRuntimeObservation, CopyRotation, HealthObservation, Machine, MachineFailure,
    MachineId, MachineName, MachineObservation, MembershipObservation, Namespace, QualifiedService,
    ResolvedServiceSpec, RpcError, RpcErrorCode, WireGuardPublicKey,
};
use serde_json::json;

use super::{DrainScope, unsigned_moves};
use crate::deploy::DeploySnapshot;

/// web-b is the Server drained.
fn drained() -> MachineId {
    id('b')
}

fn id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

fn server(hex: char, accepts_services: bool, enforcing: bool) -> MachineObservation {
    let mut machine = Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services,
        accepts_ingress: false,
        id: id(hex),
        name: MachineName::parse(format!("web-{hex}")).unwrap(),
        subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
            .parse()
            .unwrap(),
        public_key: WireGuardPublicKey([hex as u8; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        join_approval: None,
        build_concurrency: None,
    };
    machine.runtime.device_enforcement = Some(enforcing);
    MachineObservation::new(machine, MembershipObservation::Up)
}

const SEALED: &str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";

fn api(environment: serde_json::Value) -> ResolvedServiceSpec {
    serde_json::from_value(json!({
        "service_id": "a".repeat(32),
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 2 },
        "container": {
            "image": "alpine:3.23.3",
            "pull_policy": "missing",
            "environment": environment,
        },
    }))
    .unwrap()
}

fn plain() -> ResolvedServiceSpec {
    api(json!({ "TOKEN": "plain" }))
}

/// A copy that reaches data, so that only where it goes can spare it a signature.
fn sealed() -> ResolvedServiceSpec {
    api(json!({ "TOKEN": { "sealed": SEALED, "scope": "shared", "key": "TOKEN" } }))
}

fn copy(
    hex: char,
    on: char,
    rotation: Option<CopyRotation>,
    spec: ResolvedServiceSpec,
) -> ContainerObservation {
    ContainerObservation::try_from(ContainerObservationParts {
        rotation,
        r#gen: None,
        container_id: ContainerId::parse(hex.to_string().repeat(64)).unwrap(),
        display_name: "api".into(),
        created_at_unix_nanos: 0,
        machine_id: id(on),
        namespace: Namespace::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: spec,
        address: None,
        labels: Default::default(),
        stopped: false,
        run: None,
        log_driver: None,
        admission: None,
        replaced_by: None,
    })
    .unwrap()
}

fn unsigned(snapshot: DeploySnapshot) -> Result<Vec<String>, String> {
    unsigned_moves(snapshot, &drained(), &DrainScope::EveryNamespace)
        .map(|services| services.iter().map(QualifiedService::to_string).collect())
}

#[test]
fn a_copy_moving_to_another_server_needs_its_signed_version() {
    // Even one that reaches no data: the move copies its image in under pull policy
    // never, which a Server runs only under a signature.
    let snapshot = DeploySnapshot {
        machines: vec![server('a', true, true), server('b', true, false)],
        containers: vec![copy('1', 'b', None, plain())],
        ..DeploySnapshot::default()
    };
    assert_eq!(unsigned(snapshot), Ok(vec!["app/api".to_owned()]));
}

#[test]
fn an_old_copy_the_drain_only_cleans_up_needs_nothing() {
    let snapshot = DeploySnapshot {
        machines: vec![server('a', true, true), server('b', true, false)],
        containers: vec![
            copy('1', 'a', Some(CopyRotation::In), sealed()),
            copy('2', 'b', Some(CopyRotation::Out), sealed()),
        ],
        ..DeploySnapshot::default()
    };
    assert_eq!(unsigned(snapshot), Ok(Vec::new()));
}

#[test]
fn a_drained_server_that_did_not_list_its_containers_refuses_rather_than_guess() {
    let mut snapshot = DeploySnapshot {
        machines: vec![server('a', true, true), server('b', true, false)],
        ..DeploySnapshot::default()
    };
    snapshot.container_failures.push(MachineFailure {
        machine_id: drained(),
        error: RpcError::new(RpcErrorCode::Unavailable, "timed out"),
    });
    assert!(unsigned(snapshot).is_err());
}
