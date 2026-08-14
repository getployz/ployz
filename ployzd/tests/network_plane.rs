use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::{Duration, SystemTime},
};

use ipnet::{IpNet, Ipv4Net};
use ployz_core::{
    AdvertisedEndpoint, Machine, MachineId, MachineName, MachineSubnet, ManagementAddress,
    PublicIpDiscovery, SelectedEndpoint, WireGuardPublicKey,
};
use ployzd::network::{
    EndpointSelection, PeerStatus, WIREGUARD_KEEPALIVE_SECONDS, allocate_machine_subnet,
    default_cluster_network, discover_network, machine_gateway, management_address, peers_for,
};

#[test]
fn address_math_and_management_address_match_the_network_contract() {
    let subnet = MachineSubnet("10.210.7.0/24".parse().unwrap());
    assert_eq!(
        machine_gateway(subnet).unwrap().0,
        Ipv4Addr::new(10, 210, 7, 1)
    );

    let key = WireGuardPublicKey([
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0xee,
        0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
    assert_eq!(
        management_address(key),
        ManagementAddress("fdcc:1:203:405:607:809:a0b:c0d".parse().unwrap())
    );
}

#[test]
fn allocation_uses_only_the_supplied_stale_snapshot() {
    let pool = default_cluster_network();
    let claimed = [MachineSubnet("10.210.0.0/24".parse().unwrap())];
    assert_eq!(
        allocate_machine_subnet(pool, claimed).unwrap(),
        MachineSubnet("10.210.1.0/24".parse().unwrap())
    );

    let stale_snapshot: [MachineSubnet; 0] = [];
    let first = allocate_machine_subnet(pool, stale_snapshot).unwrap();
    let concurrent = allocate_machine_subnet(pool, stale_snapshot).unwrap();
    assert_eq!(first, concurrent, "concurrent overlap remains permitted");
}

#[test]
fn every_machine_table_rebuild_is_a_complete_directed_mesh() {
    let machines = [machine(1), machine(2), machine(3)];
    let peer_count = machines
        .iter()
        .map(|machine| peers_for(&machine.id, &machines).len())
        .sum::<usize>();
    assert_eq!(peer_count, machines.len() * (machines.len() - 1));

    let peer = peers_for(&machines[0].id, &machines)
        .into_iter()
        .find(|peer| peer.machine_id == machines[1].id)
        .unwrap();
    assert_eq!(
        peer.allowed_ips,
        [
            IpNet::new(IpAddr::V6(machines[1].management_address.0), 128).unwrap(),
            IpNet::V4(machines[1].subnet.0),
        ]
    );
    assert_eq!(WIREGUARD_KEEPALIVE_SECONDS, 25);
}

#[test]
fn endpoint_selection_preserves_unknown_down_rotation_and_reverse_learning() {
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let endpoints = [endpoint(10), endpoint(11)];
    let mut selection = EndpointSelection::new(&endpoints, None, start);
    assert_eq!(selection.selected(), Some(SelectedEndpoint(endpoints[0].0)));
    assert_eq!(
        selection.status(start + Duration::from_secs(14), None),
        PeerStatus::Unknown
    );

    assert_eq!(
        selection.status(start + Duration::from_secs(16), None),
        PeerStatus::Down
    );
    let rotation = selection.poll(start + Duration::from_secs(16), None, None);
    assert_eq!(rotation, Some(SelectedEndpoint(endpoints[1].0)));

    let reverse = SocketAddr::from(([203, 0, 113, 7], 51820));
    let learned = selection.poll(
        start + Duration::from_secs(17),
        Some(start + Duration::from_secs(17)),
        Some(reverse),
    );
    assert_eq!(learned, Some(SelectedEndpoint(reverse)));
    assert_eq!(
        selection.status(
            start + Duration::from_secs(17),
            Some(start + Duration::from_secs(17))
        ),
        PeerStatus::Up
    );

    let stale = selection.poll(
        start + Duration::from_secs(17 + 276),
        Some(start + Duration::from_secs(17)),
        None,
    );
    assert_eq!(stale, Some(SelectedEndpoint(endpoints[0].0)));

    let only = [endpoint(12)];
    let mut reverse_only = EndpointSelection::new(&only, None, start);
    reverse_only.poll(start, Some(start), Some(reverse));
    let fallback = reverse_only.poll(start + Duration::from_secs(276), Some(start), None);
    assert_eq!(fallback, Some(SelectedEndpoint(only[0].0)));

    let persisted =
        EndpointSelection::new(&endpoints, Some(SelectedEndpoint(endpoints[0].0)), start);
    assert_eq!(
        persisted.status(
            start + Duration::from_secs(16),
            Some(start - Duration::from_secs(1))
        ),
        PeerStatus::Up,
        "a persisted choice is not treated as freshly selected"
    );
}

#[tokio::test]
async fn explicit_public_ip_is_added_without_a_reachability_probe() {
    let public = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
    let endpoints = discover_network(51820, PublicIpDiscovery::Override(public))
        .await
        .unwrap()
        .endpoints;
    assert!(endpoints.contains(&AdvertisedEndpoint(SocketAddr::new(public, 51820))));
}

fn machine(seed: u8) -> Machine {
    Machine {
        id: MachineId::parse(format!("{seed:032x}")).unwrap(),
        name: MachineName::parse(format!("machine-{seed}")).unwrap(),
        subnet: MachineSubnet(Ipv4Net::new(Ipv4Addr::new(10, 210, seed, 0), 24).unwrap()),
        management_address: ManagementAddress(Ipv6Addr::from([
            0xfd, 0xcc, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, seed,
        ])),
        public_key: WireGuardPublicKey([seed; 32]),
        public_ip: None,
        advertised_endpoints: vec![endpoint(seed)],
        runtime: Default::default(),
    }
}

fn endpoint(seed: u8) -> AdvertisedEndpoint {
    AdvertisedEndpoint(SocketAddr::from(([192, 0, 2, seed], 51820)))
}
