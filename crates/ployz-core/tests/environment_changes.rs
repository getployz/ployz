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
    submitted: Option<Value>,
) -> Value {
    let node = json!({"type":node_type,"id":"stable-id"});
    json!({
        "working":{"token":"working","nodes":[{"node":node,"config":working}]},
        "saved":{"token":"saved","nodes":[{"node":node,"config":saved}]},
        "applied":{"token":"applied","nodes":[{"node":node,"config":applied}]},
        "nodeIntroductions":{"token":"introduced","nodes":[{"node":node,"config":introduced}]},
        "submitted":submitted.map(|config| json!({"token":"submitted","nodes":[{"node":node,"config":config}]}))
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
        for saved in [&baseline_nodes, &current_nodes] {
            let review = project(json!({
                "working":{"token":"working","nodes":current_nodes},
                "saved":{"token":"saved","nodes":saved},
                "applied":{"token":"applied","nodes":baseline_nodes},
                "nodeIntroductions":{"token":"none","nodes":[]},"submitted":null
            }));
            assert_eq!(review["totalCount"], 1, "{before} -> {after}");
            assert_eq!(review["canSave"], saved != &current_nodes);
            let group = &review["groups"][0];
            assert_eq!(group["node"], json!({"type":"service","id":id(1)}));
            let row = &group["settings"][0];
            assert_eq!(row["path"], "variableGroupAttachments");
            assert_eq!(row["before"], before);
            assert_eq!(row["after"], after);
            assert_eq!(row["canRestore"], true);
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
fn lifecycle_and_settings_compare_against_submitted_or_applied_state() {
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
        for submitted in [false, true] {
            for (before, after, lifecycle) in [
                (Value::Null, Value::Null, None),
                (Value::Null, original.clone(), Some("create")),
                (original.clone(), Value::Null, Some("delete")),
                (original.clone(), original.clone(), None),
                (original.clone(), changed.clone(), Some("update")),
            ] {
                let result = project(input(
                    node_type,
                    &original,
                    after.clone(),
                    after.clone(),
                    if submitted { after } else { before.clone() },
                    submitted.then_some(before),
                ));
                let groups = result["groups"].as_array().unwrap();
                if let Some(lifecycle) = lifecycle {
                    assert_eq!(groups.len(), 1, "{node_type}, submitted={submitted}");
                    assert_eq!(groups[0]["lifecycle"], lifecycle);
                    assert_eq!(result["totalCount"], 1);
                    if lifecycle == "update" {
                        assert_eq!(groups[0]["settings"][0]["path"], setting);
                        assert_eq!(groups[0]["settings"][0]["canRestore"], resettable);
                    }
                } else {
                    assert!(groups.is_empty(), "{node_type}, submitted={submitted}");
                    assert_eq!(result["totalCount"], 0);
                }
            }
        }
        let result = project(input(
            node_type,
            &original,
            changed.clone(),
            Value::Null,
            Value::Null,
            None,
        ));
        let row = &result["groups"][0]["settings"][0];
        assert_eq!(result["groups"][0]["lifecycle"], "create");
        assert_eq!(result["totalCount"], 2);
        assert_eq!(row["path"], setting);
        assert_eq!(row["canRestore"], resettable);
        let result = project(input(
            node_type,
            &original,
            changed.clone(),
            changed.clone(),
            original.clone(),
            Some(changed),
        ));
        assert_eq!(result["groups"], json!([]));
        assert_eq!(result["totalCount"], 0);
    }
}

#[test]
fn derived_variables_are_not_counted_and_secret_values_stay_redacted() {
    let mut current = service(1);
    current["env"] = json!({"TOKEN":{"kind":"secret","fingerprint":"private-fingerprint","source":{"kind":"variable_group","resourceId":"group","resourceName":"Shared","variableGroupId":"group-id","key":"TOKEN"}}});
    let mut candidate = input(
        "service",
        &service(1),
        current,
        service(1),
        service(1),
        None,
    );
    let result = project(candidate.clone());
    assert_eq!(result["totalCount"], 0);
    assert!(!result.to_string().contains("private-fingerprint"));
    candidate["working"]["nodes"][0]["config"]["env"]["TOKEN"] =
        json!({"kind":"secret","fingerprint":"private-fingerprint"});
    let result = project(candidate.clone());
    assert_eq!(result["totalCount"], 1);
    assert_eq!(result["groups"][0]["settings"][0]["path"], "env.TOKEN");
    assert!(!result.to_string().contains("private-fingerprint"));
    assert_eq!(result, project(candidate));
}
