use ployz_core::{RpcErrorCode, RpcRequestBody};
use serde_json::{Value, json};

use super::volume_switch_path;

fn body(command: &str, payload: Value) -> RpcRequestBody {
    serde_json::from_value(json!({ "command": command, "payload": payload }))
        .unwrap_or_else(|error| panic!("{command} parses: {error}"))
}

fn resolved_spec() -> Value {
    let requested: ployz_core::RequestedServiceSpec = serde_json::from_value(json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "missing" },
    }))
    .unwrap_or_else(|error| panic!("the spec parses: {error}"));
    let resolved = requested
        .to_resolved(
            ployz_core::ServiceId::random(),
            ployz_core::ResolvedUpdateConfig {
                order: ployz_core::UpdateOrder::StartFirst,
                monitor_millis: None,
            },
        )
        .unwrap_or_else(|error| panic!("the spec resolves: {error}"));
    serde_json::to_value(resolved).unwrap_or_else(|error| panic!("the spec serializes: {error}"))
}

fn switch() -> Value {
    json!({ "lease": 3, "pos": { "seq": 4, "round": 0, "sub": 0 } })
}

#[test]
fn every_volume_run_verb_is_sent_on_its_own_path() {
    let leased = json!({ "switch": switch(), "name": "data" });
    let handed = json!({ "switch": switch(), "name": "data", "guid": "7" });
    let source = json!({ "switch": switch(), "name": "data", "container_id": "a".repeat(64) });
    let service = json!({ "switch": switch(), "name": "data", "namespace": "app-prod", "resolved_spec": resolved_spec() });
    let cases = [
        (
            "inspect_volume_copy",
            json!({ "name": "data" }),
            "InspectVolumeCopy",
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
        ("forget_snapshots", leased.clone(), "ForgetSnapshots"),
        ("forget_lease", leased.clone(), "ForgetLease"),
        ("withdraw", source.clone(), "Withdraw"),
        ("freeze", source.clone(), "Freeze"),
        ("thaw", source, "Thaw"),
        ("hand_over", handed.clone(), "HandOver"),
        ("accept_hand_off", handed, "AcceptHandOff"),
        ("promote", service.clone(), "Promote"),
        (
            "start_handed_container",
            service.clone(),
            "StartHandedContainer",
        ),
        ("close", leased.clone(), "Close"),
        ("clear_final", leased, "ClearFinal"),
        ("restore", service, "Restore"),
    ];
    for (command, payload, route) in cases {
        let path = volume_switch_path(&body(command, payload))
            .unwrap_or_else(|error| panic!("{command} is a Volume run verb: {error:?}"));
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
