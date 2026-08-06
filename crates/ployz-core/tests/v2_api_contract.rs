use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use ployz_core::corrosion::{
    AcceptedRosterPrincipal, CorrosionDocumentVersion, CorrosionTimestamp, MachineLoadBand,
    MachineStatusDocument, MachineTransport, OperationInitiator, OperatorWriteProvenance,
    PeerTransport, Principal, SourcePrincipalResolutionError, resolve_source_principal,
};
use ployz_core::ids::{ClusterId, MachineRowId, PeerId};
use ployz_core::network::{MachineEndpointSubnet, WireGuardPublicKey};
use ployz_core::{
    API_MAJOR, ApiFeature, ApiRefusal, ApiVersion, FOUNDING_ROUTE, KnownApiFeature,
    LENS_SNAPSHOT_EVENT, LENS_STATE_EVENT, LENS_TERMINAL_EVENT, LensCollection, LensSnapshot,
    LensWatchEvent, MachineStatusLensRow, MachineStatusLensRowIdentityError, V2Method, V2Route,
    VERSION_ROUTE, lens_route, lens_watch_route,
};
use serde_json::json;

const MACHINE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const MACHINE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const PEER_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const PEER_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

fn machine_id(value: &str) -> MachineRowId {
    MachineRowId::try_new(value).expect("fixture machine id")
}

fn peer_id(value: &str) -> PeerId {
    PeerId::try_new(value).expect("fixture peer id")
}

fn wireguard_machine(addr_v6: Ipv6Addr) -> MachineTransport {
    MachineTransport::Wireguard {
        pubkey: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .expect("fixture public key"),
        addr_v6,
        endpoint: Some(SocketAddr::from_str("198.51.100.10:51820").expect("fixture endpoint")),
        subnet_v4: MachineEndpointSubnet::try_new("10.210.20.0/24").expect("fixture subnet"),
    }
}

fn wireguard_peer(addr_v6: Ipv6Addr) -> PeerTransport {
    PeerTransport::Wireguard {
        pubkey: WireGuardPublicKey::try_new("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=")
            .expect("fixture public key"),
        addr_v6,
        endpoint: Some(SocketAddr::from_str("198.51.100.11:51820").expect("fixture endpoint")),
    }
}

fn tailscale_machine(ip: Ipv4Addr) -> MachineTransport {
    MachineTransport::Tailscale {
        ip,
        subnet_v4: MachineEndpointSubnet::try_new("10.210.30.0/24").expect("fixture subnet"),
    }
}

fn tailscale_peer(ip: Ipv4Addr) -> PeerTransport {
    PeerTransport::Tailscale { ip }
}

#[test]
fn principal_is_the_one_durable_provenance_wire_union() {
    let peer_id = peer_id(PEER_A);
    let principal = Principal::Peer {
        peer_id: peer_id.clone(),
    };
    let alias: OperationInitiator = principal.clone();
    assert_eq!(alias, principal);

    let provenance = OperatorWriteProvenance {
        written_by: alias,
        written_at: CorrosionTimestamp::try_new("2026-08-04T10:00:00Z").expect("fixture timestamp"),
    };
    let serialized = serde_json::to_value(&provenance).expect("provenance serializes");
    assert_eq!(
        serialized,
        json!({
            "written_by": { "kind": "peer", "peer_id": PEER_A },
            "written_at": "2026-08-04T10:00:00.000000000Z"
        })
    );

    let restored: OperatorWriteProvenance =
        serde_json::from_value(serialized).expect("durable provenance deserializes");
    assert_eq!(restored.written_by, principal);
}

#[test]
fn source_resolution_matches_only_accepted_transport_addresses() {
    let machine_wireguard_id = machine_id(MACHINE_A);
    let peer_wireguard_id = peer_id(PEER_A);
    let machine_tailscale_id = machine_id(MACHINE_B);
    let peer_tailscale_id = peer_id(PEER_B);
    let roster = [
        AcceptedRosterPrincipal::machine(
            machine_wireguard_id.clone(),
            wireguard_machine(Ipv6Addr::from_str("fd00::20").expect("fixture address")),
        ),
        AcceptedRosterPrincipal::peer(
            peer_wireguard_id.clone(),
            wireguard_peer(Ipv6Addr::from_str("fd00::21").expect("fixture address")),
        ),
        AcceptedRosterPrincipal::machine(
            machine_tailscale_id.clone(),
            tailscale_machine(Ipv4Addr::new(100, 64, 0, 20)),
        ),
        AcceptedRosterPrincipal::peer(
            peer_tailscale_id.clone(),
            tailscale_peer(Ipv4Addr::new(100, 64, 0, 21)),
        ),
    ];

    for (source, expected) in [
        (
            IpAddr::V6(Ipv6Addr::from_str("fd00::20").expect("fixture address")),
            Principal::Machine {
                machine_id: machine_wireguard_id,
            },
        ),
        (
            IpAddr::V6(Ipv6Addr::from_str("fd00::21").expect("fixture address")),
            Principal::Peer {
                peer_id: peer_wireguard_id,
            },
        ),
        (
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 20)),
            Principal::Machine {
                machine_id: machine_tailscale_id,
            },
        ),
        (
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 21)),
            Principal::Peer {
                peer_id: peer_tailscale_id,
            },
        ),
    ] {
        assert_eq!(
            resolve_source_principal(source, &roster).expect("one accepted address match"),
            expected
        );
    }

    for source in [
        IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10)),
        IpAddr::V4(Ipv4Addr::new(10, 210, 20, 2)),
        IpAddr::V4(Ipv4Addr::new(10, 210, 30, 2)),
    ] {
        assert_eq!(
            resolve_source_principal(source, &roster),
            Err(SourcePrincipalResolutionError::UnknownSource { source }),
            "endpoints and container subnets never authenticate a principal"
        );
    }
}

#[test]
fn duplicate_accepted_addresses_fail_closed_as_ambiguous() {
    let address = Ipv6Addr::from_str("fd00::99").expect("fixture address");
    let source = IpAddr::V6(address);
    let roster = [
        AcceptedRosterPrincipal::machine(machine_id(MACHINE_A), wireguard_machine(address)),
        AcceptedRosterPrincipal::peer(peer_id(PEER_A), wireguard_peer(address)),
    ];

    let error = resolve_source_principal(source, &roster).expect_err("duplicate address fails");
    assert_eq!(
        error,
        SourcePrincipalResolutionError::AmbiguousSource {
            source,
            candidate_count: 2,
        }
    );
    assert_eq!(
        ApiRefusal::from(error),
        ApiRefusal::AmbiguousSource {
            source,
            candidate_count: 2,
        }
    );
}

#[test]
fn machine_status_lens_row_requires_its_machine_owned_key() {
    let document = MachineStatusDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterId::try_new(MACHINE_A).expect("fixture cluster id"),
        machine_id: machine_id(MACHINE_A),
        ployz_version: "0.1.0-alpha.9".to_owned(),
        corrosion_version: "0.2.0".to_owned(),
        architecture: "x86_64".to_owned(),
        free_disk_bytes: 1,
        free_memory_bytes: 1,
        load: MachineLoadBand::Idle,
        observed_at: CorrosionTimestamp::try_new("2026-08-04T10:00:00Z")
            .expect("fixture timestamp"),
        mesh: None,
        container_isolation: None,
        wireguard_handshakes: None,
    };

    assert!(MachineStatusLensRow::try_new(machine_id(MACHINE_A), document.clone()).is_ok());
    assert_eq!(
        MachineStatusLensRow::try_new(machine_id(MACHINE_B), document),
        Err(MachineStatusLensRowIdentityError {
            id: machine_id(MACHINE_B),
            document_machine_id: machine_id(MACHINE_A),
        })
    );
}

#[test]
fn v2_routes_version_and_watch_envelopes_are_stable() {
    assert_eq!(API_MAJOR, 1);
    assert_eq!(VERSION_ROUTE, "/version");
    assert_eq!(FOUNDING_ROUTE, "/founding");
    assert_eq!(V2Route::parse(FOUNDING_ROUTE), Some(V2Route::Founding));
    assert_eq!(V2Route::Founding.path(), FOUNDING_ROUTE);
    assert_eq!(V2Route::Founding.method(), V2Method::Post);
    assert_eq!(V2Route::Version.method(), V2Method::Get);
    assert_eq!(
        V2Route::Lens(LensCollection::Machines).method(),
        V2Method::Get
    );
    assert_eq!(
        V2Route::LensWatch(LensCollection::Machines).method(),
        V2Method::Get
    );
    assert_eq!(
        lens_route(LensCollection::MachineStatus),
        "/lenses/machine_status"
    );
    assert_eq!(
        lens_watch_route(LensCollection::Operations),
        "/lenses/operations/watch"
    );
    assert_eq!(
        V2Route::parse("/lenses/services/watch"),
        Some(V2Route::LensWatch(LensCollection::Services))
    );
    assert_eq!(V2Route::parse("/lenses/services/watch/again"), None);

    let version = ApiVersion::new(
        "0.1.0-alpha.9+abc",
        [
            ApiFeature::from(KnownApiFeature::Founding),
            ApiFeature::from(KnownApiFeature::Lenses),
            ApiFeature::other("future.example"),
        ],
    );
    assert_eq!(
        serde_json::to_value(version).expect("version serializes"),
        json!({
            "major": 1,
            "build": "0.1.0-alpha.9+abc",
            "features": ["v2.founding", "v2.lenses", "future.example"]
        })
    );

    let snapshot = LensSnapshot::Services { rows: Vec::new() };
    let first = LensWatchEvent::snapshot(snapshot.clone());
    assert_eq!(first.event_name(), LENS_SNAPSHOT_EVENT);
    assert_eq!(
        serde_json::to_value(&first).expect("snapshot event serializes"),
        json!({
            "kind": "snapshot",
            "snapshot": { "collection": "services", "rows": [] }
        })
    );
    assert_eq!(
        LensWatchEvent::state(snapshot).event_name(),
        LENS_STATE_EVENT
    );
    assert_eq!(
        LensWatchEvent::terminal(ApiRefusal::MissingCluster).event_name(),
        LENS_TERMINAL_EVENT
    );
}
