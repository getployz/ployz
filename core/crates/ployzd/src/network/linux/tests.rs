use super::*;

fn installed_peer(seed: u8) -> Peer {
    let mut peer = Peer::new(Key::new([seed; 32]));
    peer.allowed_ips = vec![IpAddrMask::new(IpAddr::from([10, 210, seed, 0]), 24)];
    peer.endpoint = Some(SocketAddr::from(([192, 0, 2, seed], WIREGUARD_PORT)));
    peer.persistent_keepalive_interval = Some(WIREGUARD_KEEPALIVE_SECONDS);
    peer
}

fn reconcile(
    installed: &mut HashMap<Key, Peer>,
    planned: &[Peer],
    set: PeerSet,
) -> BTreeSet<IpNet> {
    let PeerChanges {
        updates,
        removals,
        routes,
    } = peer_changes(installed, planned, set).unwrap();
    let updates = updates.into_iter().cloned().collect::<Vec<_>>();
    let removals = removals.into_iter().cloned().collect::<Vec<_>>();
    for peer in updates {
        installed.insert(peer.public_key.clone(), peer);
    }
    for key in removals {
        installed.remove(&key);
    }
    routes
}

#[test]
fn bootstrap_keeps_installed_peers_and_roamed_endpoints() {
    let mut roamed = installed_peer(1);
    roamed.endpoint = Some(SocketAddr::from(([203, 0, 113, 7], WIREGUARD_PORT)));
    roamed.last_handshake = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1000));
    roamed.rx_bytes = 1234;
    let unrelated = installed_peer(2);
    let mut installed = HashMap::from([
        (roamed.public_key.clone(), roamed.clone()),
        (unrelated.public_key.clone(), unrelated.clone()),
    ]);
    let original = installed.clone();
    let routes = reconcile(&mut installed, &[], PeerSet::Bootstrap);
    assert_eq!(
        installed, original,
        "founder bootstrap is not membership deletion"
    );
    assert_eq!(routes, peer_routes(original.values()).unwrap());

    let missing = installed_peer(3);
    let routes = reconcile(
        &mut installed,
        &[installed_peer(1), missing.clone()],
        PeerSet::Bootstrap,
    );
    assert_eq!(installed.get(&roamed.public_key), Some(&roamed));
    assert_eq!(installed.get(&unrelated.public_key), Some(&unrelated));
    assert_eq!(installed.get(&missing.public_key), Some(&missing));
    assert_eq!(routes, peer_routes(installed.values()).unwrap());
}

#[test]
fn snapshot_updates_adds_and_prunes_peers_and_routes() {
    let first = installed_peer(1);
    let departed = installed_peer(2);
    let mut installed = HashMap::from([
        (first.public_key.clone(), first.clone()),
        (departed.public_key.clone(), departed.clone()),
    ]);
    let mut changed = first;
    changed.allowed_ips = vec![IpAddrMask::new(IpAddr::from([10, 211, 1, 0]), 24)];
    changed.endpoint = Some(SocketAddr::from(([203, 0, 113, 8], WIREGUARD_PORT)));
    let added = installed_peer(3);
    let routes = reconcile(
        &mut installed,
        &[changed.clone(), added.clone()],
        PeerSet::Snapshot,
    );
    assert_eq!(
        installed,
        HashMap::from([
            (changed.public_key.clone(), changed.clone()),
            (added.public_key.clone(), added.clone()),
        ])
    );
    assert_eq!(routes, peer_routes([&changed, &added].into_iter()).unwrap());
    assert!(!routes.contains(&"10.210.2.0/24".parse().unwrap()));

    let mut live = changed.clone();
    live.last_handshake = Some(SystemTime::now());
    live.rx_bytes = 99;
    installed.insert(live.public_key.clone(), live.clone());
    reconcile(&mut installed, &[changed, added], PeerSet::Snapshot);
    assert_eq!(
        installed.get(&live.public_key),
        Some(&live),
        "runtime telemetry does not replace a matching peer"
    );
    assert!(reconcile(&mut installed, &[], PeerSet::Snapshot).is_empty());
    assert!(
        installed.is_empty(),
        "a complete empty observation permits removal"
    );
}

fn machine(seed: u8, public_key: WireGuardPublicKey) -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::parse(format!("{seed:032x}")).unwrap(),
        name: ployz_core::MachineName::parse(format!("machine-{seed}")).unwrap(),
        subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
        public_key,
        public_ip: None,
        advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(SocketAddr::from((
            [192, 0, 2, seed],
            WIREGUARD_PORT,
        )))],
        runtime: Default::default(),
        build_concurrency: None,
    }
}

#[test]
fn retained_device_refuses_wrong_identity_and_wrong_keyed_port() {
    let private_key = WireGuardPrivateKey::from_bytes([7; 32]);
    let mut machine = machine(7, private_key.public_key());
    let host = Host::new(WIREGUARD_PORT, Key::new([7; 32]));
    validate_retained_device(&host, &private_key, &machine, &[]).unwrap();
    for invalid in [
        Host::new(WIREGUARD_PORT, Key::new([8; 32])),
        Host::new(WIREGUARD_PORT + 1, Key::new([7; 32])),
    ] {
        let error = validate_retained_device(&invalid, &private_key, &machine, &[]).unwrap_err();
        assert!(matches!(error, NetworkError::WireGuardConflict { .. }));
        assert!(!error.to_string().contains(&private_key.encoded()));
    }
    machine.public_key = WireGuardPublicKey([9; 32]);
    assert!(validate_machine_identity(&private_key, machine.public_key).is_err());
    for host in [host, Host::default()] {
        assert!(validate_retained_device(&host, &private_key, &machine, &[]).is_err());
    }
}

#[test]
fn interrupted_cold_device_recovers_only_without_peers_or_unrelated_addresses() {
    let private_key = WireGuardPrivateKey::from_bytes([7; 32]);
    let machine = machine(7, private_key.public_key());
    let management = IpAddrMask::host(IpAddr::V6(machine.management_address().0));
    for port in [0, WIREGUARD_PORT, 51855] {
        let mut host = Host::default();
        host.listen_port = port;
        validate_retained_device(&host, &private_key, &machine, &[]).unwrap();
        validate_retained_device(
            &host,
            &private_key,
            &machine,
            std::slice::from_ref(&management),
        )
        .unwrap();
        for address in [
            IpAddrMask::new(management.address, 64),
            IpAddrMask::host("fdcc::1234".parse().unwrap()),
            IpAddrMask::host("192.0.2.1".parse().unwrap()),
        ] {
            assert!(
                validate_retained_device(
                    &host,
                    &private_key,
                    &machine,
                    &[management.clone(), address]
                )
                .is_err()
            );
        }
        let peer = installed_peer(1);
        host.peers.insert(peer.public_key.clone(), peer);
        assert!(validate_retained_device(&host, &private_key, &machine, &[]).is_err());
    }
}

#[test]
fn first_snapshot_preserves_kernel_endpoint_and_updates_allowed_ips() {
    use super::super::{PEER_DOWN_INTERVAL, PeerStatus};
    use std::collections::BTreeMap;

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    let remote = machine(1, WireGuardPublicKey([1; 32]));
    let observer = machine(2, WireGuardPublicKey([2; 32]));
    let snapshot = ReplicatedObservations {
        observations: vec![remote.clone()],
        incomplete_ids: Vec::new(),
    };
    let mut live = installed_peer(1);
    live.endpoint = Some(SocketAddr::from(([203, 0, 113, 7], WIREGUARD_PORT)));
    live.last_handshake = Some(now);
    let selected = SelectedEndpoint(live.endpoint.unwrap());
    for joining in [false, true] {
        for saved in [
            BTreeMap::new(),
            BTreeMap::from([(
                remote.id,
                SelectedEndpoint(remote.advertised_endpoints.first().unwrap().0),
            )]),
        ] {
            let mut installed = HashMap::from([(live.public_key.clone(), live.clone())]);
            let bootstrap = if joining {
                peers_for(&observer.id, &snapshot.observations)
            } else {
                Vec::new()
            };
            reconcile(
                &mut installed,
                &bootstrap
                    .iter()
                    .map(|peer| wireguard_peer(peer, peer.selected()))
                    .collect::<Vec<_>>(),
                PeerSet::Bootstrap,
            );
            let planned = snapshot_peers(&observer.id, &snapshot, &bootstrap, joining).unwrap();
            let mut previous = Vec::new();
            seed_retained_selections(&planned, &mut previous, &installed, now);
            let (mut planned, _) = attach_peer_selections(planned, previous, &saved, now);
            let peer = planned.first_mut().unwrap();
            assert_eq!(peer.selected(), Some(selected));
            assert_eq!(peer.selection.status(now, Some(now)), PeerStatus::Up);
            let wg = wireguard_peer(peer, peer.selected());
            reconcile(&mut installed, std::slice::from_ref(&wg), PeerSet::Snapshot);
            let retained = installed.get(&live.public_key).unwrap();
            assert_eq!(retained.endpoint, live.endpoint);
            assert_eq!(retained.allowed_ips, wg.allowed_ips);
            assert!(
                retained
                    .allowed_ips
                    .contains(&IpAddrMask::host(IpAddr::V6(remote.management_address().0)))
            );
            assert_eq!(
                peer.poll(
                    now + PEER_DOWN_INTERVAL + Duration::from_secs(1),
                    Some(now),
                    live.endpoint
                ),
                Some(SelectedEndpoint(
                    remote.advertised_endpoints.first().unwrap().0
                ))
            );
        }
    }
}

#[test]
fn retained_selection_seeding_matches_keys_and_preserves_previous_choices() {
    let now = SystemTime::UNIX_EPOCH;
    let peer = machine(1, WireGuardPublicKey([1; 32]));
    let observer = machine(2, WireGuardPublicKey([2; 32]));
    let planned = peers_for(&observer.id, std::slice::from_ref(&peer));
    let live = installed_peer(1);
    let other = installed_peer(3);
    let mut previous = Vec::new();
    seed_retained_selections(
        &planned,
        &mut previous,
        &HashMap::from([(other.public_key.clone(), other)]),
        now,
    );
    assert!(previous.is_empty());
    let endpoint = SelectedEndpoint(SocketAddr::from(([203, 0, 113, 9], WIREGUARD_PORT)));
    let (chosen, _) = attach_peer_selections(
        planned.clone(),
        Vec::new(),
        &std::collections::BTreeMap::from([(peer.id, endpoint)]),
        now,
    );
    previous = chosen;
    seed_retained_selections(
        &planned,
        &mut previous,
        &HashMap::from([(live.public_key.clone(), live)]),
        now,
    );
    let (planned, _) = attach_peer_selections(planned, previous, &Default::default(), now);
    assert_eq!(planned.first().unwrap().selected(), Some(endpoint));
}

#[test]
fn incomplete_snapshot_holds_peers_and_joining_keeps_bootstrap() {
    let machine_id = MachineId::random();
    let bootstrap_id = MachineId::random();
    let bootstrap = MeshPeer {
        machine_id: bootstrap_id,
        public_key: WireGuardPublicKey([1; 32]),
        allowed_ips: [
            "fdcc::1/128".parse().unwrap(),
            "10.210.1.0/24".parse().unwrap(),
        ],
        selection: super::super::EndpointSelection::new(&[], None, SystemTime::now()),
    };
    let mut snapshot = ReplicatedObservations {
        observations: Vec::new(),
        incomplete_ids: vec![bootstrap_id],
    };
    for joining in [false, true] {
        assert!(
            snapshot_peers(
                &machine_id,
                &snapshot,
                std::slice::from_ref(&bootstrap),
                joining
            )
            .is_none()
        );
    }
    snapshot.incomplete_ids.clear();
    assert_eq!(
        snapshot_peers(
            &machine_id,
            &snapshot,
            std::slice::from_ref(&bootstrap),
            true
        ),
        Some(vec![bootstrap.clone()])
    );
    assert_eq!(
        snapshot_peers(&machine_id, &snapshot, &[bootstrap], false),
        Some(Vec::new())
    );
}

#[test]
fn network_conflict_names_observed_values_without_debug_wrappers() {
    let mut network = stale_network("ployz", Some(""), true);
    network.options = Some(required_docker_network_options(1400));
    network
        .labels
        .as_mut()
        .unwrap()
        .insert("key\n".into(), "value\u{1b}[2J".into());
    network
        .options
        .as_mut()
        .unwrap()
        .insert("option\n".into(), "setting\u{1b}[2J".into());
    network.driver = Some("bridge\u{1b}[2J".into());

    let message = docker_network_conflict(
        &network,
        "10.0.0.0/24",
        "10.0.0.1",
        &required_docker_network_options(1420),
        "containers are attached",
    )
    .to_string();
    assert!(!message.chars().any(char::is_control), "{message}");
    for detail in [
        "name=ployz",
        "id=unknown",
        "7074faa8a368",
        r#"mtu"="1400""#,
        r#"mtu"="1420""#,
        r#""key\n"="value\u{1b}[2J""#,
        r#""option\n"="setting\u{1b}[2J""#,
        r#"driver="bridge\u{1b}[2J""#,
    ] {
        assert!(message.contains(detail), "{message}");
    }
    assert!(
        !message.contains("Some(") && !message.contains("NetworkInspect"),
        "{message}"
    );
}

fn stale_network(name: &str, managed_label: Option<&str>, attached: bool) -> NetworkInspect {
    NetworkInspect {
        name: Some(name.into()),
        labels: managed_label
            .map(|value| HashMap::from([(DOCKER_NETWORK_MANAGED_LABEL.into(), value.into())])),
        containers: attached.then(|| HashMap::from([("7074faa8a368".into(), Default::default())])),
        ..Default::default()
    }
}

#[test]
fn network_diagnostics_preserve_field_and_collection_boundaries() {
    let describe = |network: &NetworkInspect| {
        docker_network_conflict(
            network,
            "10.0.0.0/24",
            "10.0.0.1",
            &HashMap::new(),
            "conflict",
        )
        .to_string()
    };
    let mut one = stale_network("ployz", None, false);
    let mut two = one.clone();
    one.labels = Some(HashMap::from([("a".into(), "b, c=d".into())]));
    two.labels = Some(HashMap::from([
        ("a".into(), "b".into()),
        ("c".into(), "d".into()),
    ]));
    assert_ne!(describe(&one), describe(&two));
    one.options = one.labels.take();
    two.options = two.labels.take();
    assert_ne!(describe(&one), describe(&two));
    one.options = None;
    two.options = None;
    one.containers = Some(HashMap::from([("a, b".into(), Default::default())]));
    two.containers = Some(HashMap::from([
        ("a".into(), Default::default()),
        ("b".into(), Default::default()),
    ]));
    assert_ne!(describe(&one), describe(&two));
    one.name = Some("ployz, driver=bridge".into());
    assert!(describe(&one).contains(r#"name="ployz, driver=bridge""#));
    one.ipam = Some(Ipam {
        config: Some(vec![IpamConfig {
            subnet: Some("net, gateway=elsewhere".into()),
            ..Default::default()
        }]),
        ..Default::default()
    });
    assert!(describe(&one).contains(r#"subnet="net, gateway=elsewhere""#));
}

#[test]
fn stale_network_deletion_requires_exact_name_label_and_no_attachments() {
    assert_eq!(
        stale_network_replacement_allowed(&stale_network("ployz", Some(""), false)),
        Ok(())
    );

    for network in [
        stale_network("ployz-old", Some(""), false),
        stale_network("ployz", None, false),
        stale_network("ployz", Some("other-owner"), false),
        stale_network("ployz", Some(""), true),
    ] {
        assert!(stale_network_replacement_allowed(&network).is_err());
    }
}

#[test]
fn matching_network_is_reused_with_attached_containers() {
    let mut network = stale_network("ployz", Some(""), true);
    network.driver = Some("bridge".into());
    network.scope = Some("local".into());
    network.ipam = Some(Ipam {
        config: Some(vec![IpamConfig {
            subnet: Some("10.210.1.0/24".into()),
            gateway: Some("10.210.1.1".into()),
            ..Default::default()
        }]),
        ..Default::default()
    });
    let expected_options = required_docker_network_options(1420);
    network.options = Some(expected_options.clone());
    network.id = Some("owned-network".into());
    assert_eq!(
        reset_network_id(&network, "10.210.1.0/24", "10.210.1.1", &expected_options).unwrap(),
        "owned-network"
    );
    for invalid in [
        NetworkInspect {
            labels: None,
            ..network.clone()
        },
        NetworkInspect {
            id: None,
            ..network.clone()
        },
        NetworkInspect {
            name: Some("other-network".into()),
            ..network.clone()
        },
    ] {
        assert!(
            reset_network_id(&invalid, "10.210.1.0/24", "10.210.1.1", &expected_options).is_err()
        );
    }
    assert!(reset_network_id(&network, "10.210.2.0/24", "10.210.2.1", &expected_options).is_err());

    assert!(docker_network_matches(
        &network,
        "10.210.1.0/24",
        "10.210.1.1",
        &expected_options,
    ));
}

#[test]
fn stale_network_refusal_is_actionable() {
    let network = stale_network("ployz", Some(""), true);
    let expected_options = required_docker_network_options(1420);
    let error = docker_network_conflict(
        &network,
        "10.210.1.0/24",
        "10.210.1.1",
        &expected_options,
        "containers are attached",
    )
    .to_string();

    assert!(error.contains("expected: name=ployz"));
    assert!(error.contains("subnet=10.210.1.0/24"));
    assert!(error.contains(r#"observed: id=unknown, name="ployz""#));
    assert!(error.contains("7074faa8a368"));
    assert!(error.contains("systemctl stop ployz"));
    assert!(error.contains("docker network inspect ployz"));
    assert!(error.contains("docker network rm ployz"));
    assert!(error.contains("systemctl start ployz"));
}
