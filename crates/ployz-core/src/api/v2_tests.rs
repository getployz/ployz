use serde_json::json;

use super::*;
use crate::ids::PeerId;

fn machine_id(value: &str) -> MachineRowId {
    MachineRowId::try_new(value).expect("fixture machine id")
}

fn machine_name() -> MachineName {
    MachineName::try_new("edge-a").expect("fixture machine name")
}

#[test]
fn machine_remove_has_one_exact_route_feature_method_and_principal_policy() {
    let route = V2Route::MachineRemove;
    assert_eq!(route.path(), MACHINE_REMOVE_ROUTE);
    assert_eq!(V2Route::parse(MACHINE_REMOVE_ROUTE), Some(route.clone()));
    assert_eq!(route.method(), V2Method::Post);
    assert_eq!(route.feature(), KnownApiFeature::MachineRemove);
    assert!(route.accepts_principal(&Principal::Peer {
        peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("fixture peer id"),
    }));
    assert!(!route.accepts_principal(&Principal::Machine {
        machine_id: machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
    }));
    assert!(!route.accepts_principal(&Principal::ApiToken {
        token_id: TokenId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("fixture token id"),
    }));
}

#[test]
fn machine_remove_contract_serializes_the_optional_id_and_typed_outcomes() {
    let request = MachineRemoveRequest {
        machine_name: machine_name(),
        machine_id: None,
    };
    assert_eq!(
        serde_json::to_value(&request).expect("request serializes"),
        json!({ "machine_name": "edge-a" })
    );
    let machine_id = machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAV");
    let reply = MachineRemoveReply::AlreadyAbsent {
        machine_id: machine_id.clone(),
    };
    assert_eq!(
        serde_json::to_value(reply).expect("reply serializes"),
        json!({ "kind": "already_absent", "machine_id": machine_id.as_str() })
    );
    let refusal = MachineRemoveRefusal::IdMismatch {
        machine_name: machine_name(),
        machine_id: machine_id.clone(),
    };
    assert_eq!(
        serde_json::from_value::<MachineRemoveRefusal>(
            serde_json::to_value(&refusal).expect("refusal serializes"),
        )
        .expect("refusal deserializes"),
        refusal
    );
}

#[test]
fn machine_remove_selection_requires_an_unambiguous_accepted_roster_row() {
    let lower = machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAV");
    let higher = machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAW");
    let request = MachineRemoveRequest {
        machine_name: machine_name(),
        machine_id: None,
    };
    assert_eq!(
        select_machine_removal(
            &request,
            [
                (higher.clone(), machine_name()),
                (lower.clone(), machine_name()),
            ],
        ),
        Err(MachineRemoveRefusal::Ambiguous {
            machine_name: machine_name(),
            machine_ids: vec![lower.clone(), higher.clone()],
        })
    );

    let request = MachineRemoveRequest {
        machine_name: machine_name(),
        machine_id: Some(higher.clone()),
    };
    assert_eq!(
        select_machine_removal(
            &request,
            [
                (lower.clone(), machine_name()),
                (higher.clone(), machine_name()),
            ],
        ),
        Ok(higher.clone())
    );

    let missing = machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAX");
    let request = MachineRemoveRequest {
        machine_name: machine_name(),
        machine_id: Some(missing.clone()),
    };
    assert_eq!(
        select_machine_removal(&request, [(lower, machine_name())]),
        Err(MachineRemoveRefusal::IdMismatch {
            machine_name: machine_name(),
            machine_id: missing,
        })
    );

    let request = MachineRemoveRequest {
        machine_name: MachineName::try_new("edge-b").expect("fixture machine name"),
        machine_id: Some(higher.clone()),
    };
    assert_eq!(
        select_machine_removal(&request, [(higher.clone(), machine_name())]),
        Err(MachineRemoveRefusal::IdMismatch {
            machine_name: MachineName::try_new("edge-b").expect("fixture machine name"),
            machine_id: higher,
        })
    );
}

#[test]
fn peer_remove_has_one_exact_route_feature_method_and_principal_policy() {
    let route = V2Route::PeerRemove;
    assert_eq!(route.path(), PEER_REMOVE_ROUTE);
    assert_eq!(V2Route::parse(PEER_REMOVE_ROUTE), Some(route.clone()));
    assert_eq!(route.method(), V2Method::Post);
    assert_eq!(route.feature(), KnownApiFeature::PeerRemove);
    assert!(KNOWN_API_FEATURES.contains(&KnownApiFeature::PeerRemove));
    assert!(route.accepts_principal(&Principal::Peer {
        peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("fixture peer id"),
    }));
    assert!(!route.accepts_principal(&Principal::Machine {
        machine_id: machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
    }));
}

#[test]
fn peer_remove_contract_serializes_identity_and_names_self_repair() {
    let peer_id = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("fixture peer id");
    let request = PeerRemoveRequest {
        peer_name: "operator-laptop".to_owned(),
        peer_id: Some(peer_id.clone()),
    };
    assert_eq!(
        serde_json::to_value(&request).expect("request JSON"),
        json!({"peer_name":"operator-laptop","peer_id":peer_id.as_str()})
    );
    let refusal = PeerRemoveRefusal::SelfRemoval {
        peer_name: "operator-laptop".to_owned(),
        peer_id: peer_id.clone(),
    };
    assert_eq!(
        refusal.to_string(),
        format!(
            "peer operator-laptop ({peer_id}) is the authenticated caller; run `ployz peer rm operator-laptop --id {peer_id} --target <context>` from another accepted operator context"
        )
    );
}

#[test]
fn peer_remove_selection_requires_an_unambiguous_accepted_roster_row() {
    let first = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("first peer id");
    let second = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAZ").expect("second peer id");
    let request = PeerRemoveRequest {
        peer_name: "operator-laptop".to_owned(),
        peer_id: None,
    };
    assert_eq!(
        select_peer_removal(
            &request,
            [
                (second.clone(), "operator-laptop".to_owned()),
                (first.clone(), "operator-laptop".to_owned()),
            ],
        ),
        Err(PeerRemoveRefusal::Ambiguous {
            peer_name: "operator-laptop".to_owned(),
            peer_ids: vec![first.clone(), second],
        })
    );
    let request = PeerRemoveRequest {
        peer_name: "operator-laptop".to_owned(),
        peer_id: Some(first.clone()),
    };
    assert_eq!(
        select_peer_removal(&request, [(first.clone(), "operator-laptop".to_owned())]),
        Ok(first)
    );
}

fn request_json(url: &str) -> serde_json::Value {
    serde_json::json!({
        "version": "v0.1.0-alpha.7",
        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "url": url,
    })
}

#[test]
fn machine_upgrade_route_is_post_advertised_and_peer_only() {
    let route = V2Route::parse(MACHINE_UPGRADE_ROUTE).expect("machine upgrade route");

    assert_eq!(route, V2Route::MachineUpgrade);
    assert_eq!(route.path(), MACHINE_UPGRADE_ROUTE);
    assert_eq!(route.method(), V2Method::Post);
    assert_eq!(route.feature(), KnownApiFeature::MachineUpgrade);
    assert!(KNOWN_API_FEATURES.contains(&KnownApiFeature::MachineUpgrade));
    assert!(route.accepts_principal(&Principal::Peer {
        peer_id: PeerId::generate(),
    }));
    assert!(!route.accepts_principal(&Principal::Machine {
        machine_id: MachineRowId::generate(),
    }));
    assert!(!route.accepts_principal(&Principal::ApiToken {
        token_id: TokenId::generate(),
    }));
    assert_eq!(V2Route::parse("/machines/upgrade/next"), None);
}

#[test]
fn machine_upgrade_request_accepts_only_host_addressed_https_urls() {
    let request = request_json("https://releases.example.test/ployzd?signature=abc");
    let decoded: MachineUpgradeRequest =
        serde_json::from_value(request.clone()).expect("valid upgrade request");

    assert_eq!(
        decoded.url.as_str(),
        "https://releases.example.test/ployzd?signature=abc"
    );
    assert_eq!(
        serde_json::to_value(decoded).expect("request serializes"),
        request
    );

    for url in [
        "http://releases.example.test/ployzd",
        "/var/lib/ployz/ployzd",
        "https:///",
        "ployzd",
    ] {
        assert!(
            serde_json::from_value::<MachineUpgradeRequest>(request_json(url)).is_err(),
            "{url:?} must not be accepted as an upgrade URL"
        );
    }

    let mut unknown_field = request_json("https://releases.example.test/ployzd");
    unknown_field
        .as_object_mut()
        .expect("request object")
        .insert("install_path".to_owned(), serde_json::json!("/tmp/ployzd"));
    assert!(serde_json::from_value::<MachineUpgradeRequest>(unknown_field).is_err());
}

#[test]
fn machine_upgrade_reply_and_refusals_have_strict_typed_wire_shapes() {
    let sha256 = InstallSha256Digest::try_new("a".repeat(64)).expect("sha256");
    let reply = MachineUpgradeReply {
        version: InstallArtifactVersion::try_new("v0.1.0-alpha.7").expect("version"),
        sha256: sha256.clone(),
    };
    assert_eq!(
        serde_json::to_value(reply).expect("reply serializes"),
        serde_json::json!({
            "version": "v0.1.0-alpha.7",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        })
    );

    let mismatch = MachineUpgradeRefusal::Sha256Mismatch {
        expected: sha256,
        got: InstallSha256Digest::try_new("b".repeat(64)).expect("sha256"),
    };
    assert_eq!(
        serde_json::to_value(mismatch).expect("refusal serializes"),
        serde_json::json!({
            "kind": "sha256_mismatch",
            "expected": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "got": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        })
    );

    let unsupported = MachineUpgradeRefusal::UnsupportedSupervisor {
        supervisor: MachineUpgradeSupervisor::OpenRc,
    };
    assert_eq!(
        serde_json::to_value(unsupported).expect("refusal serializes"),
        serde_json::json!({"kind": "unsupported_supervisor", "supervisor": "open_rc"})
    );
}
