use serde_json::json;

use super::*;
use crate::ids::PeerName;

fn machine_id(value: &str) -> MachineName {
    MachineName::try_new(value).expect("fixture machine id")
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
        peer_id: PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("fixture peer id"),
    }));
    assert!(!route.accepts_principal(&Principal::Machine {
        machine_id: machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
    }));
    assert!(!route.accepts_principal(&Principal::ApiToken {
        token_id: TokenName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("fixture token id"),
    }));
}

#[test]
fn machine_remove_contract_uses_only_the_canonical_name() {
    let request = MachineRemoveRequest {
        machine_name: machine_name(),
    };
    assert_eq!(
        serde_json::to_value(&request).expect("request serializes"),
        json!({ "machine_name": "edge-a" })
    );
    let reply = MachineRemoveReply::Removed {
        machine_name: machine_name(),
    };
    assert_eq!(
        serde_json::to_value(reply).expect("reply serializes"),
        json!({ "kind": "removed", "machine_name": "edge-a" })
    );
    let refusal = MachineRemoveRefusal::NotFound {
        machine_name: machine_name(),
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
fn machine_remove_selection_uses_the_name_as_the_row_key() {
    let request = MachineRemoveRequest {
        machine_name: machine_name(),
    };
    assert_eq!(
        select_machine_removal(
            &request,
            [
                (machine_id("edge-b"), machine_id("edge-b")),
                (machine_name(), machine_name()),
            ],
        ),
        Ok(machine_name())
    );
    assert_eq!(
        select_machine_removal(&request, [(machine_id("edge-b"), machine_name())]),
        Err(MachineRemoveRefusal::NotFound {
            machine_name: machine_name(),
        })
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
        peer_id: PeerName::try_new("operator").expect("peer name"),
    }));
    assert!(!route.accepts_principal(&Principal::Machine {
        machine_id: MachineName::try_new("edge-a").expect("machine name"),
    }));
    assert!(!route.accepts_principal(&Principal::ApiToken {
        token_id: TokenName::try_new("bootstrap").expect("token name"),
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
