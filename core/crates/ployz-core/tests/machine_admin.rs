use std::{collections::BTreeMap, net::IpAddr};

use ployz_core::{
    AdvertisedEndpoint, Machine, MachineId, MachineName, MachineRuntime, MachineUpdate,
    MachineUpdateError, MembershipObservation, PublicIpUpdate, RttStatistics, WireGuardPublicKey,
    apply_machine_update, rtt_statistics, synthesize_membership,
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
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(updated.id, original.id);
    assert_eq!(updated.subnet, original.subnet);
    assert_eq!(updated.management_address(), original.management_address());
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
        (up.management_address(), ployz_core::MembershipEvidence::Up),
        (
            suspect.management_address(),
            ployz_core::MembershipEvidence::Suspect,
        ),
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
    assert!(local.membership.invites_rpc());
    assert!(up.membership.invites_rpc());
    assert!(suspect.membership.invites_rpc());
    assert!(!down.membership.invites_rpc());
    assert!(!MembershipObservation::Unknown.invites_rpc());
    assert!(!MembershipObservation::Unrecognized("gone".into()).invites_rpc());
    assert_eq!(
        observations
            .iter()
            .filter(|observation| observation.machine.name.as_str() == "duplicate")
            .count(),
        2
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

fn machine(id: char, name: &str, seed: u8) -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
        public_key: WireGuardPublicKey([seed; 32]),
        public_ip: Some(IpAddr::from([192, 0, 2, seed])),
        advertised_endpoints: vec![AdvertisedEndpoint(
            format!("192.0.2.{seed}:51820").parse().unwrap(),
        )],
        runtime: MachineRuntime::default(),
        build_concurrency: None,
    }
}

#[test]
fn machine_management_address_is_derived_after_decode_and_key_update() {
    let mut wire = serde_json::to_value(machine('1', "derived", 1)).unwrap();
    *wire.get_mut("public_key").unwrap() = serde_json::json!([
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31
    ]);
    wire.as_object_mut()
        .unwrap()
        .insert("management_address".into(), serde_json::json!("::1"));
    let mut decoded: Machine = serde_json::from_value(wire).unwrap();
    assert_eq!(
        decoded.management_address().0.to_string(),
        "fdcc:1:203:405:607:809:a0b:c0d"
    );
    assert!(
        serde_json::to_value(&decoded)
            .unwrap()
            .get("management_address")
            .is_none()
    );
    decoded.public_key = WireGuardPublicKey([0; 32]);
    assert_eq!(decoded.management_address().0.to_string(), "fdcc::");
}

#[test]
fn label_and_role_patch_preserves_unrelated_metadata() {
    let mut original = machine('1', "first", 1);
    original.labels = BTreeMap::from([
        ("zone".parse().unwrap(), "west".parse().unwrap()),
        ("keep".parse().unwrap(), "yes".parse().unwrap()),
        ("remove".parse().unwrap(), "old".parse().unwrap()),
    ]);
    let patch: MachineUpdate = serde_json::from_value(serde_json::json!({
        "label_changes": {"zone": "east", "remove": null, "missing": null},
        "accepts_services": false
    }))
    .unwrap();
    assert!(!patch.is_empty());
    let updated = apply_machine_update(&original, &[], patch).unwrap();
    assert_eq!(
        updated.labels,
        BTreeMap::from([
            ("zone".parse().unwrap(), "east".parse().unwrap()),
            ("keep".parse().unwrap(), "yes".parse().unwrap())
        ])
    );
    assert!(updated.accepts_builds && updated.accepts_ingress);
    assert!(!updated.accepts_services);
    assert_eq!(updated.name, original.name);
}

#[test]
fn malformed_machine_labels_are_rejected_at_the_wire_boundary() {
    let valid: MachineUpdate = serde_json::from_value(
        serde_json::json!({"label_changes": {"Region": "East", "region": "west"}}),
    )
    .unwrap();
    let updated = apply_machine_update(&machine('1', "first", 1), &[], valid).unwrap();
    assert_eq!(
        serde_json::to_value(&updated)
            .unwrap()
            .get("labels")
            .unwrap(),
        &serde_json::json!({"Region": "East", "region": "west"})
    );
    let wire = serde_json::to_value(machine('1', "first", 1)).unwrap();
    for labels in [
        serde_json::json!({"bad key": "value"}),
        serde_json::json!({"rack/zone": "west"}),
        serde_json::json!({"région": "west"}),
        serde_json::json!({"key": ""}),
        serde_json::json!({"key": " west"}),
        serde_json::json!({"key": "west "}),
        serde_json::json!({"key": "é"}),
        serde_json::json!({"key": "🦀"}),
        serde_json::json!({"key": "K"}),
        serde_json::json!({"key": "ſ"}),
        serde_json::json!({"key": "a=b"}),
        serde_json::json!({"key": "a,b"}),
        serde_json::json!({"key": "a\tb"}),
        serde_json::json!({"key": "bad\nvalue"}),
    ] {
        let mut wire = wire.clone();
        *wire.get_mut("labels").unwrap() = labels.clone();
        assert!(serde_json::from_value::<Machine>(wire).is_err(), "{labels}");
        assert!(serde_json::from_value::<ployz_core::InitialMachinePolicy>(serde_json::json!({
            "labels": labels.clone(), "accepts_builds": true, "accepts_services": true, "accepts_ingress": true
        })).is_err(), "{labels}");
        assert!(
            serde_json::from_value::<MachineUpdate>(serde_json::json!({"label_changes": labels}))
                .is_err()
        );
    }
    assert!(
        serde_json::from_value::<MachineUpdate>(serde_json::json!({"label_changes": {" ": null}}))
            .is_err()
    );
}

#[test]
fn admitted_machine_labels_are_selectable_by_literal_swarm_constraints() {
    for (key, value) in [
        ("rack.zone-1_A", "West"),
        ("Region", r"EU west:/alpha.*()?+[]\^$|_-"),
    ] {
        let patch: MachineUpdate =
            serde_json::from_value(serde_json::json!({"label_changes": {key: value}})).unwrap();
        let selected = apply_machine_update(&machine('1', "first", 1), &[], patch).unwrap();
        let constraint =
            ployz_core::PlacementConstraint::parse(format!("node.labels.{key}=={value}")).unwrap();
        assert!(constraint.matches(&selected));
        let insensitive = ployz_core::PlacementConstraint::parse(format!(
            "node.labels.{key}=={}",
            value.to_ascii_uppercase()
        ))
        .unwrap();
        assert!(insensitive.matches(&selected));
        let missing =
            ployz_core::PlacementConstraint::parse(format!("node.labels.missing!={value}"))
                .unwrap();
        assert!(missing.matches(&selected));
    }
}

#[test]
fn admin_membership_evidence_survives_machine_wire_roundtrip_and_absence_stays_unproven() {
    use ployz_core::{MachineList, MembershipEvidence};
    let entry = machine('1', "entry", 1);
    let peer = machine('2', "peer", 2);
    for raw in ["down", "up", "suspect", "future-state"] {
        let states = BTreeMap::from([(
            peer.management_address(),
            MembershipEvidence::Unrecognized { raw: raw.into() },
        )]);
        let response = MachineList {
            enrollment: None,
            machines: synthesize_membership(vec![entry.clone(), peer.clone()], &entry.id, &states),
        };
        let decoded: MachineList =
            serde_json::from_value(serde_json::to_value(response).unwrap()).unwrap();
        let observed = decoded
            .machines
            .iter()
            .find(|row| row.machine.id == peer.id)
            .unwrap();
        assert_eq!(
            observed.membership_evidence,
            Some(MembershipEvidence::Unrecognized { raw: raw.into() })
        );
        assert!(
            !observed.invites_rpc(),
            "unknown raw {raw} must not become a responding peer"
        );
    }
    let rows = synthesize_membership(
        vec![entry.clone(), peer.clone()],
        &entry.id,
        &BTreeMap::new(),
    );
    let absent = rows.iter().find(|row| row.machine.id == peer.id).unwrap();
    assert_eq!(absent.membership, MembershipObservation::Down);
    assert_eq!(absent.membership_evidence, None);
    let states = BTreeMap::from([(peer.management_address(), MembershipEvidence::Down)]);
    let rows = synthesize_membership(vec![peer.clone()], &entry.id, &states);
    assert_eq!(
        rows.first().unwrap().membership_evidence,
        Some(MembershipEvidence::Down)
    );
    let legacy: ployz_core::MachineObservation =
        serde_json::from_value(serde_json::to_value(absent).unwrap()).unwrap();
    assert_eq!(legacy.membership_evidence, None);
}
