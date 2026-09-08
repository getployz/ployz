#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::config_request;
use serde_json::{Value, json};

fn service(replicas: u8) -> Value {
    json!({"version":2,"name":"API","privateDns":"api","source":{"version":1,"type":"image","image":"nginx:stable","autoUpdate":{"type":"off"},"credentials":{"type":"none"}},"preDeployCommand":null,"startCommand":null,"healthcheck":{"type":"none"},"restartPolicy":"unless-stopped","replicas":replicas})
}

fn input(
    node_type: &str,
    introduced: &Value,
    working: Value,
    saved: Value,
    applied: Value,
    runtime: Value,
) -> Value {
    let node = json!({"type":node_type,"id":"stable-id"});
    json!({
        "working":{"token":"working","nodes":[{"node":node,"config":working}]},
        "saved":{"kind":"saved_revision","savedStateSnapshotId":"saved-id","token":"saved","nodes":[{"node":node,"config":saved}]},
        "applied":{"token":"applied","nodes":[{"node":node,"config":applied}]},
        "nodeIntroductions":{"token":"introduced","nodes":[{"node":node,"config":introduced}]},
        "runtimeObserved":{"token":"observed","nodes":[{"node":node,"config":runtime}]}
    })
}

fn project(value: Value) -> Value {
    config_request(json!({"operation":"project_changes","value":value})).unwrap()
}

#[test]
fn variable_group_attachment_changes_have_one_restorable_authored_owner() {
    let id = |n| format!("00000000-0000-4000-8000-{n:012}");
    let group = |n, name, value| {
        json!({"resourceId":id(n),"resourceLineageId":id(n + 1),
        "variableGroupId":id(n + 2),"variableGroupLineageId":id(n + 3),
        "slug":name,"name":name,"variables":[{
            "id":id(n + 4),"key":"KEY","description":null,"exported":true,
            "valueFingerprint":"plain","value":{"kind":"literal","value":value}
        }]})
    };
    let original = json!({"version":1,"environmentSlug":"production",
        "services":[{"id":id(1),"lineageId":id(2),"slug":"api","config":service(1),
            "variables":[{"id":id(3),"key":"KEY","description":null,"exported":false,
                "valueFingerprint":"plain","value":{"kind":"literal","value":"local"}}],
            "variableGroupAttachments":[],"volumeAttachments":[],
            "encryptedRegistryUsername":null,"encryptedRegistrySecret":null}],
        "variableGroups":[group(10,"first","one"),group(20,"second","two")],"volumes":[]});
    let attachment = |group, order| json!({"variableGroupId":id(group),"sortOrder":order});
    let project_nodes = |intent: &Value| {
        let compiled = config_request(json!({"operation":"compile_environment",
            "environment_id":id(4),"value":intent}))
        .unwrap();
        compiled["nodeSnapshots"].as_array().unwrap().iter().map(|node|
            json!({"node":{"type":node["nodeType"],"id":node["nodeId"]},"config":node["config"]})
        ).collect::<Vec<_>>()
    };
    for (before, after) in [
        (json!([]), json!([attachment(12, 0)])),
        (json!([attachment(12, 0)]), json!([])),
        (
            json!([attachment(12, 0), attachment(22, 1)]),
            json!([attachment(22, 0), attachment(12, 1)]),
        ),
    ] {
        let mut baseline = original.clone();
        baseline["services"][0]["variableGroupAttachments"] = before.clone();
        let mut current = original.clone();
        current["services"][0]["variableGroupAttachments"] = after.clone();
        let baseline_nodes = project_nodes(&baseline);
        let current_nodes = project_nodes(&current);
        assert_ne!(
            baseline_nodes[0]["config"]["env"],
            current_nodes[0]["config"]["env"]
        );
        for slice in ["unsaved", "pending"] {
            let saved = if slice == "unsaved" {
                &baseline_nodes
            } else {
                &current_nodes
            };
            let review = project(json!({
                "working":{"token":"working","nodes":current_nodes},
                "saved":{"kind":"saved_revision","savedStateSnapshotId":id(5),"token":"saved","nodes":saved},
                "applied":{"token":"applied","nodes":baseline_nodes},
                "nodeIntroductions":{"token":"none","nodes":[]},"runtimeObserved":null
            }));
            assert_eq!(
                review[slice]["totalCount"], 1,
                "{slice}: {before} -> {after}"
            );
            let row = &review[slice]["groups"][0]["settings"][0];
            assert_eq!(
                row["owner"],
                json!({"node":{"type":"service","id":id(1)},"setting":"variableGroupAttachments"})
            );
            assert_eq!(
                row["discardPlan"]["config"]["variableGroupAttachments"],
                before
            );
            assert_eq!(
                row["discardPlan"]["target"],
                if slice == "unsaved" {
                    "working"
                } else {
                    "saved"
                }
            );
        }
        let restored = config_request(json!({"operation":"restore_environment",
            "current":current,"baseline":baseline,"node_type":"service","node_id":id(1),
            "path":"variableGroupAttachments"}))
        .unwrap();
        assert_eq!(restored["services"][0]["variableGroupAttachments"], before);
        assert_eq!(project_nodes(&restored), baseline_nodes);
    }
}

#[test]
fn presence_and_setting_ownership_preserve_each_comparison_role() {
    for (node_type, original, changed, setting, resettable) in [
        ("service", service(1), service(2), "replicas", true),
        (
            "variable_group",
            json!({"version":1,"name":"Shared","variables":[]}),
            json!({"version":1,"name":"Renamed","variables":[]}),
            "name",
            false,
        ),
        (
            "volume",
            json!({"version":2,"name":"Data"}),
            json!({"version":2,"name":"Renamed"}),
            "name",
            false,
        ),
    ] {
        for slice in ["unsaved", "pending", "drift"] {
            for (before, after, lifecycle) in [
                (false, false, None),
                (false, true, Some("create")),
                (true, false, Some("delete")),
                (true, true, None),
            ] {
                let before = if before {
                    original.clone()
                } else {
                    Value::Null
                };
                let after = if after { original.clone() } else { Value::Null };
                let value = match slice {
                    "unsaved" => input(
                        node_type,
                        &original,
                        after,
                        before,
                        Value::Null,
                        Value::Null,
                    ),
                    "pending" => input(
                        node_type,
                        &original,
                        after.clone(),
                        after,
                        before.clone(),
                        before,
                    ),
                    _ => input(
                        node_type,
                        &original,
                        before.clone(),
                        before.clone(),
                        before,
                        after,
                    ),
                };
                let result = project(value);
                let groups = result[slice]["groups"].as_array().unwrap();
                if let Some(lifecycle) = lifecycle {
                    assert_eq!(groups.len(), 1, "{node_type} {slice}");
                    assert_eq!(groups[0]["lifecycle"]["kind"], lifecycle);
                    assert_eq!(!groups[0]["discardPlan"].is_null(), slice != "drift");
                } else {
                    assert!(groups.is_empty(), "{node_type} {slice}");
                }
            }
        }
        let result = project(input(
            node_type,
            &original,
            changed.clone(),
            Value::Null,
            Value::Null,
            Value::Null,
        ));
        let row = &result["unsaved"]["groups"][0]["settings"][0];
        assert_eq!(row["owner"]["setting"], setting);
        assert_eq!(row["baselineSource"]["role"], "node_introduction");
        assert_eq!(!row["discardPlan"].is_null(), resettable);
        let result = project(input(
            node_type,
            &original,
            changed.clone(),
            Value::Null,
            original.clone(),
            original.clone(),
        ));
        assert!(
            result["unsaved"]["groups"][0]["settings"]
                .as_array()
                .unwrap()
                .is_empty(),
            "Applied is not an unsaved baseline"
        );
        let result = project(input(
            node_type,
            &original,
            changed.clone(),
            changed,
            original.clone(),
            original.clone(),
        ));
        assert_eq!(
            result["pending"]["groups"][0]["settings"][0]["baselineSource"]["role"],
            "applied"
        );
        assert_eq!(
            result["pending"]["groups"][0]["settings"][0]["id"],
            row["id"]
        );
    }
}

#[test]
fn derived_secrets_unknown_runtime_and_partial_runtime_evidence_stay_honest() {
    let mut current = service(1);
    current["env"] = json!({"TOKEN":{"kind":"secret","fingerprint":"private-fingerprint","source":{"kind":"variable_group","resourceId":"group","resourceName":"Shared","variableGroupId":"group-id","key":"TOKEN"}}});
    let mut candidate = input(
        "service",
        &service(1),
        current,
        service(1),
        service(1),
        service(1),
    );
    candidate["runtimeObserved"] = Value::Null;
    let result = project(candidate.clone());
    assert_eq!(result["unsaved"]["totalCount"], 0);
    assert_eq!(result["drift"]["totalCount"], 0);
    assert_eq!(
        result["drift"]["provenance"]["target"]["token"],
        "runtime:unavailable"
    );
    assert!(!result.to_string().contains("private-fingerprint"));
    candidate["runtimeObserved"] = candidate["applied"].clone();
    candidate["runtimeObservations"] = json!({"token":"fresh-machine","settings":[{"node":{"type":"service","id":"stable-id"},"setting":"image.observed","label":"Image","appliedValue":"a","observedValue":"b"}],"presence":[{"node":{"type":"service","id":"other-id"},"applied":"present","observed":"absent"}]});
    let result = project(candidate.clone());
    assert_eq!(result["drift"]["totalCount"], 2);
    assert_eq!(
        result["drift"]["discardPlans"],
        json!({"nodes":[],"settings":[]})
    );
    assert_eq!(result, project(candidate));
}

#[test]
fn review_setting_kinds_reject_unknown_wire_values() {
    use ployz_core::config::ReviewSettingKind;

    for kind in ["add", "update", "remove", "drift"] {
        assert!(serde_json::from_value::<ReviewSettingKind>(json!(kind)).is_ok());
    }
    assert!(serde_json::from_value::<ReviewSettingKind>(json!("unknown")).is_err());
}
