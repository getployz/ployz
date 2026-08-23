//! Projection tests for Internal DNS answers.

use std::collections::BTreeMap;

use ployz_core::{
    ContainerAddress, ContainerId, ContainerKind, ContainerRuntimeObservation, HealthObservation,
    Machine, MachineId, MachineName, MachineRuntime, ManagementAddress, ProjectName,
    ResolvedServiceSpec, ServiceId, ServiceName, WireGuardPublicKey,
};
use serde_json::json;
use tokio::net::UnixListener;

use super::*;
use crate::corrosion::fake_cluster;

const SUBNET: &str = "10.210.1.0/24";

#[tokio::test]
async fn run_reports_subscription_failure() {
    let machine = Machine {
        id: MachineId::random(),
        name: MachineName::parse("node-a").unwrap(),
        subnet: SUBNET.parse().unwrap(),
        management_address: ManagementAddress("fdcc::1".parse().unwrap()),
        public_key: WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: MachineRuntime::default(),
    };
    let (replicated, replicated_server) = fake_cluster::store().await;

    let error = run(
        machine,
        replicated,
        AdminClient::new("/no/such/admin.sock"),
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("HTTP 404 Not Found"));
    replicated_server.abort();
}

#[tokio::test(start_paused = true)]
async fn membership_sample_times_out() {
    let root = std::env::temp_dir().join(format!("ployzd-dns-membership-{}", MachineId::random()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("admin.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let admin_server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let (replicated, replicated_server) = fake_cluster::store().await;

    let error = load_down_machines(&replicated, &AdminClient::new(path), &MachineId::random())
        .await
        .unwrap_err();

    assert!(matches!(error, CorrosionError::Io(error) if error.kind() == io::ErrorKind::TimedOut));
    admin_server.abort();
    replicated_server.abort();
    std::fs::remove_dir_all(root).unwrap();
}

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
            &unfiltered_projection(&observations),
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
    let projection = unfiltered_projection(&[
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
    let projection = unfiltered_projection(&observations);

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
    let projection = unfiltered_projection(&[
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
    let projection = unfiltered_projection(&[]);

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

#[test]
fn membership_filter_excludes_down_keeps_suspect_local_and_fails_open() {
    let local = MachineId::parse("a".repeat(32)).unwrap();
    let suspect = MachineId::parse("b".repeat(32)).unwrap();
    let down = MachineId::parse("c".repeat(32)).unwrap();
    let service = ServiceId::parse("d".repeat(32)).unwrap();
    let name = ServiceName::parse("api").unwrap();
    let observations = [
        observation(
            1,
            &local,
            &service,
            &name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 1, 2]),
        ),
        observation(
            2,
            &suspect,
            &service,
            &name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 2, 2]),
        ),
        observation(
            3,
            &down,
            &service,
            &name,
            ContainerKind::ServiceContainer,
            running(HealthObservation::Healthy),
            Some([10, 210, 3, 2]),
        ),
    ];
    let down_machines = HashSet::from([local, down]);
    let mut inputs = ProjectionInputs {
        local_id: local,
        observations: observations.to_vec(),
        down_machines: None,
    };

    assert!(!inputs.update_membership(Err(CorrosionError::Protocol("not loaded".into()))));
    assert_eq!(inputs.down_machines, None);
    assert!(inputs.update_membership(Ok(down_machines.clone())));
    assert!(!inputs.update_membership(Err(CorrosionError::Protocol("unavailable".into()))));
    assert_eq!(inputs.down_machines.as_ref(), Some(&down_machines));

    assert_eq!(
        addresses(plan(
            &Projection::from_observations(&observations, &local, Some(&down_machines)),
            "api.app.internal.",
            RecordType::A,
        )),
        vec![Ipv4Addr::new(10, 210, 1, 2), Ipv4Addr::new(10, 210, 2, 2)]
    );
    assert_eq!(
        addresses(plan(
            &Projection::from_observations(&observations, &local, None),
            "api.app.internal.",
            RecordType::A,
        )),
        vec![
            Ipv4Addr::new(10, 210, 1, 2),
            Ipv4Addr::new(10, 210, 2, 2),
            Ipv4Addr::new(10, 210, 3, 2),
        ]
    );
}

fn unfiltered_projection(observations: &[ContainerObservation]) -> Projection {
    Projection::from_observations(
        observations,
        &MachineId::parse("0".repeat(32)).unwrap(),
        None,
    )
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
