//! Current SDK evidence admission and complete per-Service operation outcomes.

#![expect(
    clippy::indexing_slicing,
    reason = "Test JSON fixtures assert public response fields."
)]

use ployz_core::config::config_request;
use serde_json::{Value, json};

fn operation(id: &str) -> Value {
    json!({"type":"remove_container", "machine_id":"a".repeat(32), "container_id":id.repeat(64)})
}

#[test]
fn current_outcome_confirms_only_services_with_all_operations_completed() {
    let api = operation("a");
    let worker = operation("b");
    let replica = operation("c");
    let preview = |operations: Vec<(&str, Value)>| {
        json!({
            "project_name":"production", "operations":operations.into_iter().enumerate().map(|(index, (service, operation))|
                json!({"index":index,"machine_id":"a".repeat(32),"service_name":service,"operation":operation,"status":{"type":"pending"}})
            ).collect::<Vec<_>>(), "warnings":[]
        })
    };
    let project = |preview: Value, outcome: Value| {
        config_request(json!({
            "operation":"project_runtime_outcome", "preview":preview,
            "value":{"version":1,"outcome":outcome}
        }))
    };
    let partial = json!({"type":"failed","completed":[api.clone()],
        "failed":{"type":"operation","operation":worker.clone(),"error":{"type":"cancelled"}},"unexecuted":[]});
    let result = project(
        preview(vec![("api", api.clone()), ("worker", worker.clone())]),
        partial.clone(),
    )
    .unwrap();
    assert_eq!(result["confirmedServices"], json!(["api"]));
    let mut incomplete = partial;
    incomplete["unexecuted"] = json!([replica.clone()]);
    assert_eq!(
        project(
            preview(vec![
                ("api", api.clone()),
                ("worker", worker),
                ("api", replica)
            ]),
            incomplete
        )
        .unwrap()["confirmedServices"],
        json!([])
    );
    let success = json!({"type":"success","completed":[api.clone()]});
    assert_eq!(
        project(preview(vec![("api", api)]), success).unwrap()["summary"],
        json!({"type":"success","completed":1})
    );
    let mut unknown_field = json!({"type":"success","completed":[operation("a")]});
    unknown_field["completed"][0]["unexpected"] = json!("must-not-be-echoed");
    let error = project(preview(vec![("api", operation("a"))]), unknown_field).unwrap_err();
    assert!(!error.to_string().contains("must-not-be-echoed"));
    assert!(
        project(
            preview(vec![("api", operation("a"))]),
            json!({"type":"success","completed":[]})
        )
        .is_err()
    );
    let preflight = json!({"type":"failed","completed":[],
        "failed":{"type":"operation","operation":operation("b"),"error":{"type":"cancelled"}},"unexecuted":[operation("a")]});
    assert_eq!(
        project(
            preview(vec![("api", operation("a")), ("worker", operation("b"))]),
            preflight
        )
        .unwrap()["confirmedServices"],
        json!([])
    );
    let invalid = config_request(
        json!({"operation":"project_runtime_outcome","preview":preview(vec![]),"value":{"version":2,"outcome":{"type":"success","completed":[]}}}),
    );
    assert!(invalid.is_err());
}
