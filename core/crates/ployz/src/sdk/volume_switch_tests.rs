use ployz_core::{RpcErrorCode, RpcRequestBody, RpcResponseBody};
use serde_json::{Value, json};

use super::{volume_switch_path, volume_switch_payload};

fn body(command: &str, payload: Value) -> RpcRequestBody {
    serde_json::from_value(json!({ "command": command, "payload": payload }))
        .unwrap_or_else(|error| panic!("{command} parses: {error}"))
}

fn switch() -> Value {
    json!({ "lease": 3, "pos": { "seq": 4, "round": 0, "sub": 0 } })
}

#[test]
fn every_volume_copy_verb_is_sent_on_its_own_path() {
    let cases = [
        (
            "inspect_volume_copy",
            json!({ "name": "data" }),
            "InspectVolumeCopy",
        ),
        (
            "seed_volume",
            json!({ "switch": switch(), "name": "data", "into": "data2", "guid": "7" }),
            "SeedVolume",
        ),
        (
            "set_volume_copy",
            json!({ "name": "data", "expect": { "kind": "none" }, "want": { "kind": "forget" } }),
            "SetVolumeCopy",
        ),
    ];
    for (command, payload, route) in cases {
        let path = volume_switch_path(&body(command, payload))
            .unwrap_or_else(|error| panic!("{command} is a Volume copy verb: {error:?}"));
        assert!(path.ends_with(&format!("/{route}")), "{command} -> {path}");
    }
}

#[test]
fn other_machine_verbs_are_refused_before_anything_is_sent() {
    let cases = [("list_volumes", json!({})), ("inspect_storage", json!({}))];
    for (command, payload) in cases {
        let error = volume_switch_path(&body(command, payload))
            .expect_err(&format!("{command} is not a Volume run verb"));
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{command}");
        assert!(error.message.contains(command), "{}", error.message);
    }
}

#[test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
fn a_set_volume_copy_reply_and_its_refusal_pass_through_as_the_machine_sent_them() {
    let copy = json!({
        "kind": "root",
        "writer": { "phase": "idle" },
        "readonly": false,
        "newest": { "name": "f-1", "guid": "13", "created_unix_seconds": 1700000000 },
        "epoch": 2,
    });
    let current: ployz_core::VolumeCopy = serde_json::from_value(copy.clone())
        .unwrap_or_else(|error| panic!("the copy parses: {error}"));
    let reply = RpcResponseBody::SetVolumeCopyReply(ployz_core::SetVolumeCopyReply {
        copy: Some(current.clone()),
    });
    assert_eq!(
        volume_switch_payload("set_volume_copy", reply).unwrap_or_else(|error| panic!("{error:?}")),
        json!({ "copy": copy })
    );

    let refused = ployz_core::SetVolumeCopyRefused {
        reason: ployz_core::CopyRefusal::Behind,
        copy: Some(current),
    }
    .rpc_error("Volume data is not at the wanted snapshot".to_owned());
    let error = volume_switch_payload("set_volume_copy", RpcResponseBody::Error(refused.clone()))
        .expect_err("a refusal is the Machine's error");
    assert_eq!(error, refused);
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(error.details["reason"], json!("behind"));
    assert_eq!(error.details["copy"], copy);
}
