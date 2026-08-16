use std::{collections::BTreeMap, net::Ipv4Addr, process, time::Duration};

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
    HealthObservation, Machine, MachineId, MachineSelector, MembershipObservation,
    ResolvedServiceSpec, ServiceId, StartContainerRequest, StopContainerRequest, op,
    select_service,
};
use ployz_testkit::{Cluster, ClusterPlan};

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn internal_dns_tracks_healthy_replicated_containers() {
    let plan = ClusterPlan::new(&format!("l3-dns-{}", process::id()), 3).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_three().await.unwrap();
    let [first_machine, second_machine, third_machine] = &machines;
    cluster
        .machine_shell(
            0,
            "dnsmasq --conf-file=/dev/null --listen-address=127.0.0.53 --bind-interfaces --address=/#/203.0.113.7 --pid-file=/run/ployz-test-dnsmasq.pid; printf 'nameserver 127.0.0.53\n' >/etc/resolv.conf; old=$(cat /run/ployzd.pid); kill \"$old\"; while [ \"$(cat /run/ployzd.pid)\" = \"$old\" ]; do sleep 0.1; done",
        )
        .unwrap();
    cluster.wait_ready(Duration::from_secs(60)).await.unwrap();

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
            .create_container(machine.id, ContainerKind::ServiceContainer, spec.clone())
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
    let gateway = Ipv4Addr::from(u32::from(first_machine.subnet.0.network()) + 1);
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
    assert_protocol_contract_and_forwarding(&probe);
    assert_membership_blind_projection(
        &probe,
        third_machine,
        third_container,
        *by_machine.get(&third_machine.id).unwrap(),
        &expected,
    )
    .await;
}

fn assert_managed_container_dns(
    cluster: &Cluster,
    container_ids: &[ContainerId],
    machines: &[Machine],
) {
    for (index, (container_id, machine)) in container_ids.iter().zip(machines).enumerate() {
        let docker = docker_inspect(cluster, index, container_id);
        let gateway = Ipv4Addr::from(u32::from(machine.subnet.0.network()) + 1).to_string();
        assert_eq!(
            docker.pointer("/HostConfig/Dns"),
            Some(&serde_json::json!([gateway]))
        );
        assert_eq!(
            docker.pointer("/HostConfig/DnsSearch"),
            Some(&serde_json::json!(["internal"]))
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
    probe.wait_addresses("dns-api.internal", expected).await;
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
    probe.assert_addresses(&format!("{service_id}.internal"), expected);
    for machine in machines {
        probe.assert_addresses(
            &format!("{}.m.dns-api.internal", machine.id),
            &[*by_machine.get(&machine.id).unwrap()],
        );
    }
    let authoritative = probe.dig("dns-api.internal", "A", false);
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
    // L3-072: nearest is local-first and rr keeps the complete answer set.
    let nearest = dig_addresses(&probe.dig("nearest.dns-api.internal", "A", false));
    assert_eq!(nearest.first(), Some(&local_address));
    assert_eq!(sorted(nearest), sorted(expected.to_vec()));
    probe.assert_addresses("rr.dns-api.internal", expected);
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
        .wait_addresses("dns-api.internal", &without_second)
        .await;
    probe
        .cluster
        .machine_shell(
            1,
            &format!("docker exec {second_container} touch /tmp/healthy"),
        )
        .unwrap();
    wait_for_health(client, second_container, HealthObservation::Healthy).await;
    probe.wait_addresses("dns-api.internal", expected).await;
    stop_container(client, second_machine.id, *second_container).await;
    probe
        .wait_addresses("dns-api.internal", &without_second)
        .await;
    start_container(client, second_machine.id, *second_container).await;
    probe.wait_addresses("dns-api.internal", expected).await;
}

fn assert_protocol_contract_and_forwarding(probe: &DnsProbe<'_>) {
    let missing = probe.dig("missing.internal", "A", false);
    assert!(missing.contains("status: NXDOMAIN"));
    assert!(missing.contains("flags: qr aa"));
    let non_a = probe.dig("missing.internal", "TXT", false);
    assert!(non_a.contains("status: NOERROR"));
    assert!(non_a.contains("flags: qr aa"));

    for tcp in [false, true] {
        assert_eq!(
            dig_addresses(&probe.dig("forward.test", "A", tcp)),
            vec![Ipv4Addr::new(203, 0, 113, 7)]
        );
    }
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

async fn assert_membership_blind_projection(
    probe: &DnsProbe<'_>,
    third_machine: &Machine,
    third_container: &ContainerId,
    third_address: Ipv4Addr,
    expected: &[Ipv4Addr],
) {
    // L3-073: a Down Membership Observation does not erase the last replicated healthy observation.
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
    probe.wait_addresses("dns-api.internal", expected).await;
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
            if let Ok(live) = client.live_services().await
                && let Ok(service) = select_service(&live.services, service_id.as_str())
                && service.containers.len() == count
                && service.containers.iter().all(|container| {
                    container.address.is_some()
                        && matches!(
                            &container.runtime,
                            ContainerRuntimeObservation::Running {
                                health: HealthObservation::Healthy
                            }
                        )
                })
            {
                return service.containers.clone();
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
            if let Ok(live) = client.live_services().await
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
            Some(&MachineSelector::from(&machine_id)),
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
            Some(&MachineSelector::from(&machine_id)),
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
