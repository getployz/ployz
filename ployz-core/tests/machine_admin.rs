use std::{collections::BTreeMap, net::IpAddr};

use ipnet::IpNet;
use ployz_core::{
    AdvertisedEndpoint, Machine, MachineId, MachineIdentity, MachineName, MachineRuntime,
    MachineSelector, MachineSubnet, MachineUpdate, MachineUpdateError, ManagementAddress,
    MembershipObservation, NameMatches, PublicIpUpdate, RttStatistics, WireGuardDevice,
    WireGuardPeer, WireGuardPublicKey, apply_machine_update, associate_wireguard_peers,
    resolve_machine_selector, rtt_statistics, synthesize_membership,
};

#[test]
fn update_mapping_preserves_omissions_and_applies_one_atomic_patch() {
    let original = machine('1', "first", 1);
    let other = machine('2', "second", 2);
    let endpoints = vec![AdvertisedEndpoint("203.0.113.9:51820".parse().unwrap())];
    let updated = apply_machine_update(
        &original,
        &[original.clone(), other.clone()],
        MachineUpdate {
            name: Some(MachineName::parse("renamed").unwrap()),
            public_ip: PublicIpUpdate::Set("203.0.113.9".parse().unwrap()),
            advertised_endpoints: Some(endpoints.clone()),
        },
    )
    .unwrap();

    assert_eq!(updated.id, original.id);
    assert_eq!(updated.subnet, original.subnet);
    assert_eq!(updated.management_address, original.management_address);
    assert_eq!(updated.public_key, original.public_key);
    assert_eq!(updated.name.as_str(), "renamed");
    assert_eq!(updated.public_ip, Some("203.0.113.9".parse().unwrap()));
    assert_eq!(updated.advertised_endpoints, endpoints);

    assert_eq!(
        apply_machine_update(
            &updated,
            &[updated.clone(), other],
            MachineUpdate::default()
        )
        .unwrap(),
        updated
    );
}

#[test]
fn invalid_machine_updates_are_rejected_without_a_partial_value() {
    let original = machine('1', "first", 1);
    let other = machine('2', "second", 2);

    assert_eq!(
        apply_machine_update(
            &original,
            &[original.clone(), other],
            MachineUpdate {
                name: Some(MachineName::parse("second").unwrap()),
                ..Default::default()
            },
        ),
        Err(MachineUpdateError::DuplicateName)
    );
    assert_eq!(
        apply_machine_update(
            &original,
            std::slice::from_ref(&original),
            MachineUpdate {
                advertised_endpoints: Some(Vec::new()),
                ..Default::default()
            },
        ),
        Err(MachineUpdateError::MissingEndpoints)
    );
}

#[test]
fn membership_is_responder_relative_and_keeps_duplicate_names() {
    let local = machine('1', "duplicate", 1);
    let up = machine('2', "duplicate", 2);
    let suspect = machine('3', "suspect", 3);
    let down = machine('4', "down", 4);
    let states = BTreeMap::from([
        (up.management_address, MembershipObservation::Up),
        (suspect.management_address, MembershipObservation::Suspect),
    ]);

    let observations =
        synthesize_membership(vec![local.clone(), up, suspect, down], &local.id, &states);

    let [local, up, suspect, down] = observations.as_slice() else {
        panic!("expected four Machine observations")
    };
    assert_eq!(local.membership, MembershipObservation::Up);
    assert_eq!(up.membership, MembershipObservation::Up);
    assert_eq!(suspect.membership, MembershipObservation::Suspect);
    assert_eq!(down.membership, MembershipObservation::Down);
    assert_eq!(
        observations
            .iter()
            .filter(|observation| observation.machine.name.as_str() == "duplicate")
            .count(),
        2
    );
}

#[test]
fn selector_resolution_prefers_ids_and_preserves_name_ambiguity() {
    let first = machine('1', "duplicate", 1);
    let second = machine('2', "duplicate", 2);
    let id_collision = machine('3', first.id.as_str(), 3);
    let visible = [first.clone(), second.clone(), id_collision];

    assert_eq!(
        resolve_machine_selector(&MachineSelector::from(&first.id), &visible),
        NameMatches::One(&first)
    );
    assert_eq!(
        resolve_machine_selector(&MachineSelector::parse("duplicate").unwrap(), &visible),
        NameMatches::Ambiguous(vec![&first, &second])
    );
}

#[test]
fn rtt_statistics_use_median_and_population_standard_deviation() {
    assert_eq!(rtt_statistics(&[]), None);
    assert_eq!(
        rtt_statistics(&[10.0, 20.0]),
        Some(RttStatistics {
            median_ns: 15_000_000,
            population_stddev_ns: 5_000_000,
        })
    );
    let odd = rtt_statistics(&[3.0, 1.0, 2.0]).unwrap();
    assert_eq!(odd.median_ns, 2_000_000);
    assert_eq!(odd.population_stddev_ns, 816_496);
}

#[test]
fn rtt_statistics_keep_sub_millisecond_samples() {
    assert_eq!(rtt_statistics(&[]), None);
    assert_eq!(
        rtt_statistics(&[0.0]),
        Some(RttStatistics {
            median_ns: 0,
            population_stddev_ns: 0,
        })
    );
    assert_eq!(
        rtt_statistics(&[0.0, 0.0]),
        Some(RttStatistics {
            median_ns: 0,
            population_stddev_ns: 0,
        })
    );
}

#[test]
fn wireguard_projection_keeps_unknown_peers_and_optional_live_fields() {
    let known = machine('2', "known", 2);
    let known_key = known.public_key;
    let unknown_key = WireGuardPublicKey([9; 32]);
    let known_rtt = RttStatistics {
        median_ns: 10,
        population_stddev_ns: 2,
    };
    let device = WireGuardDevice {
        interface_name: "ployz-wg".into(),
        public_key: WireGuardPublicKey([1; 32]),
        listen_port: 51820,
        peers: vec![
            WireGuardPeer {
                public_key: known_key,
                endpoint: Some("203.0.113.2:51820".parse().unwrap()),
                last_handshake_unix_seconds: Some(100),
                received_bytes: 7,
                sent_bytes: 8,
                allowed_ips: vec!["fdcc::2/128".parse::<IpNet>().unwrap()],
                machine: None,
                rtt: None,
            },
            WireGuardPeer {
                public_key: unknown_key,
                endpoint: None,
                last_handshake_unix_seconds: None,
                received_bytes: 0,
                sent_bytes: 0,
                allowed_ips: Vec::new(),
                machine: None,
                rtt: None,
            },
        ],
    };

    let projected = associate_wireguard_peers(
        device.clone(),
        std::slice::from_ref(&known),
        &BTreeMap::from([(known.id, known_rtt.clone())]),
    );

    let [known_peer, unknown_peer] = projected.peers.as_slice() else {
        panic!("expected known and unknown WireGuard peers")
    };
    assert_eq!(
        known_peer.machine,
        Some(MachineIdentity {
            id: known.id,
            name: known.name.clone(),
        })
    );
    assert_eq!(known_peer.rtt, Some(known_rtt));
    assert_eq!(unknown_peer.public_key, unknown_key);
    assert_eq!(unknown_peer.machine, None);
    assert_eq!(unknown_peer.endpoint, None);

    let mut duplicate = machine('3', "duplicate-key", 3);
    duplicate.public_key = known_key;
    let ambiguous = associate_wireguard_peers(device, &[known, duplicate], &BTreeMap::new());
    let ambiguous_peer = ambiguous.peers.first().unwrap();
    assert_eq!(ambiguous_peer.machine, None);
    assert_eq!(ambiguous_peer.rtt, None);
}

fn machine(id: char, name: &str, seed: u8) -> Machine {
    Machine {
        id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: MachineSubnet(format!("10.210.{seed}.0/24").parse().unwrap()),
        management_address: ManagementAddress(format!("fdcc::{seed}").parse().unwrap()),
        public_key: WireGuardPublicKey([seed; 32]),
        public_ip: Some(IpAddr::from([192, 0, 2, seed])),
        advertised_endpoints: vec![AdvertisedEndpoint(
            format!("192.0.2.{seed}:51820").parse().unwrap(),
        )],
        runtime: MachineRuntime::default(),
    }
}
