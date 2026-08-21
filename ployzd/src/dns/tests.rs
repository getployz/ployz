//! Projection tests for Internal DNS answers.

use std::collections::BTreeMap;

use ployz_core::{
    ContainerAddress, ContainerId, ContainerKind, ContainerRuntimeObservation, HealthObservation,
    MachineId, ProjectName, ResolvedServiceSpec, ServiceId, ServiceName,
};
use serde_json::json;

use super::*;

const SUBNET: &str = "10.210.1.0/24";

#[test]
fn projects_only_eligible_service_container_addresses() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let service = ServiceId::parse("b".repeat(32)).unwrap();
    let name = ServiceName::parse("api").unwrap();
    let observations = [
        observation(
            1,
            &machine,
            &service,
            &name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 2]),
        ),
        observation(
            2,
            &machine,
            &service,
            &name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::NotConfigured),
            Some([10, 210, 1, 3]),
        ),
        observation(
            3,
            &machine,
            &service,
            &name,
            ContainerKind::PreDeployHook,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 4]),
        ),
        observation(
            4,
            &machine,
            &service,
            &name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Starting),
            Some([10, 210, 1, 5]),
        ),
        observation(
            5,
            &machine,
            &service,
            &name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            None,
        ),
    ];

    assert_eq!(
        addresses(plan(
            &Projection::from_observations(&observations),
            "api.app.internal.",
            RecordType::A,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2), Ipv4Addr::new(10, 210, 1, 3)]
    );
}

#[test]
fn resolves_every_canonical_lookup_and_rotates_ordinary_answers() {
    let local = MachineId::parse("a".repeat(32)).unwrap();
    let remote = MachineId::parse("c".repeat(32)).unwrap();
    let first = ServiceId::parse("b".repeat(32)).unwrap();
    let second = ServiceId::parse("d".repeat(32)).unwrap();
    let name = ServiceName::parse("api").unwrap();
    let projection = Projection::from_observations(&[
        observation(
            1,
            &remote,
            &first,
            &name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 2, 2]),
        ),
        observation(
            2,
            &local,
            &second,
            &name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 2]),
        ),
    ]);

    let ordinary = "api.app.internal.";
    assert_eq!(
        addresses(plan(&projection, ordinary, RecordType::A)),
        vec![Ipv4Addr::new(10, 210, 2, 2), Ipv4Addr::new(10, 210, 1, 2)]
    );
    assert_eq!(
        addresses(plan(
            &projection,
            &format!("{first}.id.lookup.internal."),
            RecordType::A,
        )),
        vec![Ipv4Addr::new(10, 210, 2, 2)]
    );
    assert_eq!(
        addresses(plan(&projection, ordinary, RecordType::A)),
        vec![Ipv4Addr::new(10, 210, 1, 2), Ipv4Addr::new(10, 210, 2, 2)]
    );
    assert_eq!(
        addresses(plan(
            &projection,
            "api.app.nearest.internal.",
            RecordType::A,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2), Ipv4Addr::new(10, 210, 2, 2)]
    );
    assert_eq!(
        addresses(plan(
            &projection,
            &format!("api.app.{local}.machine.internal."),
            RecordType::A,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2)]
    );
    assert_nxdomain(plan(
        &projection,
        "api.app.eu-west.region.internal.",
        RecordType::A,
    ));
}

#[test]
fn cross_project_and_reserved_names_are_resolved_structurally() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let words = ["rr", "nearest", "machine", "region", "id", "lookup"];
    let mut observations = Vec::new();
    for (index, word) in words.into_iter().enumerate() {
        observations.push(in_project(
            observation(
                index as u8 + 1,
                &machine,
                &ServiceId::parse(format!("{:032x}", index + 1)).unwrap(),
                &ServiceName::parse(word).unwrap(),
                ContainerKind::ServiceContainer,
                running(HealthObservation::Healthy),
                Some([10, 210, 1, index as u8 + 2]),
            ),
            "project",
        ));
        observations.push(in_project(
            observation(
                index as u8 + 9,
                &machine,
                &ServiceId::parse(format!("{:032x}", index + 16)).unwrap(),
                &ServiceName::parse("service").unwrap(),
                ContainerKind::ServiceContainer,
                running(HealthObservation::Healthy),
                Some([10, 210, 2, index as u8 + 2]),
            ),
            word,
        ));
    }
    let projection = Projection::from_observations(&observations);

    for (index, word) in words.into_iter().enumerate() {
        assert_eq!(
            addresses(plan(
                &projection,
                &format!("{word}.project.internal."),
                RecordType::A,
            )),
            vec![Ipv4Addr::new(10, 210, 1, index as u8 + 2)]
        );
        assert_eq!(
            addresses(plan(
                &projection,
                &format!("service.{word}.internal."),
                RecordType::A,
            )),
            vec![Ipv4Addr::new(10, 210, 2, index as u8 + 2)]
        );
    }
    assert_nxdomain(plan(&projection, "service.internal.", RecordType::A));
}

#[test]
fn exact_selectors_do_not_fall_back_when_their_endpoint_is_ineligible() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let selected_id = ServiceId::parse("b".repeat(32)).unwrap();
    let colliding_id = ServiceId::parse("c".repeat(32)).unwrap();
    let colliding_name = ServiceName::parse(selected_id.to_string()).unwrap();
    let projection = Projection::from_observations(&[
        observation(
            1,
            &machine,
            &selected_id,
            &ServiceName::parse("selected").unwrap(),
            ContainerKind::ServiceContainer,
            running(HealthObservation::Unhealthy),
            Some([10, 210, 1, 2]),
        ),
        observation(
            2,
            &machine,
            &colliding_id,
            &colliding_name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 3]),
        ),
    ]);

    assert_eq!(
        addresses(plan(
            &projection,
            &format!("{selected_id}.app.internal."),
            RecordType::A,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 3)]
    );
    assert_nxdomain(plan(
        &projection,
        &format!("{selected_id}.id.lookup.internal."),
        RecordType::A,
    ));
    assert_nxdomain(plan(
        &projection,
        &format!("selected.app.{}.machine.internal.", "d".repeat(32)),
        RecordType::A,
    ));
}

#[test]
fn malformed_internal_and_non_a_queries_are_authoritative() {
    let projection = Projection::from_observations(&[]);

    for name in [
        "internal.",
        "missing.internal.",
        "rr.api.app.internal.",
        "not-an-id.id.lookup.internal.",
    ] {
        assert_nxdomain(plan(&projection, name, RecordType::A));
    }
    for record_type in [RecordType::AAAA, RecordType::SRV, RecordType::TXT] {
        assert_eq!(
            plan(&projection, "api.app.internal.", record_type),
            ResponsePlan::Internal {
                code: ResponseCode::NoError,
                answers: Vec::new(),
            }
        );
    }
    assert_eq!(
        plan(&projection, "example.com.", RecordType::A),
        ResponsePlan::Forward
    );
}

#[test]
fn explicit_upstreams_override_system_configuration() {
    let upstreams = vec!["192.0.2.53:5353".parse().unwrap()];
    assert_eq!(
        configured_upstreams(Some(upstreams.clone()), Ipv4Addr::new(10, 210, 1, 1)),
        upstreams
    );
}

#[test]
fn parses_resolv_conf_nameservers_skips_listen_and_junk() {
    let text = "\
# generated
search internal example.test
nameserver 192.0.2.53
nameserver 10.210.1.1
nameserver 2001:db8::53
nameserver not-an-ip
options ndots:1
\tnameserver 198.51.100.53 # trailing comment
";
    assert_eq!(
        nameservers_from_resolv_conf(text, Ipv4Addr::new(10, 210, 1, 1)),
        vec![
            "192.0.2.53:53".parse().unwrap(),
            "[2001:db8::53]:53".parse().unwrap(),
            "198.51.100.53:53".parse().unwrap(),
        ]
    );
}

fn plan(projection: &Projection, name: &str, record_type: RecordType) -> ResponsePlan {
    projection.plan(
        &Name::from_ascii(name).unwrap(),
        record_type,
        SUBNET.parse().unwrap(),
    )
}

fn running(health: HealthObservation) -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running { health }
}

fn observation(
    suffix: u8,
    machine_id: &MachineId,
    service_id: &ServiceId,
    service_name: &ServiceName,
    kind: ContainerKind,
    runtime: ContainerRuntimeObservation,
    address: Option<[u8; 4]>,
) -> ContainerObservation {
    ContainerObservation {
        container_id: ContainerId::parse(format!("{suffix:x}").repeat(64)).unwrap(),
        display_name: format!("{service_name}-{suffix}"),
        created_at_unix_nanos: 0,
        machine_id: *machine_id,
        project_name: ProjectName::parse("app").unwrap(),
        service_id: *service_id,
        service_name: service_name.clone(),
        kind,
        runtime,
        effective_healthcheck: None,
        resolved_spec: fixture_spec(service_id, service_name),
        address: address.map(|octets| ContainerAddress(octets.into())),
        labels: BTreeMap::new(),
    }
}

fn in_project(mut observation: ContainerObservation, project: &str) -> ContainerObservation {
    observation.project_name = ProjectName::parse(project).unwrap();
    observation
}

fn fixture_spec(service_id: &ServiceId, service_name: &ServiceName) -> ResolvedServiceSpec {
    serde_json::from_value(json!({
        "service_id": service_id,
        "name": service_name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "example.test/image", "pull_policy": "missing" }
    }))
    .unwrap()
}

fn addresses(plan: ResponsePlan) -> Vec<Ipv4Addr> {
    let ResponsePlan::Internal { answers, .. } = plan else {
        panic!("expected internal answer")
    };
    answers
        .into_iter()
        .map(|record| {
            let Some(IpAddr::V4(address)) = record.data.ip_addr() else {
                panic!("expected A record, got {:?}", record.data)
            };
            address
        })
        .collect()
}

fn assert_nxdomain(plan: ResponsePlan) {
    assert!(matches!(
        plan,
        ResponsePlan::Internal {
            code: ResponseCode::NXDomain,
            answers,
        } if answers.is_empty()
    ));
}
