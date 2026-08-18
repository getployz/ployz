//! Projection tests for Internal DNS answers.

use std::collections::BTreeMap;

use ployz_core::{
    ContainerAddress, ContainerId, ContainerKind, ContainerRuntimeObservation, HealthObservation,
    MachineId, ProjectName, ResolvedServiceSpec, ServiceId, ServiceName,
};
use serde_json::json;

use super::*;

const HOST: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

#[test]
fn does_not_publish_hook_container_addresses() {
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
            ContainerKind::PreDeployHook,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 4]),
        ),
    ];

    assert_eq!(
        addresses(Projection::from_observations(&observations).plan(
            &Name::from_ascii("api.app.internal.").unwrap(),
            RecordType::A,
            "10.210.1.0/24".parse().unwrap(),
            HOST,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2)]
    );
}

#[test]
fn projects_only_healthy_addressed_service_containers() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let service = ServiceId::parse("b".repeat(32)).unwrap();
    let name = ServiceName::parse("api").unwrap();
    let mut observations = vec![
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
    ];
    for (suffix, kind, runtime, address) in [
        (
            3,
            ContainerKind::PreDeployHook,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 4]),
        ),
        (
            4,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Starting),
            Some([10, 210, 1, 5]),
        ),
        (
            5,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Unhealthy),
            Some([10, 210, 1, 6]),
        ),
        (
            6,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Exited { code: 0 },
            Some([10, 210, 1, 7]),
        ),
        (
            7,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            None,
        ),
    ] {
        observations.push(observation(
            suffix, &machine, &service, &name, kind, runtime, address,
        ));
    }

    assert_eq!(
        addresses(Projection::from_observations(&observations).plan(
            &Name::from_ascii("api.app.internal.").unwrap(),
            RecordType::A,
            "10.210.1.0/24".parse().unwrap(),
            HOST,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2), Ipv4Addr::new(10, 210, 1, 3)]
    );
}

#[test]
fn resolves_name_id_machine_and_nearest_without_dropping_duplicates() {
    let local = MachineId::parse("a".repeat(32)).unwrap();
    let remote = MachineId::parse("c".repeat(32)).unwrap();
    let first = ServiceId::parse("b".repeat(32)).unwrap();
    let second = ServiceId::parse("d".repeat(32)).unwrap();
    let name = ServiceName::parse("api").unwrap();
    let observations = vec![
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
    ];
    let projection = Projection::from_observations(&observations);
    let subnet = "10.210.1.0/24".parse().unwrap();
    let caller = IpAddr::V4(Ipv4Addr::new(10, 210, 1, 2));

    assert_eq!(
        address_set(projection.plan(
            &Name::from_ascii("api.internal.").unwrap(),
            RecordType::A,
            subnet,
            caller,
        )),
        [Ipv4Addr::new(10, 210, 1, 2), Ipv4Addr::new(10, 210, 2, 2)].into()
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii(format!("{first}.internal.")).unwrap(),
            RecordType::A,
            subnet,
            HOST,
        )),
        vec![Ipv4Addr::new(10, 210, 2, 2)]
    );
    assert!(
        addresses(projection.plan(
            &Name::from_ascii(format!("{local}.m.api.internal.")).unwrap(),
            RecordType::A,
            subnet,
            HOST,
        ))
        .is_empty()
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii(format!("{local}.m.api.internal.")).unwrap(),
            RecordType::A,
            subnet,
            caller,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2)]
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii(format!("{local}.m.api.app.internal.")).unwrap(),
            RecordType::A,
            subnet,
            HOST,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2)]
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii("nearest.api.internal.").unwrap(),
            RecordType::A,
            subnet,
            caller,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2), Ipv4Addr::new(10, 210, 2, 2)]
    );
    assert_eq!(
        address_set(projection.plan(
            &Name::from_ascii("rr.api.internal.").unwrap(),
            RecordType::A,
            subnet,
            caller,
        )),
        [Ipv4Addr::new(10, 210, 1, 2), Ipv4Addr::new(10, 210, 2, 2)].into()
    );
}

#[test]
fn two_projects_keep_separate_qualified_answers() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let staging_id = ServiceId::parse("b".repeat(32)).unwrap();
    let prod_id = ServiceId::parse("c".repeat(32)).unwrap();
    let name = ServiceName::parse("web").unwrap();
    let mut staging = observation(
        1,
        &machine,
        &staging_id,
        &name,
        ContainerKind::ServiceContainer,
        running(HealthObservation::Healthy),
        Some([10, 210, 1, 2]),
    );
    staging.project_name = ProjectName::parse("shop-staging").unwrap();
    let mut prod = observation(
        2,
        &machine,
        &prod_id,
        &name,
        ContainerKind::ServiceContainer,
        running(HealthObservation::Healthy),
        Some([10, 210, 1, 3]),
    );
    prod.project_name = ProjectName::parse("shop-prod").unwrap();
    let projection = Projection::from_observations(&[staging, prod]);
    let subnet = "10.210.1.0/24".parse().unwrap();

    assert!(
        addresses(projection.plan(
            &Name::from_ascii("web.internal.").unwrap(),
            RecordType::A,
            subnet,
            HOST,
        ))
        .is_empty()
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii("web.shop-staging.internal.").unwrap(),
            RecordType::A,
            subnet,
            HOST,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2)]
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii("web.shop-prod.internal.").unwrap(),
            RecordType::A,
            subnet,
            HOST,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 3)]
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii(format!("{machine}.m.web.shop-staging.internal.")).unwrap(),
            RecordType::A,
            subnet,
            HOST,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2)]
    );
    assert!(
        addresses(projection.plan(
            &Name::from_ascii(format!("{machine}.m.web.internal.")).unwrap(),
            RecordType::A,
            subnet,
            HOST,
        ))
        .is_empty()
    );
    let staging_caller = IpAddr::V4(Ipv4Addr::new(10, 210, 1, 2));
    let prod_caller = IpAddr::V4(Ipv4Addr::new(10, 210, 1, 3));
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii(format!("{machine}.m.web.internal.")).unwrap(),
            RecordType::A,
            subnet,
            staging_caller,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2)]
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii(format!("{machine}.m.web.internal.")).unwrap(),
            RecordType::A,
            subnet,
            prod_caller,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 3)]
    );
}

#[test]
fn service_id_selector_takes_precedence_over_a_colliding_name() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let selected_id = ServiceId::parse("b".repeat(32)).unwrap();
    let other_id = ServiceId::parse("c".repeat(32)).unwrap();
    let colliding_name = ServiceName::parse(selected_id.to_string()).unwrap();
    let selected_name = ServiceName::parse("selected").unwrap();
    let projection = Projection::from_observations(&[
        observation(
            1,
            &machine,
            &selected_id,
            &selected_name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 2]),
        ),
        observation(
            2,
            &machine,
            &other_id,
            &colliding_name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 3]),
        ),
    ]);

    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii(format!("{selected_id}.internal.")).unwrap(),
            RecordType::A,
            "10.210.1.0/24".parse().unwrap(),
            HOST,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2)]
    );
}

#[test]
fn empty_service_id_selector_does_not_fall_back_to_a_colliding_name() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let selected_id = ServiceId::parse("b".repeat(32)).unwrap();
    let other_id = ServiceId::parse("c".repeat(32)).unwrap();
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
            &other_id,
            &colliding_name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 3]),
        ),
    ]);

    assert!(matches!(
        projection.plan(
            &Name::from_ascii(format!("{selected_id}.internal.")).unwrap(),
            RecordType::A,
            "10.210.1.0/24".parse().unwrap(),
            HOST,
        ),
        ResponsePlan::Internal {
            code: ResponseCode::NXDomain,
            answers,
        } if answers.is_empty()
    ));
}

#[test]
fn unknown_service_id_does_not_resolve_as_a_service_name() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let missing_id = ServiceId::parse("b".repeat(32)).unwrap();
    let other_id = ServiceId::parse("c".repeat(32)).unwrap();
    let colliding_name = ServiceName::parse(missing_id.to_string()).unwrap();
    let projection = Projection::from_observations(&[observation(
        1,
        &machine,
        &other_id,
        &colliding_name,
        ContainerKind::ServiceContainer,
        running(HealthObservation::Healthy),
        Some([10, 210, 1, 3]),
    )]);

    assert!(matches!(
        projection.plan(
            &Name::from_ascii(format!("{missing_id}.internal.")).unwrap(),
            RecordType::A,
            "10.210.1.0/24".parse().unwrap(),
            HOST,
        ),
        ResponsePlan::Internal {
            code: ResponseCode::NXDomain,
            answers,
        } if answers.is_empty()
    ));
}

#[test]
fn plans_internal_protocol_responses_and_external_forwarding() {
    let projection = Projection::from_observations(&[]);
    let subnet = "10.210.1.0/24".parse().unwrap();

    assert_eq!(
        projection.plan(
            &Name::from_ascii("internal.").unwrap(),
            RecordType::A,
            subnet,
            HOST,
        ),
        ResponsePlan::Internal {
            code: ResponseCode::NXDomain,
            answers: Vec::new(),
        }
    );
    assert_eq!(
        projection.plan(
            &Name::from_ascii("missing.internal.").unwrap(),
            RecordType::A,
            subnet,
            HOST,
        ),
        ResponsePlan::Internal {
            code: ResponseCode::NXDomain,
            answers: Vec::new(),
        }
    );
    for record_type in [RecordType::AAAA, RecordType::SRV, RecordType::TXT] {
        assert_eq!(
            projection.plan(
                &Name::from_ascii("missing.internal.").unwrap(),
                record_type,
                subnet,
                HOST,
            ),
            ResponsePlan::Internal {
                code: ResponseCode::NoError,
                answers: Vec::new(),
            }
        );
    }
    assert_eq!(
        projection.plan(
            &Name::from_ascii("example.com.").unwrap(),
            RecordType::A,
            subnet,
            HOST,
        ),
        ResponsePlan::Forward
    );
}

#[test]
fn short_name_resolves_within_the_uniquely_identified_callers_project() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let staging_web = in_project(
        observation(
            1,
            &machine,
            &ServiceId::parse("b".repeat(32)).unwrap(),
            &ServiceName::parse("web").unwrap(),
            ContainerKind::ServiceContainer,
            running(HealthObservation::Unhealthy),
            Some([10, 210, 1, 2]),
        ),
        "shop-staging",
    );
    let staging_database = in_project(
        observation(
            2,
            &machine,
            &ServiceId::parse("c".repeat(32)).unwrap(),
            &ServiceName::parse("database").unwrap(),
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 10]),
        ),
        "shop-staging",
    );
    let prod_web = in_project(
        observation(
            3,
            &machine,
            &ServiceId::parse("d".repeat(32)).unwrap(),
            &ServiceName::parse("web").unwrap(),
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 3]),
        ),
        "shop-prod",
    );
    let prod_database = in_project(
        observation(
            4,
            &machine,
            &ServiceId::parse("e".repeat(32)).unwrap(),
            &ServiceName::parse("database").unwrap(),
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 11]),
        ),
        "shop-prod",
    );
    let staging_cache = in_project(
        observation(
            5,
            &machine,
            &ServiceId::parse("f".repeat(32)).unwrap(),
            &ServiceName::parse("cache").unwrap(),
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 12]),
        ),
        "shop-staging",
    );
    let projection = Projection::from_observations(&[
        staging_web,
        staging_database,
        prod_web,
        prod_database,
        staging_cache,
    ]);
    let subnet = "10.210.1.0/24".parse().unwrap();
    let staging_caller = IpAddr::V4(Ipv4Addr::new(10, 210, 1, 2));
    let prod_caller = IpAddr::V4(Ipv4Addr::new(10, 210, 1, 3));

    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii("database.internal.").unwrap(),
            RecordType::A,
            subnet,
            staging_caller,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 10)]
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii("database.internal.").unwrap(),
            RecordType::A,
            subnet,
            prod_caller,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 11)]
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii("database.shop-prod.internal.").unwrap(),
            RecordType::A,
            subnet,
            staging_caller,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 11)]
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii("database.shop-staging.internal.").unwrap(),
            RecordType::A,
            subnet,
            HOST,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 10)]
    );
    assert!(
        addresses(projection.plan(
            &Name::from_ascii("database.internal.").unwrap(),
            RecordType::A,
            subnet,
            HOST,
        ))
        .is_empty()
    );
    assert!(
        addresses(projection.plan(
            &Name::from_ascii("cache.internal.").unwrap(),
            RecordType::A,
            subnet,
            HOST,
        ))
        .is_empty()
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii("cache.internal.").unwrap(),
            RecordType::A,
            subnet,
            staging_caller,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 12)]
    );
}

#[test]
fn hook_or_ambiguous_caller_gets_no_project_relative_resolution() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let web = ServiceName::parse("web").unwrap();
    let database = ServiceName::parse("database").unwrap();
    let staging_web = in_project(
        observation(
            1,
            &machine,
            &ServiceId::parse("b".repeat(32)).unwrap(),
            &web,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 2]),
        ),
        "shop-staging",
    );
    let staging_database = in_project(
        observation(
            2,
            &machine,
            &ServiceId::parse("c".repeat(32)).unwrap(),
            &database,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 10]),
        ),
        "shop-staging",
    );
    let hook = in_project(
        observation(
            3,
            &machine,
            &ServiceId::parse("b".repeat(32)).unwrap(),
            &web,
            ContainerKind::PreDeployHook,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 4]),
        ),
        "shop-staging",
    );
    let colliding = in_project(
        observation(
            4,
            &machine,
            &ServiceId::parse("d".repeat(32)).unwrap(),
            &web,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 2]),
        ),
        "shop-prod",
    );
    let projection =
        Projection::from_observations(&[staging_web, staging_database, hook, colliding]);
    let subnet = "10.210.1.0/24".parse().unwrap();

    assert!(
        addresses(projection.plan(
            &Name::from_ascii("database.internal.").unwrap(),
            RecordType::A,
            subnet,
            IpAddr::V4(Ipv4Addr::new(10, 210, 1, 2)),
        ))
        .is_empty()
    );
    assert!(
        addresses(projection.plan(
            &Name::from_ascii("database.internal.").unwrap(),
            RecordType::A,
            subnet,
            IpAddr::V4(Ipv4Addr::new(10, 210, 1, 4)),
        ))
        .is_empty()
    );
    assert_eq!(
        addresses(projection.plan(
            &Name::from_ascii("database.shop-staging.internal.").unwrap(),
            RecordType::A,
            subnet,
            IpAddr::V4(Ipv4Addr::new(10, 210, 1, 4)),
        )),
        vec![Ipv4Addr::new(10, 210, 1, 10)]
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
	nameserver 198.51.100.53 # trailing comment
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

fn address_set(plan: ResponsePlan) -> std::collections::BTreeSet<Ipv4Addr> {
    addresses(plan).into_iter().collect()
}
