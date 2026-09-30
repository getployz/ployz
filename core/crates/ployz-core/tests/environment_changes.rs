#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::project_environment_changes;
use serde_json::{Value, json};

fn service(replicas: u8) -> Value {
    json!({"version":2,"privateDns":"api","source":{"version":1,"type":"image","image":"nginx:stable","credentials":{"type":"none"}},"preDeployCommand":null,"startCommand":null,"healthcheck":{"type":"none"},"restartPolicy":"unless-stopped","replicas":replicas})
}

fn input(
    node_type: &str,
    introduced: &Value,
    working: Value,
    applied: Value,
    submitted: Option<Value>,
) -> Value {
    let node = json!({"type":node_type,"id":"stable-id"});
    json!({
        "working":{"token":"working","nodes":[{"node":node,"config":working}]},
        "applied":{"token":"applied","nodes":[{"node":node,"config":applied}]},
        "nodeIntroductions":{"token":"introduced","nodes":[{"node":node,"config":introduced}]},
        "submitted":submitted.map(|config| json!({"token":"submitted","nodes":[{"node":node,"config":config}]}))
    })
}

fn project(value: Value) -> Value {
    serde_json::to_value(
        project_environment_changes(serde_json::from_value(value).unwrap()).unwrap(),
    )
    .unwrap()
}

#[test]
fn lifecycle_and_settings_compare_against_submitted_or_applied_state() {
    for (node_type, original, changed, setting, resettable) in [
        ("service", service(1), service(2), "replicas", true),
        (
            "volume",
            json!({"version":2,"name":"Data","storage":{"kind":"local"}}),
            json!({"version":2,"name":"Renamed","storage":{"kind":"local"}}),
            "name",
            true,
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
            original.clone(),
            Some(changed),
        ));
        assert_eq!(result["groups"], json!([]));
        assert_eq!(result["totalCount"], 0);
    }
}

#[test]
fn secret_changes_are_listed_once_without_their_fingerprints() {
    let secret = |fingerprint: &str| {
        let mut config = service(1);
        config["env"] = json!({"TOKEN":{"kind":"secret","fingerprint":fingerprint}});
        config
    };
    let result = project(input(
        "service",
        &Value::Null,
        secret("private-after"),
        secret("private-before"),
        None,
    ));
    let paths: Vec<_> = result["groups"][0]["settings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["path"].clone())
        .collect();
    assert_eq!(paths, vec![json!("env.TOKEN")]);
    assert!(!result.to_string().contains("private-"));
}
