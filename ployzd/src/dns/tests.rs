//! Projection tests for Internal DNS answers.

use std::collections::BTreeMap;

use hickory_server::proto::op::{Edns, Query as WireQuery};
use ployz_core::{
    ContainerAddress, ContainerId, ContainerKind, ContainerRuntimeObservation, HealthObservation,
    Machine, MachineId, MachineName, MachineRuntime, ProjectName, ResolvedServiceSpec, ServiceId,
    ServiceName, WireGuardPublicKey,
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

#[tokio::test]
async fn oversized_internal_udp_sets_tc_and_stays_within_client_limit() {
    let (udp, tcp, mut server) = serve_internal(50).await;

    let (no_edns_len, no_edns) = query_udp(udp, None).await;
    assert!(no_edns_len <= 512, "no-EDNS UDP was {no_edns_len} bytes");
    assert!(no_edns.metadata.truncation);
    assert!(!no_edns.answers.is_empty());

    let (edns_512_len, edns_512) = query_udp(udp, Some(512)).await;
    assert!(edns_512_len <= 512, "EDNS 512 UDP was {edns_512_len} bytes");
    assert!(edns_512.metadata.truncation);
    assert!(!edns_512.answers.is_empty());

    let (edns_4096_len, edns_4096) = query_udp(udp, Some(4096)).await;
    assert!(
        edns_4096_len <= 4096,
        "EDNS 4096 UDP was {edns_4096_len} bytes"
    );
    assert!(!edns_4096.metadata.truncation);
    assert_eq!(edns_4096.answers.len(), 50);

    let tcp_response = query_tcp(tcp, None).await;
    assert!(!tcp_response.metadata.truncation);
    assert_eq!(tcp_response.answers.len(), 50);

    server.shutdown_gracefully().await.unwrap();
}

#[tokio::test]
async fn small_internal_udp_answer_is_not_truncated() {
    let (udp, _, mut server) = serve_internal(2).await;
    let (len, response) = query_udp(udp, None).await;
    assert!(len <= 512, "small UDP was {len} bytes");
    assert!(!response.metadata.truncation);
    assert_eq!(response.answers.len(), 2);
    server.shutdown_gracefully().await.unwrap();
}

async fn serve_internal(count: u16) -> (SocketAddr, SocketAddr, Server<Handler>) {
    let handler = Handler {
        projection: Arc::new(RwLock::new(unfiltered_projection(&replica_observations(
            count,
        )))),
        local_subnet: SUBNET.parse().unwrap(),
        upstreams: Vec::new(),
    };
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_addr = udp.local_addr().unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_addr = tcp.local_addr().unwrap();
    let mut server = Server::new(handler);
    server.register_socket(udp);
    server.register_listener(tcp, TCP_REQUEST_TIMEOUT, TCP_RESPONSE_BUFFER);
    (udp_addr, tcp_addr, server)
}

async fn query_udp(addr: SocketAddr, edns_payload: Option<u16>) -> (usize, Message) {
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(&a_query(edns_payload), addr).await.unwrap();
    let mut buf = [0u8; 4096];
    let (len, _) = tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buf))
        .await
        .expect("UDP Internal DNS response")
        .unwrap();
    let packet = buf.get(..len).unwrap();
    (len, Message::from_vec(packet).unwrap())
}

async fn query_tcp(addr: SocketAddr, edns_payload: Option<u16>) -> Message {
    let query = a_query(edns_payload);
    let length = u16::try_from(query.len()).unwrap();
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_u16(length).await.unwrap();
    stream.write_all(&query).await.unwrap();
    let length = tokio::time::timeout(Duration::from_secs(2), stream.read_u16())
        .await
        .expect("TCP Internal DNS length")
        .unwrap();
    let mut response = vec![0; usize::from(length)];
    stream.read_exact(&mut response).await.unwrap();
    Message::from_vec(&response).unwrap()
}

fn a_query(edns_payload: Option<u16>) -> Vec<u8> {
    let mut message = Message::query();
    message.add_query(WireQuery::query(
        Name::from_ascii("api.app.internal.").unwrap(),
        RecordType::A,
    ));
    if let Some(max_payload) = edns_payload {
        let mut edns = Edns::new();
        edns.set_max_payload(max_payload);
        message.set_edns(edns);
    }
    message.to_vec().unwrap()
}

fn replica_observations(count: u16) -> Vec<ContainerObservation> {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let service = ServiceId::parse("b".repeat(32)).unwrap();
    let name = ServiceName::parse("api").unwrap();
    (0..count)
        .map(|i| {
            let host = u8::try_from(i + 1).unwrap();
            let mut observation = observation(
                1,
                &machine,
                &service,
                &name,
                ContainerKind::ServiceContainer,
                running(HealthObservation::Healthy),
                Some([10, 210, 1, host]),
            );
            observation
                .try_update(|parts| {
                    parts.container_id = ContainerId::parse(format!("{i:064x}")).unwrap()
                })
                .unwrap();
            observation
        })
        .collect()
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
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: ContainerId::parse(format!("{suffix:x}").repeat(64)).unwrap(),
        display_name: format!("{service_name}-{suffix}"),
        created_at_unix_nanos: 0,
        machine_id: *machine_id,
        project_name: ProjectName::parse("app").unwrap(),
        kind,
        runtime,
        effective_healthcheck: None,
        resolved_spec: fixture_spec(service_id, service_name),
        address: address.map(|octets| ContainerAddress(octets.into())),
        labels: BTreeMap::new(),
    })
    .unwrap()
}

fn in_project(mut observation: ContainerObservation, project: &str) -> ContainerObservation {
    observation
        .try_update(|parts| parts.project_name = ProjectName::parse(project).unwrap())
        .unwrap();
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
