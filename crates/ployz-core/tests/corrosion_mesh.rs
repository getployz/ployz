use std::net::Ipv6Addr;

use base64::Engine as _;
use ployz_core::corrosion::{
    BuiltinWireguardFenceReason, BuiltinWireguardKeyMismatch, BuiltinWireguardMeshOutcome,
    ClusterDocument, DesiredMachineContainerRoute, EbpfMeshDegradationReason, EbpfMeshDegraded,
    MachineDocument, MachineTransport, MeshComponentReady, MeshConvergenceTestimony,
    MeshDegradation, NamedAcceptedRow, NamedReadReport, PeerDocument, RosterMemberId, StoredRow,
    derive_builtin_wireguard_cluster_prefix, derive_builtin_wireguard_member,
    project_builtin_wireguard_mesh,
};
use ployz_core::ids::{ClusterId, CorrosionUlid, MachineRowId};
use ployz_core::network::WireGuardPublicKey;
use serde_json::json;
use sha2::{Digest, Sha256};

const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const MACHINE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const MACHINE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const MACHINE_C: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const ZERO_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const ONE_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
const TWO_KEY: &str = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";

fn cluster() -> ClusterDocument {
    serde_json::from_value(json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "written_by": { "kind": "machine", "machine_id": MACHINE_ID },
        "written_at": "2026-08-04T10:00:00Z",
        "name": "mesh-test",
        "storage_default": "plain",
        "hostname_mode": { "mode": "disabled" },
        "prefix": "10.210.0.0/16",
        "provider": "builtin_wireguard",
        "acme_directory_url": "https://acme.example/directory",
        "acme_contact": null
    }))
    .expect("cluster fixture")
}

fn machine_row(id: &str, name: &str, key: &str, subnet: &str) -> NamedAcceptedRow<MachineDocument> {
    let cluster_id = ClusterId::try_new(CLUSTER_ID).expect("cluster id");
    let public_key = WireGuardPublicKey::try_new(key).expect("public key");
    let address = derive_builtin_wireguard_member(&cluster_id, &public_key).bind_address();
    let value = serde_json::from_value(json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "written_by": { "kind": "machine", "machine_id": MACHINE_ID },
        "written_at": "2026-08-04T10:00:00Z",
        "name": name,
        "lifecycle": "active",
        "transport": {
            "kind": "wireguard",
            "pubkey": key,
            "addr_v6": address.to_string(),
            "endpoint": null,
            "subnet_v4": subnet
        },
        "storage": { "mode": "plain", "reason": { "kind": "default" } }
    }))
    .expect("machine fixture");
    NamedAcceptedRow {
        id: CorrosionUlid::try_new(id).expect("row id"),
        source: StoredRow::new(id, "fixture"),
        value,
    }
}

fn peer_row(id: &str, name: &str, key: &str) -> NamedAcceptedRow<PeerDocument> {
    let cluster_id = ClusterId::try_new(CLUSTER_ID).expect("cluster id");
    let public_key = WireGuardPublicKey::try_new(key).expect("public key");
    let address = derive_builtin_wireguard_member(&cluster_id, &public_key).bind_address();
    let value = serde_json::from_value(json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "written_by": { "kind": "machine", "machine_id": MACHINE_ID },
        "written_at": "2026-08-04T10:00:00Z",
        "name": name,
        "transport": {
            "kind": "wireguard",
            "pubkey": key,
            "addr_v6": address.to_string(),
            "endpoint": null
        }
    }))
    .expect("peer fixture");
    NamedAcceptedRow {
        id: CorrosionUlid::try_new(id).expect("row id"),
        source: StoredRow::new(id, "fixture"),
        value,
    }
}

fn report<Document>(accepted: Vec<NamedAcceptedRow<Document>>) -> NamedReadReport<Document> {
    NamedReadReport {
        accepted,
        skipped: Vec::new(),
        shadows: Vec::new(),
    }
}

#[test]
fn builtin_wireguard_ula_derivation_matches_fixed_vectors() {
    let cluster_id = ClusterId::try_new(CLUSTER_ID).expect("cluster id");
    let public_key = WireGuardPublicKey::try_new(ZERO_KEY).expect("public key");

    assert_eq!(
        derive_builtin_wireguard_cluster_prefix(&cluster_id).to_string(),
        "fd8e:ac53:b3f1::/48"
    );
    let member = derive_builtin_wireguard_member(&cluster_id, &public_key);
    assert_eq!(
        member.subnet().to_string(),
        "fd8e:ac53:b3f1:6668:7aad:f862:bd77:0/112"
    );
    assert_eq!(
        member.bind_address().get(),
        "fd8e:ac53:b3f1:6668:7aad:f862:bd77:1"
            .parse::<Ipv6Addr>()
            .expect("fixed address")
    );
}

#[test]
fn empty_accepted_roster_is_an_explicit_no_roster_outcome() {
    let local_machine_id = MachineRowId::try_new(MACHINE_ID).expect("machine id");
    let local_public_key = WireGuardPublicKey::try_new(ZERO_KEY).expect("public key");
    let machines = NamedReadReport::<MachineDocument> {
        accepted: Vec::new(),
        skipped: Vec::new(),
        shadows: Vec::new(),
    };
    let peers = NamedReadReport::<PeerDocument> {
        accepted: Vec::new(),
        skipped: Vec::new(),
        shadows: Vec::new(),
    };

    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        local_machine_id,
        &local_public_key,
        machines,
        peers,
    )
    .expect("builtin provider");
    assert!(matches!(
        outcome,
        BuiltinWireguardMeshOutcome::NoRoster { .. }
    ));
}

#[test]
fn one_machine_roster_converges_with_no_remote_work() {
    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(vec![machine_row(
            MACHINE_ID,
            "local",
            ZERO_KEY,
            "10.210.1.0/24",
        )]),
        report(Vec::new()),
    )
    .expect("projection");

    let BuiltinWireguardMeshOutcome::Desired(desired) = outcome else {
        panic!("one accepted self row is desired state");
    };
    assert!(desired.machine_peers.is_empty());
    assert!(desired.roaming_peers.is_empty());
    assert!(desired.ebpf_routes.is_empty());
}

#[test]
fn nonempty_roster_missing_self_is_fenced() {
    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(vec![machine_row(
            MACHINE_B,
            "remote",
            ONE_KEY,
            "10.210.2.0/24",
        )]),
        report(Vec::new()),
    )
    .expect("projection");

    assert!(matches!(
        outcome,
        BuiltinWireguardMeshOutcome::Fenced { .. }
    ));
}

#[test]
fn peers_without_any_machine_row_fence_instead_of_looking_like_an_empty_roster() {
    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(Vec::new()),
        report(vec![peer_row(MACHINE_B, "operator", ONE_KEY)]),
    )
    .expect("projection");

    assert!(matches!(
        outcome,
        BuiltinWireguardMeshOutcome::Fenced {
            reason: BuiltinWireguardFenceReason::MissingLocalMachine,
            ..
        }
    ));
}

#[test]
fn local_public_key_mismatch_blocks_the_fold() {
    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ONE_KEY).expect("local key"),
        report(vec![machine_row(
            MACHINE_ID,
            "local",
            ZERO_KEY,
            "10.210.1.0/24",
        )]),
        report(Vec::new()),
    )
    .expect("projection");

    assert!(matches!(
        outcome,
        BuiltinWireguardMeshOutcome::KeyMismatch { mismatches, .. }
            if matches!(mismatches.as_slice(), [BuiltinWireguardKeyMismatch::LocalPublicKey { .. }])
    ));
}

#[test]
fn bad_remote_stored_address_is_excluded_without_freezing_valid_convergence() {
    let mut remote = peer_row(MACHINE_B, "operator", ONE_KEY);
    let PeerDocument { transport, .. } = &mut remote.value;
    let ployz_core::corrosion::PeerTransport::Wireguard { addr_v6, .. } = transport else {
        panic!("wireguard fixture");
    };
    *addr_v6 = "fd00::bad".parse().expect("IPv6");

    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(vec![machine_row(
            MACHINE_ID,
            "local",
            ZERO_KEY,
            "10.210.1.0/24",
        )]),
        report(vec![remote]),
    )
    .expect("projection");

    let BuiltinWireguardMeshOutcome::Desired(desired) = outcome else {
        panic!("valid local roster still converges");
    };
    assert!(desired.roaming_peers.is_empty());
    assert_eq!(desired.evidence.address_mismatches.len(), 1);
}

#[test]
fn malformed_low_id_remote_cannot_shadow_a_valid_higher_id_identity() {
    let mut malformed = peer_row(MACHINE_B, "malformed", ONE_KEY);
    let ployz_core::corrosion::PeerTransport::Wireguard { addr_v6, .. } =
        &mut malformed.value.transport
    else {
        panic!("wireguard fixture");
    };
    *addr_v6 = "fd00::bad".parse().expect("IPv6");
    let valid = peer_row(MACHINE_C, "valid", ONE_KEY);

    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(vec![machine_row(
            MACHINE_ID,
            "local",
            ZERO_KEY,
            "10.210.1.0/24",
        )]),
        report(vec![malformed, valid]),
    )
    .expect("projection");

    let BuiltinWireguardMeshOutcome::Desired(desired) = outcome else {
        panic!("valid local roster still converges");
    };
    assert!(matches!(
        desired.roaming_peers.as_slice(),
        [peer] if peer.peer_id.as_str() == MACHINE_C
    ));
    assert_eq!(desired.evidence.address_mismatches.len(), 1);
    assert!(desired.evidence.identity_conflicts.is_empty());
}

#[test]
fn duplicate_identity_loser_cannot_shadow_valid_higher_id_subnet_claim() {
    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(vec![
            machine_row(MACHINE_ID, "local", ZERO_KEY, "10.210.1.0/24"),
            machine_row(MACHINE_B, "identity-loser", ZERO_KEY, "10.210.2.0/24"),
            machine_row(MACHINE_C, "subnet-owner", TWO_KEY, "10.210.2.0/24"),
        ]),
        report(Vec::new()),
    )
    .expect("projection");

    let BuiltinWireguardMeshOutcome::Desired(desired) = outcome else {
        panic!("the accepted identities have deterministic desired state");
    };
    assert!(matches!(
        desired.machine_peers.as_slice(),
        [peer]
            if peer.machine_id.as_str() == MACHINE_C
                && matches!(
                    &peer.container_route,
                    DesiredMachineContainerRoute::Claimed { subnet }
                        if subnet.as_string() == "10.210.2.0/24"
                )
    ));
    assert!(
        desired
            .ebpf_routes
            .iter()
            .any(|subnet| subnet.as_string() == "10.210.2.0/24")
    );
    assert!(desired.evidence.identity_conflicts.iter().all(|conflict| {
        conflict.loser
            != RosterMemberId::Machine {
                machine_id: MachineRowId::try_new(MACHINE_C).expect("machine C id"),
            }
    }));
}

#[test]
fn duplicate_subnet_loser_keeps_ipv6_but_loses_v4_routes() {
    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(vec![
            machine_row(MACHINE_ID, "local", ZERO_KEY, "10.210.1.0/24"),
            machine_row(MACHINE_B, "winner", ONE_KEY, "10.210.2.0/24"),
            machine_row(MACHINE_C, "loser", TWO_KEY, "10.210.2.0/24"),
        ]),
        report(Vec::new()),
    )
    .expect("projection");

    let BuiltinWireguardMeshOutcome::Desired(desired) = outcome else {
        panic!("duplicate subnet has deterministic desired state");
    };
    assert_eq!(desired.machine_peers.len(), 2);
    assert_eq!(desired.ebpf_routes.len(), 1);
    assert!(matches!(
        desired.machine_peers.as_slice(),
        [_, loser]
            if matches!(loser.container_route, DesiredMachineContainerRoute::Withheld { .. })
    ));
    assert_eq!(desired.evidence.identity_conflicts.len(), 1);
}

#[test]
fn local_duplicate_subnet_loser_gets_a_typed_reallocation_input() {
    let winning_machine = machine_row(CLUSTER_ID, "winner", ONE_KEY, "10.210.1.0/24");
    let local_machine = machine_row(MACHINE_ID, "local", ZERO_KEY, "10.210.1.0/24");
    let expected_local_document = local_machine.value.clone();

    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(vec![
            local_machine,
            machine_row(MACHINE_B, "remote", TWO_KEY, "10.210.2.0/24"),
            winning_machine,
        ]),
        report(Vec::new()),
    )
    .expect("projection");

    let BuiltinWireguardMeshOutcome::ReallocateLocalContainerSubnet { repair, evidence } = outcome
    else {
        panic!("the higher-ULID local duplicate must reallocate");
    };
    assert_eq!(repair.machine_id().as_str(), MACHINE_ID);
    assert_eq!(repair.accepted_machine(), &expected_local_document);
    assert_eq!(
        repair
            .occupied_subnets()
            .iter()
            .map(|subnet| subnet.as_string())
            .collect::<Vec<_>>(),
        vec!["10.210.1.0/24", "10.210.2.0/24"]
    );
    assert!(matches!(
        evidence.identity_conflicts.as_slice(),
        [conflict]
            if conflict.winner == RosterMemberId::Machine {
                machine_id: MachineRowId::try_new(CLUSTER_ID).expect("winner id"),
            }
            && conflict.loser == RosterMemberId::Machine {
                machine_id: MachineRowId::try_new(MACHINE_ID).expect("local id"),
            }
    ));

    let (_, accepted_machine, _) = repair.into_parts();
    assert!(matches!(
        accepted_machine.transport,
        MachineTransport::Wireguard { subnet_v4, .. }
            if subnet_v4.as_string() == "10.210.1.0/24"
    ));
}

#[test]
fn lowest_ulid_local_subnet_winner_withholds_only_the_remote_ipv4_route() {
    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(vec![
            machine_row(MACHINE_ID, "local", ZERO_KEY, "10.210.1.0/24"),
            machine_row(MACHINE_B, "remote-loser", ONE_KEY, "10.210.1.0/24"),
        ]),
        report(Vec::new()),
    )
    .expect("projection");

    let BuiltinWireguardMeshOutcome::Desired(desired) = outcome else {
        panic!("the lowest-ULID local claim remains desired state");
    };
    assert!(desired.ebpf_routes.is_empty());
    assert!(matches!(
        desired.machine_peers.as_slice(),
        [peer]
            if matches!(
                &peer.container_route,
                DesiredMachineContainerRoute::Withheld { subnet, winner }
                    if subnet.as_string() == "10.210.1.0/24"
                        && winner == &RosterMemberId::Machine {
                            machine_id: MachineRowId::try_new(MACHINE_ID).expect("local id"),
                        }
            )
    ));
}

#[test]
fn duplicate_key_loser_is_removed_from_peer_projection_with_both_claims_visible() {
    let outcome = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(vec![
            machine_row(MACHINE_ID, "local", ZERO_KEY, "10.210.1.0/24"),
            machine_row(MACHINE_B, "winner", ONE_KEY, "10.210.2.0/24"),
        ]),
        report(vec![peer_row(MACHINE_C, "loser", ONE_KEY)]),
    )
    .expect("projection");

    let BuiltinWireguardMeshOutcome::Desired(desired) = outcome else {
        panic!("duplicate identity has deterministic desired state");
    };
    assert_eq!(desired.machine_peers.len(), 1);
    assert!(desired.roaming_peers.is_empty());
    assert_eq!(desired.evidence.identity_conflicts.len(), 2);
}

#[test]
fn degraded_testimony_keeps_wireguard_ready_when_ebpf_is_degraded() {
    let at = ployz_core::corrosion::CorrosionTimestamp::try_new("2026-08-04T10:00:00Z")
        .expect("timestamp");
    let testimony = MeshConvergenceTestimony::Degraded {
        bind_address: derive_builtin_wireguard_member(
            &ClusterId::try_new(CLUSTER_ID).expect("cluster id"),
            &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        )
        .bind_address(),
        attempted_at: at,
        last_successful_converge: None,
        degradation: MeshDegradation::Ebpf {
            wireguard: MeshComponentReady { converged_at: at },
            ebpf: EbpfMeshDegraded {
                reason: EbpfMeshDegradationReason::MissingBridge {
                    ifname: "ployz0".to_owned(),
                },
            },
        },
    };

    let value = serde_json::to_value(testimony).expect("testimony JSON");
    assert_eq!(
        value,
        json!({
            "state": "degraded",
            "bind_address": "fd8e:ac53:b3f1:6668:7aad:f862:bd77:1",
            "attempted_at": "2026-08-04T10:00:00.000000000Z",
            "last_successful_converge": null,
            "degradation": {
                "components": "ebpf",
                "wireguard": {
                    "converged_at": "2026-08-04T10:00:00.000000000Z"
                },
                "ebpf": {
                    "reason": {
                        "kind": "missing_bridge",
                        "ifname": "ployz0"
                    }
                }
            }
        })
    );
}

#[test]
fn accepted_roster_over_the_v1_member_ceiling_is_rejected_before_host_work() {
    let peers = (1_u128..=256)
        .map(|number| {
            let id = ulid::Ulid::from(number).to_string();
            let key = base64::engine::general_purpose::STANDARD
                .encode(Sha256::digest(number.to_be_bytes()));
            peer_row(&id, &format!("peer-{number}"), &key)
        })
        .collect();

    let error = project_builtin_wireguard_mesh(
        &cluster(),
        MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        &WireGuardPublicKey::try_new(ZERO_KEY).expect("public key"),
        report(vec![machine_row(
            MACHINE_ID,
            "local",
            ZERO_KEY,
            "10.210.1.0/24",
        )]),
        report(peers),
    )
    .expect_err("257 members exceeds the bounded v1 fold");

    assert!(matches!(
        error,
        ployz_core::corrosion::BuiltinWireguardMeshError::AcceptedRosterTooLarge {
            accepted_members: 257,
            maximum: 256,
        }
    ));
}
