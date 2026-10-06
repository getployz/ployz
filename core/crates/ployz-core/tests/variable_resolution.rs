#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::{
    ResolveVariablesInput, VariableProducer, parse_variable_template, resolve_config_file,
    resolve_variables,
};
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

/// Resolve Config file `name` written as display text, where `db.KEY` names `db`'s producers.
fn file(name: &str, text: &str, producers: Value) -> ployz_core::config::ResolvedConfigFile {
    let parsed = parse_variable_template(text, |service| {
        (service == "db").then(|| "db-lineage".to_owned())
    });
    let producers: Vec<VariableProducer> = serde_json::from_value(producers).unwrap();
    resolve_config_file(name, &parsed.parts, &producers).unwrap()
}

fn literal(value: &str) -> Value {
    json!({"kind":"literal","value":value})
}

#[test]
fn a_config_file_resolves_secrets_and_keeps_escaped_references_literal() {
    let producers = json!([
        producer("HOST", literal("redis.internal")),
        producer(
            "PASSWORD",
            json!({"kind":"secret","value":"private-sentinel"})
        ),
    ]);
    let plain = file(
        "config.yml",
        "host: ${{ db.HOST }}\nraw: $${{ db.HOST }}\n",
        producers.clone(),
    );
    assert_eq!(plain.content, "host: redis.internal\nraw: ${{ db.HOST }}\n");
    assert!(!plain.secret);
    let secret = file("config.yml", "password: ${{ db.PASSWORD }}\n", producers);
    assert_eq!(secret.content, "password: private-sentinel\n");
    assert!(secret.secret);
}

#[test]
fn a_config_file_flags_each_value_that_may_break_its_syntax_once() {
    let producers = json!([
        producer("QUOTE", literal("it's")),
        producer("DOUBLE", literal("say \"hi\"")),
        producer("LINES", literal("a\nb")),
        producer("PLAIN", literal("plain")),
    ]);
    let fragile = |name: &str, text: &str| -> Vec<String> {
        file(name, text, producers.clone())
            .fragile
            .into_iter()
            .map(|reference| reference.key)
            .collect()
    };
    assert_eq!(
        fragile(
            "app.json",
            r#"{"a": "${{ db.DOUBLE }}", "b": "${{ db.DOUBLE }}", "c": "${{ db.QUOTE }}"}"#
        ),
        ["DOUBLE"]
    );
    assert_eq!(
        fragile("app.conf", "a = ${{ db.QUOTE }}\nb = '${{ db.LINES }}'"),
        ["LINES"]
    );
    assert_eq!(
        fragile(
            "app.conf",
            "a = ${{ db.QUOTE }} '${{ db.PLAIN }}' ${{ db.LINES }}"
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        fragile("APP.YAML", "a: ${{ db.LINES }}\nb: \"${{ db.PLAIN }}\""),
        ["LINES"]
    );
}

#[test]
fn a_config_file_reports_the_cycle_it_reads() {
    let producers: Vec<VariableProducer> = serde_json::from_value(json!([
        producer("A", json!({"kind":"template","parts":[reference("B")]})),
        producer("B", json!({"kind":"template","parts":[reference("A")]})),
    ]))
    .unwrap();
    let parsed = parse_variable_template("${{ db.A }}", |_| Some("db-lineage".to_owned()));
    assert_eq!(
        resolve_config_file("x", &parsed.parts, &producers).unwrap_err(),
        ["db::A", "db::B", "db::A"]
    );
}
