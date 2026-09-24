#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::{ResolveVariablesInput, resolve_variables};
use serde_json::{Value, json};

fn reference(key: &str) -> Value {
    json!({"kind":"ref","owner":{"scope":"self"},"key":key})
}
fn run(parts: Value, producers: Value) -> Value {
    let input: ResolveVariablesInput =
        serde_json::from_value(json!({"parts": parts,"selfOwnerId":"db","producers": producers}))
            .unwrap();
    serde_json::to_value(resolve_variables(&input)).unwrap()
}
fn producer(key: &str, value: Value) -> Value {
    json!({"ownerId":"db","owner":{"scope":"service","lineageId":"db-lineage"},"key":key,"value":value})
}

#[test]
fn resolves_literals_references_secrets_missing_and_diamonds() {
    assert_eq!(
        run(json!([{"kind":"text","value":"hello"}]), json!([]))["value"],
        "hello"
    );
    let producers = json!([
        producer(
            "PASSWORD",
            json!({"kind":"secret","value":"private-sentinel"})
        ),
        producer(
            "URL",
            json!({"kind":"template","parts":[{"kind":"text","value":"postgres:"},reference("PASSWORD")]})
        )
    ]);
    let result = run(
        json!([reference("URL"), reference("URL")]),
        producers.clone(),
    );
    assert_eq!(
        result["value"],
        "postgres:private-sentinelpostgres:private-sentinel"
    );
    assert_eq!(result["secret"], true);
    assert_eq!(result["warnings"], json!([]));
    let cross = run(
        json!([{"kind":"ref","owner":{"scope":"service","lineageId":"db-lineage"},"key":"URL"}]),
        producers,
    );
    assert_eq!(cross["value"], "postgres:private-sentinel");
    assert_eq!(cross["secret"], true);
    let missing = run(json!([reference("MISSING")]), json!([]));
    assert_eq!(missing["value"], "");
    assert_eq!(
        missing["warnings"],
        json!([{"kind":"missing","ownerId":"db","key":"MISSING"}])
    );
}

#[test]
fn cycle_reports_only_owner_keys_even_after_a_secret_was_resolved() {
    for chain in [vec!["B", "A"], vec!["B", "C", "A"]] {
        let mut producers = vec![producer(
            "TOKEN",
            json!({"kind":"secret","value":"private-sentinel"}),
        )];
        let mut previous = "A";
        for key in chain {
            producers.push(producer(
                previous,
                json!({"kind":"template","parts":[reference(key)]}),
            ));
            previous = key;
        }
        let result = run(
            json!([reference("TOKEN"), reference("A")]),
            json!(producers),
        );
        assert_eq!(result["status"], "cycle");
        assert!(!result.to_string().contains("private-sentinel"));
        assert!(result["path"].as_array().unwrap().contains(&json!("db::A")));
    }
}
