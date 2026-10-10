use std::{collections::BTreeMap, net::Ipv4Addr, process, time::Duration};

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
    HealthObservation, Machine, MachineId, MachineTarget, MembershipObservation, Namespace,
    ResolvedServiceSpec, ServiceId, ServiceSelector, StartContainerRequest, StopContainerRequest,
    op, select_service,
};
use ployz_testkit::{Cluster, ClusterPlan};

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn internal_dns_tracks_healthy_replicated_containers() {
    let plan = ClusterPlan::new(&format!("l3-dns-{}", process::id()), 3).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_three().await.unwrap();
    let [first_machine, second_machine, third_machine] = &machines;
    cluster
        .machine_shell(
            0,
            "dnsmasq --conf-file=/dev/null --listen-address=127.0.0.53 --bind-interfaces --address=/#/203.0.113.7 --pid-file=/run/ployz-test-dnsmasq.pid; printf 'nameserver 127.0.0.53\n' >/etc/resolv.conf",
        )
        .unwrap();

    let direct = cluster.api_address(0).unwrap();
    let mut client = ployz::connect::connect(
        std::path::Path::new("/missing-ployz-test-config"),
        Some(&direct),
        None,
    )
    .await
    .unwrap();
    let service_id = ServiceId::random();
    let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": service_id,
        "name": "dns-api",
        "mode": { "mode": "replicated", "replicas": 3 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sh", "-c", "touch /tmp/healthy; sleep 300"],
            "pull_policy": "missing",
            "healthcheck": {
                "state": "configured",
                "test": ["CMD-SHELL", "test -f /tmp/healthy"],
                "interval_millis": 100,
                "timeout_millis": 100,
                "retries": 1
            }
        }
    }))
    .unwrap();
    let mut created = Vec::new();
    for machine in &machines {
        let container = client
            .create_container(
                machine.id,
                ContainerKind::ServiceContainer,
                Namespace::parse("app").unwrap(),
                spec.clone(),
                None,
            )
            .await
            .unwrap();
        start_container(&mut client, machine.id, container.container_id).await;
        created.push(container.container_id);
    }
    let [_, second_container, third_container] = created.as_slice() else {
        panic!("expected one container per Machine")
    };
    assert_managed_container_dns(&cluster, &created, &machines);
    let observations = wait_for_dns_observations(&mut client, &service_id, 3).await;
    let by_machine = observations
        .iter()
        .map(|observation| (observation.machine_id, observation.address.unwrap().0))
        .collect::<BTreeMap<_, _>>();
    let probe_observation = observations
        .iter()
        .find(|observation| observation.machine_id == first_machine.id)
        .unwrap();
    let gateway = first_machine.subnet.gateway().0;
    let probe = DnsProbe {
        cluster: &cluster,
        container_id: &probe_observation.container_id,
        gateway,
    };
    let expected = by_machine.values().copied().collect::<Vec<_>>();
    assert_internal_selectors(&probe, &service_id, &machines, &by_machine, &expected).await;
    assert_answer_modes(
        &probe,
        *by_machine.get(&first_machine.id).unwrap(),
        &expected,
    );
    assert_projection_changes(
        &probe,
        &mut client,
        second_machine,
        second_container,
        *by_machine.get(&second_machine.id).unwrap(),
        &expected,
    )
    .await;
    assert_protocol_contract_and_forwarding(&probe).await;
    assert_membership_filtered_projection(
        &probe,
        third_machine,
        third_container,
        *by_machine.get(&third_machine.id).unwrap(),
        &expected,
    )
    .await;
}

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn internal_dns_survives_daemon_restart() {
    let plan = ClusterPlan::new(&format!("l3-dns-restart-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let [first_machine, second_machine] = cluster.initialize_two().await.unwrap();
    let direct = cluster.api_address(0).unwrap();
    let mut client = ployz::connect::connect(
        std::path::Path::new("/missing-ployz-test-config"),
        Some(&direct),
        None,
    )
    .await
    .unwrap();
    let service_id = ServiceId::random();
    let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": service_id,
        "name": "dns-api",
        "mode": { "mode": "replicated", "replicas": 2 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sh", "-c", "touch /tmp/healthy; sleep 300"],
            "pull_policy": "missing",
            "healthcheck": {
                "state": "configured",
                "test": ["CMD-SHELL", "test -f /tmp/healthy"],
                "interval_millis": 100,
                "timeout_millis": 100,
                "retries": 1
            }
        }
    }))
    .unwrap();
    let mut created = Vec::new();
    for machine in [&first_machine, &second_machine] {
        let container = client
            .create_container(
                machine.id,
                ContainerKind::ServiceContainer,
                Namespace::parse("app").unwrap(),
                spec.clone(),
                None,
            )
            .await
            .unwrap();
        start_container(&mut client, machine.id, container.container_id).await;
        created.push(container.container_id);
    }
    let [first_container, second_container] = created.as_slice() else {
        panic!("expected one container per Machine")
    };
    let observations = wait_for_dns_observations(&mut client, &service_id, 2).await;
    let by_machine = observations
        .iter()
        .map(|observation| (observation.machine_id, observation.address.unwrap().0))
        .collect::<BTreeMap<_, _>>();
    let expected = by_machine.values().copied().collect::<Vec<_>>();
    let gateway = first_machine.subnet.gateway().0;
    let probe = DnsProbe {
        cluster: &cluster,
        container_id: first_container,
        gateway,
    };
    probe
        .wait_addresses("dns-api.app.internal", &expected)
        .await;

    let serving = ServingProcesses::observe(&cluster, 0);
    let soak = DnsSoak::start(&cluster, 0, gateway, first_container);
    for _ in 0..3 {
        cluster
            .machine_shell(
                0,
                "old=$(cat /run/ployzd.pid); kill \"$old\"; while [ \"$(cat /run/ployzd.pid)\" = \"$old\" ]; do sleep 0.1; done",
            )
            .unwrap();
        cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
        assert_eq!(ServingProcesses::observe(&cluster, 0), serving);
    }
    let report = soak.stop();
    assert!(
        report.udp.sent > 30 && report.workload.sent > 30 && report.tcp_sent > 30,
        "the soak must span the restarts: {report:?}"
    );
    assert_eq!(report.udp.failed, Vec::<String>::new(), "{report:?}");
    assert_eq!(report.tcpdial.failed, Vec::<String>::new(), "{report:?}");
    assert_eq!(report.workload.failed, Vec::<String>::new(), "{report:?}");
    assert_eq!(
        report.tcp_answered,
        (1..=report.tcp_sent).collect::<Vec<_>>(),
        "every query on the persistent TCP connection is answered once: {report:?}"
    );

    stop_container(&mut client, second_machine.id, *second_container).await;
    probe
        .wait_addresses(
            "dns-api.app.internal",
            &[*by_machine.get(&first_machine.id).unwrap()],
        )
        .await;
}

#[derive(Debug, PartialEq, Eq)]
struct ServingProcesses {
    dns_pid: String,
    corrosion_started_at: String,
}

impl ServingProcesses {
    fn observe(cluster: &Cluster, index: usize) -> Self {
        let dns_pid = cluster
            .machine_shell(index, "cat /run/ployz-dns.pid")
            .unwrap();
        let corrosion_started_at = cluster
            .machine_shell(
                index,
                "docker inspect --format '{{.State.StartedAt}}' ployz-corrosion",
            )
            .unwrap();
        assert!(!dns_pid.trim().is_empty() && !corrosion_started_at.trim().is_empty());
        Self {
            dns_pid,
            corrosion_started_at,
        }
    }
}

struct DnsSoak<'a> {
    cluster: &'a Cluster,
    index: usize,
}

#[derive(Debug)]
struct SoakReport {
    udp: SoakCounts,
    tcpdial: SoakCounts,
    workload: SoakCounts,
    tcp_sent: u16,
    tcp_answered: Vec<u16>,
}

#[derive(Debug)]
struct SoakCounts {
    sent: u32,
    failed: Vec<String>,
}

const DNS_SOAK_SCRIPT: &str = include_str!("../../../scripts/verify/dns-soak.sh");

impl<'a> DnsSoak<'a> {
    fn start(
        cluster: &'a Cluster,
        index: usize,
        gateway: Ipv4Addr,
        workload: &ContainerId,
    ) -> Self {
        let workload_pid = cluster
            .machine_shell(
                index,
                &format!("docker inspect --format '{{{{.State.Pid}}}}' {workload}"),
            )
            .unwrap();
        assert!(
            !DNS_SOAK_SCRIPT.contains('\''),
            "the soak script travels inside single quotes"
        );
        cluster
            .machine_shell(
                index,
                &format!(
                    "nohup sh -c '{DNS_SOAK_SCRIPT}' dns-soak {gateway} {} >/tmp/dns-soak-log 2>&1 &",
                    workload_pid.trim()
                ),
            )
            .unwrap();
        Self { cluster, index }
    }

    fn stop(self) -> SoakReport {
        self.cluster
            .machine_shell(
                self.index,
                "touch /tmp/dns-soak.stop; while [ ! -e /tmp/dns-soak.tcp-done ] || [ ! -e /tmp/dns-soak.udp-done ] || [ ! -e /tmp/dns-soak.tcpdial-done ] || [ ! -e /tmp/dns-soak.workload-done ]; do sleep 0.1; done",
            )
            .unwrap();
        let tcp_sent = self.read("tcp-sent").trim().parse().unwrap();
        let stream = self
            .cluster
            .machine_shell(
                self.index,
                "od -An -tx1 -v /tmp/dns-soak.tcp-out | tr -d ' \\n'",
            )
            .unwrap();
        SoakReport {
            udp: self.counts("udp"),
            tcpdial: self.counts("tcpdial"),
            workload: self.counts("workload"),
            tcp_sent,
            tcp_answered: answered_ids(&hex_bytes(stream.trim())),
        }
    }

    fn counts(&self, name: &str) -> SoakCounts {
        SoakCounts {
            sent: self.read(&format!("{name}-sent")).trim().parse().unwrap(),
            failed: self
                .read(&format!("{name}-failed"))
                .lines()
                .map(str::to_owned)
                .collect(),
        }
    }

    fn read(&self, name: &str) -> String {
        self.cluster
            .machine_shell(
                self.index,
                &format!("cat /tmp/dns-soak.{name} 2>/dev/null || true"),
            )
            .unwrap()
    }
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2).unwrap(), 16).unwrap())
        .collect()
}

fn answered_ids(stream: &[u8]) -> Vec<u16> {
    let mut ids = Vec::new();
    let mut rest = stream;
    while let Some((length, after_length)) = rest.split_first_chunk::<2>() {
        let length = usize::from(u16::from_be_bytes(*length));
        let (message, after_message) = after_length.split_at_checked(length).unwrap();
        let header: &[u8; 12] = message.first_chunk().unwrap();
        let id = u16::from_be_bytes([header[0], header[1]]);
        let rcode = header[3] & 0x0f;
        let answers = u16::from_be_bytes([header[6], header[7]]);
        assert!(
            rcode == 0 && answers > 0,
            "tcp query {id} answered with rcode {rcode} and {answers} records"
        );
        ids.push(id);
        rest = after_message;
    }
    ids.sort_unstable();
    ids
}

#[test]
fn answered_ids_reads_a_concurrent_tcp_stream() {
    let answer = |id: u16| {
        let mut message = id.to_be_bytes().to_vec();
        message.extend([0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
        message.extend([1, b'a', 0, 0, 1, 0, 1]);
        message.extend([0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 0, 0, 4, 10, 0, 0, 1]);
        let length = u16::try_from(message.len()).unwrap().to_be_bytes();
        [length.to_vec(), message].concat()
    };
    let stream = [answer(2), answer(1), answer(3)].concat();
    assert_eq!(answered_ids(&stream), vec![1, 2, 3]);
}

fn assert_managed_container_dns(
    cluster: &Cluster,
    container_ids: &[ContainerId],
    machines: &[Machine],
) {
    for (index, (container_id, machine)) in container_ids.iter().zip(machines).enumerate() {
        let docker = docker_inspect(cluster, index, container_id);
        let gateway = machine.subnet.gateway().0.to_string();
        assert_eq!(
            docker.pointer("/HostConfig/Dns"),
            Some(&serde_json::json!([gateway]))
        );
        assert_eq!(
            docker.pointer("/HostConfig/DnsSearch"),
            Some(&serde_json::json!(["app.internal"]))
        );
        assert_eq!(
            docker.pointer("/HostConfig/DnsOptions"),
            Some(&serde_json::json!(["ndots:1"]))
        );
    }
}

async fn assert_internal_selectors(
    probe: &DnsProbe<'_>,
    service_id: &ServiceId,
    machines: &[Machine],
    by_machine: &BTreeMap<MachineId, Ipv4Addr>,
    expected: &[Ipv4Addr],
) {
    // L3-071: every selector resolves all healthy Service Containers with authoritative TTL-zero answers.
    probe.wait_addresses("dns-api.app.internal", expected).await;
    let searched = probe
        .cluster
        .machine_shell(
            0,
            &format!("docker exec {} nslookup dns-api", probe.container_id),
        )
        .unwrap();
    for address in expected {
        assert!(searched.contains(&address.to_string()));
    }
    let namespace_relative = probe
        .cluster
        .machine_shell(
            0,
            &format!(
                "docker exec {} nslookup dns-api.internal",
                probe.container_id
            ),
        )
        .unwrap();
    for address in expected {
        assert!(
            namespace_relative.contains(&address.to_string()),
            "dns-api.internal from a Caller Namespace should answer {address}: {namespace_relative}"
        );
    }
    probe.assert_addresses(&format!("{service_id}.id.lookup.internal"), expected);
    for machine in machines {
        probe.assert_addresses(
            &format!("dns-api.app.{}.machine.internal", machine.id),
            &[*by_machine.get(&machine.id).unwrap()],
        );
    }
    let authoritative = probe.dig("dns-api.app.internal", "A", false);
    assert!(authoritative.contains("status: NOERROR"));
    assert!(authoritative.contains("flags: qr aa"));
    assert!(
        authoritative
            .lines()
            .filter(|line| line.contains("\tIN\tA\t"))
            .all(|line| line.split_whitespace().nth(1).is_some_and(|ttl| ttl == "0"))
    );
}

fn assert_answer_modes(probe: &DnsProbe<'_>, local_address: Ipv4Addr, expected: &[Ipv4Addr]) {
    // L3-072: nearest is local-first and keeps the complete answer set.
    let nearest = dig_addresses(&probe.dig("dns-api.app.nearest.internal", "A", false));
    assert_eq!(nearest.first(), Some(&local_address));
    assert_eq!(sorted(nearest), sorted(expected.to_vec()));
}

async fn assert_projection_changes(
    probe: &DnsProbe<'_>,
    client: &mut ployz::connect::Client,
    second_machine: &Machine,
    second_container: &ContainerId,
    second_address: Ipv4Addr,
    expected: &[Ipv4Addr],
) {
    // L3-074: health and runtime changes add and remove records through the live subscription.
    probe
        .cluster
        .machine_shell(
            1,
            &format!("docker exec {second_container} rm /tmp/healthy"),
        )
        .unwrap();
    wait_for_health(client, second_container, HealthObservation::Unhealthy).await;
    let without_second = expected
        .iter()
        .copied()
        .filter(|address| *address != second_address)
        .collect::<Vec<_>>();
    probe
        .wait_addresses("dns-api.app.internal", &without_second)
        .await;
    probe
        .cluster
        .machine_shell(
            1,
            &format!("docker exec {second_container} touch /tmp/healthy"),
        )
        .unwrap();
    wait_for_health(client, second_container, HealthObservation::Healthy).await;
    probe.wait_addresses("dns-api.app.internal", expected).await;
    stop_container(client, second_machine.id, *second_container).await;
    probe
        .wait_addresses("dns-api.app.internal", &without_second)
        .await;
    start_container(client, second_machine.id, *second_container).await;
    probe.wait_addresses("dns-api.app.internal", expected).await;
}

async fn assert_protocol_contract_and_forwarding(probe: &DnsProbe<'_>) {
    let missing = probe.dig("missing.internal", "A", false);
    assert!(missing.contains("status: NXDOMAIN"));
    assert!(missing.contains("flags: qr aa"));
    let non_a = probe.dig("missing.internal", "TXT", false);
    assert!(non_a.contains("status: NOERROR"));
    assert!(non_a.contains("flags: qr aa"));

    probe
        .wait_addresses("forward.test", &[Ipv4Addr::new(203, 0, 113, 7)])
        .await;
    assert_eq!(
        dig_addresses(&probe.dig("forward.test", "A", true)),
        vec![Ipv4Addr::new(203, 0, 113, 7)]
    );
    probe
        .cluster
        .machine_shell(0, "kill $(cat /run/ployz-test-dnsmasq.pid)")
        .unwrap();
    assert!(
        probe
            .dig("forward.test", "A", false)
            .contains("status: SERVFAIL")
    );
}

async fn assert_membership_filtered_projection(
    probe: &DnsProbe<'_>,
    third_machine: &Machine,
    third_container: &ContainerId,
    third_address: Ipv4Addr,
    expected: &[Ipv4Addr],
) {
    // A Down Membership Observation filters the peer while retaining its replicated observation.
    probe.cluster.stop(2).unwrap();
    wait_for_down_membership_observation(probe.cluster, third_machine).await;
    let encoded = probe
        .cluster
        .machine_shell(
            0,
            &format!(
                "sqlite3 /var/lib/ployz/corrosion/store.db \"SELECT container FROM containers WHERE id = '{third_container}'\""
            ),
        )
        .unwrap();
    let retained: ContainerObservation = serde_json::from_str(encoded.trim()).unwrap();
    assert_eq!(retained.machine_id, third_machine.id);
    assert_eq!(
        retained.address.map(|address| address.0),
        Some(third_address)
    );
    assert!(matches!(
        retained.runtime,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy
        }
    ));
    let without_third = expected
        .iter()
        .copied()
        .filter(|address| *address != third_address)
        .collect::<Vec<_>>();
    probe
        .wait_addresses("dns-api.app.internal", &without_third)
        .await;
}

struct DnsProbe<'a> {
    cluster: &'a Cluster,
    container_id: &'a ContainerId,
    gateway: Ipv4Addr,
}

impl DnsProbe<'_> {
    async fn wait_addresses(&self, name: &str, expected: &[Ipv4Addr]) {
        let expected = sorted(expected.to_vec());
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if sorted(dig_addresses(&self.dig(name, "A", false))) == expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
    }

    fn assert_addresses(&self, name: &str, expected: &[Ipv4Addr]) {
        assert_eq!(
            sorted(dig_addresses(&self.dig(name, "A", false))),
            sorted(expected.to_vec())
        );
    }

    fn dig(&self, name: &str, record_type: &str, tcp: bool) -> String {
        self.cluster
            .container_network_shell(
                0,
                self.container_id,
                &format!(
                    "dig {}@{} {name} {record_type} +time=1 +tries=1 +noall +comments +answer",
                    if tcp { "+tcp " } else { "" },
                    self.gateway,
                ),
            )
            .unwrap()
    }
}

async fn wait_for_dns_observations(
    client: &mut ployz::connect::Client,
    service_id: &ServiceId,
    count: usize,
) -> Vec<ContainerObservation> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(live) = client
                .live_services(ployz_core::EnvironmentValues::Redacted)
                .await
            {
                let services = live.services();
                if let Ok(service) = select_service(&services, &ServiceSelector::from(service_id))
                    && service.containers.len() == count
                    && service.containers.iter().all(|container| {
                        let observation = container.as_observation();
                        observation.address.is_some()
                            && matches!(
                                &observation.runtime,
                                ContainerRuntimeObservation::Running {
                                    health: HealthObservation::Healthy
                                }
                            )
                    })
                {
                    return service
                        .containers
                        .iter()
                        .map(|container| container.as_observation().clone())
                        .collect();
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap()
}

async fn wait_for_health(
    client: &mut ployz::connect::Client,
    container_id: &ContainerId,
    expected: HealthObservation,
) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(live) = client
                .live_services(ployz_core::EnvironmentValues::Redacted)
                .await
                && live
                    .containers
                    .successes
                    .iter()
                    .flat_map(|success| &success.value)
                    .find(|container| container.container_id == *container_id)
                    .is_some_and(|container| {
                        matches!(
                            &container.runtime,
                            ContainerRuntimeObservation::Running { health }
                                if health == &expected
                        )
                    })
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_down_membership_observation(cluster: &Cluster, machine: &Machine) {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if cluster.machines(0).await.is_ok_and(|observations| {
                observations
                    .iter()
                    .find(|observation| observation.machine.id == machine.id)
                    .is_some_and(|observation| {
                        observation.membership == MembershipObservation::Down
                    })
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
}

fn dig_addresses(output: &str) -> Vec<Ipv4Addr> {
    output
        .lines()
        .filter_map(|line| line.split_whitespace().last()?.parse().ok())
        .collect()
}

async fn start_container(
    client: &mut ployz::connect::Client,
    machine_id: MachineId,
    container_id: ContainerId,
) {
    client
        .call::<op::StartContainer>(
            StartContainerRequest { container_id },
            Some(&MachineTarget::from(&machine_id)),
        )
        .await
        .unwrap();
}

async fn stop_container(
    client: &mut ployz::connect::Client,
    machine_id: MachineId,
    container_id: ContainerId,
) {
    client
        .call::<op::StopContainer>(
            StopContainerRequest {
                container_id,
                signal: None,
                grace_period_seconds: None,
            },
            Some(&MachineTarget::from(&machine_id)),
        )
        .await
        .unwrap();
}

fn sorted(mut addresses: Vec<Ipv4Addr>) -> Vec<Ipv4Addr> {
    addresses.sort_unstable();
    addresses
}

fn docker_inspect(
    cluster: &Cluster,
    machine_index: usize,
    container_id: &ContainerId,
) -> serde_json::Value {
    let output = cluster
        .machine_shell(machine_index, &format!("docker inspect {container_id}"))
        .unwrap();
    serde_json::from_str::<Vec<serde_json::Value>>(&output)
        .unwrap()
        .pop()
        .unwrap()
}
