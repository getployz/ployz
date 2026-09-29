#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use serde_json::{Value, json};

fn producer(owner_id: &str, lineage: &str, key: &str, value: Value) -> Value {
    json!({"ownerScope":"service","ownerId":owner_id,"ownerLineageId":lineage,"key":key,"value":value})
}
fn literal(value: &str) -> Value {
    json!({"kind":"literal","value":value})
}
fn service_ref(lineage: &str, key: &str) -> Value {
    json!({"kind":"ref","owner":{"scope":"service","lineageId":lineage},"key":key})
}
fn sealed() -> Value {
    json!({"kind":"secret","encryptedValue":{"version":1,"iv":"iv","tag":"tag","ciphertext":"sealed-token"}})
}

fn owner_producers() -> Vec<Value> {
    vec![
        producer(
            "owner-api",
            "api",
            "PLOYZ_PRIVATE_DOMAIN",
            literal("api.internal"),
        ),
        producer(
            "owner-api",
            "api",
            "DATABASE_URL",
            json!({"kind":"template","parts":[
                {"kind":"text","value":"postgres://"},
                service_ref("db", "PLOYZ_PRIVATE_DOMAIN"),
                {"kind":"text","value":":5432"}
            ]}),
        ),
        producer("owner-api", "api", "TOKEN", sealed()),
        producer(
            "owner-db",
            "db",
            "PLOYZ_PRIVATE_DOMAIN",
            literal("db.internal"),
        ),
        // The caller adds the owner's public address, as for the owner's own deploys.
        producer(
            "owner-api",
            "api",
            "PLOYZ_PUBLIC_DOMAIN",
            literal("api.example.dev"),
        ),
    ]
}

/// Core's live values for a JSON input, as JSON.
fn live(input: Value) -> Value {
    serde_json::to_value(ployz_core::config::live_values(
        serde_json::from_value(input).unwrap(),
    ))
    .unwrap()
}

fn live_values(namespace: &str) -> Value {
    live(json!({
        "owner": {"namespace": namespace, "producers": owner_producers()},
        "lineages": [
            {"lineageId": "api", "keys": ["PLOYZ_PRIVATE_DOMAIN", "DATABASE_URL", "TOKEN", "PLOYZ_PUBLIC_DOMAIN", "UNSET"]},
            {"lineageId": "gone", "keys": ["URL"]}
        ]
    }))
}

/// A client decrypts sealed values at apply time and hands resolver producers to core.
fn resolver_producer(frozen: &Value) -> Value {
    let value = &frozen["value"];
    let value = if value["kind"] == "secret" {
        json!({"kind":"secret","value":value["encryptedValue"]["ciphertext"]})
    } else {
        value.clone()
    };
    json!({
        "ownerId": frozen["ownerId"],
        "owner": {"scope":"service","lineageId":frozen["ownerLineageId"]},
        "key": frozen["key"],
        "value": value
    })
}

fn resolve(producers: &[Value], part: Value) -> Value {
    let producers: Vec<Value> = producers.iter().map(resolver_producer).collect();
    let input = serde_json::from_value(
        json!({"parts": [part], "selfOwnerId": "branch-web", "producers": producers}),
    )
    .unwrap();
    serde_json::to_value(ployz_core::config::resolve_variables(&input)).unwrap()
}

#[test]
fn branch_resolves_live_values_in_the_owner_scope() {
    let live = live_values("shop");
    // The Branch has its own copy of the db lineage the Live api references.
    let mut producers = vec![producer(
        "branch-db",
        "db",
        "PLOYZ_PRIVATE_DOMAIN",
        literal("db.internal"),
    )];
    producers.extend(live["producers"].as_array().unwrap().iter().cloned());

    let value =
        |lineage: &str, key: &str| resolve(&producers, service_ref(lineage, key))["value"].clone();
    assert_eq!(value("api", "PLOYZ_PRIVATE_DOMAIN"), "api.shop.internal");
    assert_eq!(
        value("api", "DATABASE_URL"),
        "postgres://db.shop.internal:5432"
    );
    assert_eq!(value("api", "PLOYZ_PUBLIC_DOMAIN"), "api.example.dev");
    assert_eq!(value("db", "PLOYZ_PRIVATE_DOMAIN"), "db.internal");
    let token = resolve(&producers, service_ref("api", "TOKEN"));
    assert_eq!(token["value"], "sealed-token");
    assert_eq!(token["secret"], true);

    assert_eq!(
        live["missing"],
        json!([{"lineageId":"api","key":"UNSET"},{"lineageId":"gone","key":"URL"}])
    );
    let token = live["producers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "TOKEN")
        .unwrap();
    assert_eq!(token["value"], sealed());
}

#[test]
fn an_address_the_owner_uses_live_keeps_its_namespace() {
    let live = live(json!({
        "owner": {"namespace": "shop-staging", "producers": [
            producer("staging-api", "api", "PLOYZ_PRIVATE_DOMAIN", literal("api.internal")),
            producer("prod-db", "db", "PLOYZ_PRIVATE_DOMAIN", literal("db.shop-production.internal")),
        ]},
        "lineages": [{"lineageId": "api", "keys": ["PLOYZ_PRIVATE_DOMAIN"]}]
    }));
    let domains: Vec<&Value> = live["producers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| &p["value"]["value"])
        .collect();
    assert_eq!(
        domains,
        [
            &json!("api.shop-staging.internal"),
            &json!("db.shop-production.internal")
        ]
    );
}
