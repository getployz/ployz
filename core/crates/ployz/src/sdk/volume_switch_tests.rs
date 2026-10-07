use ployz_core::{RpcErrorCode, RpcRequestBody};
use serde_json::{Value, json};

use super::volume_switch_path;

fn body(command: &str, payload: Value) -> RpcRequestBody {
    serde_json::from_value(json!({ "command": command, "payload": payload }))
        .unwrap_or_else(|error| panic!("{command} parses: {error}"))
}

fn switch() -> Value {
    json!({ "lease": 3, "pos": { "seq": 4, "round": 0, "sub": 0 }, "not_after_unix_seconds": 0 })
}

#[test]
fn every_volume_run_verb_is_sent_on_its_own_path() {
    let leased = json!({ "switch": switch(), "name": "data" });
    let cases = [
        (
            "inspect_volume_copy",
            json!({ "name": "data" }),
            "InspectVolumeCopy",
        ),
        (
            "adopt_lease",
            json!({ "lease": 3, "not_after_unix_seconds": 0, "name": "data" }),
            "AdoptLease",
        ),
        (
            "declare_mirror",
            json!({ "switch": switch(), "name": "data", "refquota_bytes": 1024 }),
            "DeclareMirror",
        ),
        ("begin_round", leased.clone(), "BeginRound"),
        (
            "commit_snapshots",
            json!({ "switch": switch(), "name": "data", "mirror_newest": "7" }),
            "CommitSnapshots",
        ),
        ("warm_snapshot", leased.clone(), "WarmSnapshot"),
        (
            "start_receive",
            json!({ "switch": switch(), "name": "data", "from": "fd00::1", "base": null, "target": "w-3-0", "resume_token": null }),
            "StartReceive",
        ),
        (
            "inspect_receive",
            json!({ "name": "data", "round": 0 }),
            "InspectReceive",
        ),
        ("prune_mirror", leased.clone(), "PruneMirror"),
        ("destroy_mirror", leased.clone(), "DestroyMirror"),
        ("forget_snapshots", leased, "ForgetSnapshots"),
    ];
    for (command, payload, route) in cases {
        let path = volume_switch_path(&body(command, payload))
            .unwrap_or_else(|error| panic!("{command} is a Volume run verb: {error:?}"));
        assert!(path.ends_with(&format!("/{route}")), "{command} -> {path}");
    }
}

#[test]
fn other_machine_verbs_are_refused_before_anything_is_sent() {
    let handed = json!({ "switch": switch(), "name": "data", "guid": "7" });
    let source = json!({ "switch": switch(), "name": "data", "container_id": "a".repeat(64) });
    let cases = [
        ("hand_over", handed.clone()),
        ("accept_hand_off", handed),
        ("freeze", source.clone()),
        ("withdraw", source),
        ("close", json!({ "switch": switch(), "name": "data" })),
        ("clear_final", json!({ "switch": switch(), "name": "data" })),
        ("list_volumes", json!({})),
        ("inspect_storage", json!({})),
    ];
    for (command, payload) in cases {
        let error = volume_switch_path(&body(command, payload))
            .expect_err(&format!("{command} is not a Volume run verb"));
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{command}");
        assert!(error.message.contains(command), "{}", error.message);
    }
}
