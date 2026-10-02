#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! The comparison engine a Sync runs: the rows between two Environments over what
//! they last shared, and landing the picked ones. A new Branch compares the same way,
//! over no base.

use ployz_core::config::{
    ConfigError, branch_changes, parse_environment_intent, redact_environment_intent,
};
use serde_json::{Value, json};

const API: &str = "a0000000-0000-4000-8000-000000000001";
const WEB: &str = "a0000000-0000-4000-8000-000000000002";
const WORKER: &str = "a0000000-0000-4000-8000-000000000003";
const CACHE: &str = "a0000000-0000-4000-8000-000000000004";
const DATA: &str = "a0000000-0000-4000-8000-000000000005";

fn id(seed: u32, n: u32) -> String {
    format!("{seed:08x}-0000-4000-8000-{n:012x}")
}

fn variable(seed: u32, n: u32, key: &str, value: Value, fingerprint: &str) -> Value {
    json!({"id": id(seed, n), "key": key, "description": null, "exported": false,
           "valueFingerprint": fingerprint, "value": value})
}

fn literal(value: &str) -> Value {
    json!({"kind": "literal", "value": value})
}

fn secret(ciphertext: &str) -> Value {
    json!({"kind": "secret", "encryptedValue": {"version": 1, "iv": "iv", "tag": "tag", "ciphertext": ciphertext}})
}

fn service(seed: u32, n: u32, lineage: &str, slug: &str, source: Value) -> Value {
    json!({"id": id(seed, n), "lineageId": lineage, "slug": slug, "variables": [], "volumeAttachments": [],
           "config": {"version": 2, "privateDns": slug, "source": source, "preDeployCommand": null,
                      "startCommand": null, "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped"}})
}

/// The Parent: api (image, credential, domain, secret, mount), web (git), worker, cache and a Volume.
fn parent() -> Value {
    let seed = 0xb000_0000;
    let mut api = service(
        seed,
        1,
        API,
        "api",
        json!({"version": 1, "type": "image", "image": "api:1",
               "credentials": {"type": "configured", "credentialId": id(seed, 40)}}),
    );
    api["config"]["routes"] =
        json!([{"id": id(seed, 20), "hostname": "api.example.com", "targetPort": null}]);
    api["config"]["managedHostnames"] = json!([{"prefix": "api", "targetPort": null}]);
    api["variables"] = json!([
        variable(seed, 10, "PLAIN", literal("a"), "fp-plain-a"),
        variable(seed, 11, "TOKEN", secret("parent-cipher"), "fp-token"),
        variable(
            seed,
            12,
            "WORKER_URL",
            json!({"kind": "template", "parts": [
            {"kind": "ref", "owner": {"scope": "service", "lineageId": WORKER}, "key": "URL"}]}),
            "fp-url"
        ),
    ]);
    api["volumeAttachments"] = json!([{"volumeResourceId": id(seed, 30), "mountPath": "/data"}]);
    let web = service(
        seed,
        2,
        WEB,
        "web",
        json!({"version": 2, "type": "git", "repository": "acme/web", "repositoryId": 1,
               "access": {"type": "public"}, "rootDir": "/", "branch": {"type": "connected", "name": "main"}}),
    );
    let image = |n, lineage, slug| {
        service(
            seed,
            n,
            lineage,
            slug,
            json!({"version": 1, "type": "image", "image": format!("{slug}:1"), "credentials": {"type": "none"}}),
        )
    };
    json!({"version": 1, "environmentSlug": "production",
           "services": [api, web, image(3, WORKER, "worker"), image(4, CACHE, "cache")],
           "volumes": [{"resourceId": id(seed, 30), "resourceLineageId": DATA, "name": "data", "storage": {"kind": "docker"}}]})
}

/// Give every node, variable, route and Volume a fresh id, keeping lineage.
fn reid(mut env: Value, seed: u32) -> Value {
    let mut n = 0x100;
    let mut fresh = || {
        n += 1;
        id(seed, n)
    };
    let volumes: Vec<(String, String)> = env["volumes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .map(|v| {
            let new = fresh();
            let old = std::mem::replace(&mut v["resourceId"], json!(new));
            (old.as_str().unwrap().to_owned(), new)
        })
        .collect();
    for service in env["services"].as_array_mut().unwrap() {
        service["id"] = json!(fresh());
        for v in service["variables"].as_array_mut().unwrap() {
            v["id"] = json!(fresh());
        }
        for route in service["config"]
            .get_mut("routes")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            route["id"] = json!(fresh());
        }
        for a in service["volumeAttachments"].as_array_mut().unwrap() {
            let old = a["volumeResourceId"].as_str().unwrap();
            a["volumeResourceId"] = json!(volumes.iter().find(|(o, _)| o == old).unwrap().1);
        }
    }
    env
}

/// An Own Copy of api, web and the Volume, using worker live and leaving cache out.
fn branch() -> Value {
    let mut env = reid(parent(), 0xc000_0000);
    env["environmentSlug"] = json!("pr-7");
    let services = env["services"].as_array_mut().unwrap();
    services.retain(|s| s["lineageId"] != WORKER && s["lineageId"] != CACHE);
    let api = &mut services[0];
    api["config"]["routes"] = json!([]);
    api["config"]["managedHostnames"] = json!([{"prefix": "api-pr-7", "targetPort": null}]);
    api["config"]["source"]["credentials"]["credentialId"] = json!(id(0xc000_0000, 40));
    env
}

fn svc<'a>(env: &'a mut Value, lineage: &str) -> &'a mut Value {
    env["services"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|s| s["lineageId"] == lineage)
        .unwrap()
}

fn var<'a>(env: &'a mut Value, lineage: &str, key: &str) -> &'a mut Value {
    svc(env, lineage)["variables"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|v| v["key"] == key)
        .unwrap()
}

fn request(base: Option<&Value>, from: &Value, into: &Value, extra: &Value) -> Value {
    let mut value = json!({"base": base, "from": from, "into": into, "provided": [],
                           "hostnames": {"from": "-pr-7", "into": ""}, "fromKept": false});
    for (k, v) in extra.as_object().unwrap() {
        value[k] = v.clone();
    }
    value
}

/// Core's branch changes for a JSON input, as JSON.
fn run(value: Value) -> Result<Value, ConfigError> {
    branch_changes(serde_json::from_value(value).unwrap())
        .map(|changes| serde_json::to_value(changes).unwrap())
}

fn changes(base: Option<&Value>, from: &Value, into: &Value, extra: &Value) -> Value {
    run(request(base, from, into, extra)).unwrap()
}

/// Rows as `key role why/conflict` lines for compact table assertions.
fn summary(result: &Value) -> Vec<String> {
    result["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let detail = if row["role"] == "move" {
                format!(
                    "conflict={} default={}",
                    row["conflict"],
                    row["choice"]["default"].as_str().unwrap_or("-")
                )
            } else {
                row["why"].as_str().unwrap().to_owned()
            };
            format!(
                "{} {} {detail}",
                row["key"].as_str().unwrap(),
                row["role"].as_str().unwrap()
            )
        })
        .collect()
}

fn row<'a>(result: &'a Value, key: &str) -> &'a Value {
    result["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == key)
        .unwrap_or_else(|| panic!("no row {key}"))
}

#[test]
fn own_copy_with_fresh_ids_has_only_meant_to_differ_rows() {
    let result = changes(Some(&parent()), &branch(), &parent(), &json!({}));
    assert_eq!(
        summary(&result),
        [
            format!("{API}:routes differ custom_domain"),
            format!("{WORKER}:node differ live"),
            format!("{CACHE}:node differ left_out"),
            format!("{DATA}:data differ data"),
        ]
    );
    assert_eq!(
        row(&result, &format!("{API}:routes"))["into"],
        json!(["api.example.com"])
    );
    let parsed = serde_json::to_value(parse_environment_intent(parent()).unwrap()).unwrap();
    assert_eq!(result["next"], parsed);
    assert_eq!(result["base"], parsed);
}

#[test]
fn sync_conflict_and_differ_rows_follow_the_one_rule() {
    let base = parent();
    let mut from = branch();
    let mut into = parent();
    let api = svc(&mut from, API);
    api["config"]["startCommand"] = json!("from-start");
    api["config"]["preDeployCommand"] = json!("from-migrate");
    api["config"]["replicas"] = json!(3);
    api["config"]["managedHostnames"] = json!([{"prefix": "admin-pr-7", "targetPort": null}]);
    api["volumeAttachments"][0]["mountPath"] = json!("/moved");
    svc(&mut from, WEB)["config"]["source"]["branch"]["name"] = json!("feature");
    svc(&mut into, API)["config"]["startCommand"] = json!("into-start");
    // `into`'s own change that `from` never touched does not move back.
    svc(&mut into, API)["config"]["memLimit"] = json!(512.0);
    let result = changes(Some(&base), &from, &into, &json!({}));
    let rows = summary(&result);
    for expected in [
        format!("{API}:startCommand move conflict=true default=-"),
        format!("{API}:preDeployCommand move conflict=false default=-"),
        format!("{API}:mounts.{DATA} move conflict=false default=-"),
        format!("{API}:replicas differ sizing"),
        format!("{API}:memLimit differ sizing"),
        format!("{API}:managedHostnames differ generated_address"),
        format!("{WEB}:source.branch differ git_branch"),
    ] {
        assert!(
            rows.contains(&expected),
            "{expected} missing from {rows:#?}"
        );
    }
    let conflict = row(&result, &format!("{API}:startCommand"));
    assert_eq!(
        (&conflict["base"], &conflict["into"], &conflict["from"]),
        (&Value::Null, &json!("into-start"), &json!("from-start"))
    );
    assert!(conflict.get("choice").is_none());
    assert!(
        row(&result, &format!("{API}:replicas"))
            .get("conflict")
            .is_none()
    );
}

#[test]
fn removals_and_equal_secrets_produce_no_rows() {
    let base = parent();
    let mut from = branch();
    let into = parent();
    let api = svc(&mut from, API);
    api["variables"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v["key"] != "PLAIN");
    api["volumeAttachments"] = json!([]);
    api["config"]["source"]["credentials"] = json!({"type": "none"});
    // Same secret value under a new id and new ciphertext.
    var(&mut from, API, "TOKEN")["value"] = secret("branch-cipher");
    let rows = summary(&changes(Some(&base), &from, &into, &json!({})));
    assert!(rows.iter().all(|r| !r.contains(":variables.")), "{rows:#?}");
    assert!(rows.iter().all(|r| !r.contains(":mounts.")), "{rows:#?}");
    assert!(rows.iter().all(|r| !r.contains("credentials")), "{rows:#?}");

    // Credentials compare by presence: adding one where `into` has none moves.
    let mut into = parent();
    svc(&mut into, API)["config"]["source"]["credentials"] = json!({"type": "none"});
    let rows = summary(&changes(
        Some(&base),
        &branch(),
        &into,
        &json!({"base": null}),
    ));
    assert!(rows.contains(&format!(
        "{API}:source.credentials move conflict=false default=-"
    )));
}

#[test]
fn a_secret_never_changes_where_it_is_and_arrives_without_a_value() {
    let base = parent();
    let mut from = branch();
    // `from` rotates TOKEN, which `into` has, and adds NEW_SECRET, which it lacks.
    var(&mut from, API, "TOKEN")["value"] = secret("rotated-cipher");
    var(&mut from, API, "TOKEN")["valueFingerprint"] = json!("fp-rotated");
    svc(&mut from, API)["variables"]
        .as_array_mut()
        .unwrap()
        .push(variable(
            0xc000_0000,
            1,
            "NEW_SECRET",
            secret("branch-cipher"),
            "fp-new",
        ));
    let result = changes(Some(&base), &from, &parent(), &json!({}));
    let rows = summary(&result);
    assert!(!rows.iter().any(|r| r.contains("TOKEN")), "{rows:#?}");
    let new_secret = format!("{API}:variables.NEW_SECRET");
    assert_eq!(
        row(&result, &new_secret)["from"],
        json!({"fingerprint": "fp-new", "kind": "secret"})
    );

    // Picked without a value, it lands without one; TOKEN keeps `into`'s value.
    let pick = json!({"picks": [{"key": new_secret, "choice": {"option": "new"}}]});
    let landed = changes(Some(&base), &from, &parent(), &pick);
    let mut next = landed["next"].clone();
    assert!(!next.to_string().contains("branch-cipher"), "{next:#}");
    assert_eq!(
        var(&mut next, API, "TOKEN")["value"]["encryptedValue"]["ciphertext"],
        "parent-cipher"
    );
    let arrived = var(&mut next, API, "NEW_SECRET").clone();
    assert_eq!(arrived["value"], json!({"kind": "secret_without_value"}));
    assert_eq!(arrived["valueFingerprint"], "");

    // Now the receiver has it: rotating it in `from` again offers nothing.
    var(&mut from, API, "NEW_SECRET")["valueFingerprint"] = json!("fp-new-2");
    let again = summary(&changes(Some(&landed["base"]), &from, &next, &json!({})));
    assert!(!again.iter().any(|r| r.contains("SECRET")), "{again:#?}");

    // A secret without a value, sent on, arrives as a secret without a value.
    let mut sender = next.clone();
    sender["environmentSlug"] = json!("pr-7");
    let mut receiver = parent();
    let onward = changes(Some(&parent()), &sender, &receiver, &pick);
    assert_eq!(row(&onward, &new_secret)["from"], json!({"kind": "secret"}));
    let mut onward_next = onward["next"].clone();
    assert_eq!(
        var(&mut onward_next, API, "NEW_SECRET")["value"],
        json!({"kind": "secret_without_value"})
    );

    // Only a secret without a value has no fingerprint.
    var(&mut receiver, API, "TOKEN")["valueFingerprint"] = json!("");
    assert!(parse_environment_intent(receiver).is_err());
    var(&mut sender, API, "NEW_SECRET")["valueFingerprint"] = json!("fp");
    assert!(parse_environment_intent(sender).is_err());
}

#[test]
fn variable_choices_default_by_kind() {
    let base = parent();
    let mut from = branch();
    let mut into = parent();
    var(&mut from, API, "PLAIN")["value"] = literal("b");
    var(&mut from, API, "PLAIN")["valueFingerprint"] = json!("fp-plain-b");
    let api = svc(&mut from, API);
    let vars = api["variables"].as_array_mut().unwrap();
    vars.push(variable(0xc000_0000, 1, "NEW_PLAIN", literal("x"), "fp-x"));
    vars.push(variable(
        0xc000_0000,
        2,
        "NEW_SECRET",
        secret("c"),
        "fp-new",
    ));
    // A secret both sides have, changed on `from`, never moves.
    var(&mut from, API, "TOKEN")["valueFingerprint"] = json!("fp-token-changed");
    let _ = &mut into;

    let result = changes(Some(&base), &from, &into, &json!({"parent": parent()}));
    assert!(!summary(&result).iter().any(|r| r.contains("TOKEN")));
    let choice = |key: &str| row(&result, &format!("{API}:variables.{key}"))["choice"].clone();
    assert_eq!(
        choice("PLAIN"),
        json!({"default": "from", "options": ["from", "parent", "new", "leave_out"], "secret": false})
    );
    assert_eq!(
        choice("NEW_PLAIN"),
        json!({"default": "from", "options": ["from", "new", "leave_out"], "secret": false})
    );
    assert_eq!(choice("NEW_SECRET")["default"], "new");
    assert_eq!(choice("NEW_SECRET")["secret"], true);
    let kept = changes(Some(&base), &from, &into, &json!({"fromKept": true}));
    assert_eq!(
        row(&kept, &format!("{API}:variables.NEW_SECRET"))["choice"]["default"],
        "leave_out"
    );
    // Without a Parent input, the Parent's value is never offered.
    assert_eq!(
        row(&kept, &format!("{API}:variables.PLAIN"))["choice"]["options"],
        json!(["from", "new", "leave_out"])
    );
}

#[test]
fn a_sync_into_a_branch_marks_its_live_nodes_live_and_base_only_nodes_left_out() {
    let result = changes(
        Some(&parent()),
        &parent(),
        &branch(),
        &json!({"provided": [WORKER], "hostnames": {"from": "", "into": "-pr-7"}}),
    );
    assert_eq!(
        summary(&result),
        [
            format!("{API}:routes differ custom_domain"),
            format!("{WORKER}:node differ live"),
            format!("{CACHE}:node differ left_out"),
            format!("{DATA}:data differ data"),
        ]
    );
}

#[test]
fn rows_are_redacted_and_review_ignores_ids() {
    let mut from = branch();
    var(&mut from, API, "PLAIN")["value"] = literal("b");
    var(&mut from, API, "PLAIN")["valueFingerprint"] = json!("fp-plain-b");
    let result = changes(Some(&parent()), &from, &parent(), &json!({}));
    let rows = result["rows"].to_string();
    assert!(
        !rows.contains("cipher") && !rows.contains("0000000028"),
        "{rows}"
    );
    let redact = |env: &Value| {
        serde_json::to_value(redact_environment_intent(
            parse_environment_intent(env.clone()).unwrap(),
        ))
        .unwrap()
    };
    let redacted = changes(
        Some(&redact(&parent())),
        &redact(&from),
        &redact(&parent()),
        &json!({}),
    );
    assert_eq!(redacted["rows"], result["rows"]);
    assert_eq!(redacted["review"], result["review"]);

    let review = result["review"].as_str().unwrap();
    for seed in [0xb000_0000_u32, 0xc000_0000, 0xb100_0000, 0xc100_0000] {
        assert!(
            !review.contains(&format!("{seed:08x}")),
            "review holds an id: {review}"
        );
    }
    let reided = changes(
        Some(&reid(parent(), 0xd000_0000)),
        &reid(from.clone(), 0xd100_0000),
        &reid(parent(), 0xd200_0000),
        &json!({}),
    );
    assert_eq!(reided["review"], result["review"]);
    var(&mut from, API, "PLAIN")["value"] = literal("c");
    let changed = changes(Some(&parent()), &from, &parent(), &json!({}));
    assert_ne!(changed["review"], result["review"]);
}

#[test]
fn invalid_configuration_is_refused() {
    let mut from = branch();
    from["environmentSlug"] = json!("");
    let error = run(request(None, &from, &parent(), &json!({}))).unwrap_err();
    assert_eq!(error.path, "environment");
}

/// A review is a canonical string, stable across ids.
#[test]
fn contract_review_string() {
    let env = |start: &str, n: u32| {
        json!({"version": 1, "environmentSlug": "e", "volumes": [], "services": [{
            "id": id(n, 1), "lineageId": API, "slug": "api", "variables": [], "volumeAttachments": [],
            "config": {"version": 2, "privateDns": "api", "preDeployCommand": null, "startCommand": start,
                       "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped",
                       "source": {"version": 1, "type": "image", "image": "api:1", "credentials": {"type": "none"}}}}]})
    };
    let result = run(json!({
        "base": env("a", 1), "from": env("b", 2), "into": env("a", 3), "provided": [],
        "hostnames": {"from": "", "into": ""}, "fromKept": false}))
    .unwrap();
    assert_eq!(
        result["review"],
        r#"{"picks":[],"rows":[{"base":"a","conflict":false,"from":"b","into":"a","key":"a0000000-0000-4000-8000-000000000001:startCommand","role":"move"}]}"#
    );
    let picked = run(json!({
        "base": env("a", 1), "from": env("b", 2), "into": env("a", 3), "provided": [],
        "hostnames": {"from": "", "into": ""}, "fromKept": false,
        "picks": [{"key": format!("{API}:startCommand")}]}))
    .unwrap();
    assert_eq!(picked["next"]["services"][0]["config"]["startCommand"], "b");
    assert_eq!(
        picked["review"],
        r#"{"picks":[{"choice":null,"key":"a0000000-0000-4000-8000-000000000001:startCommand"}],"rows":[{"base":"a","conflict":false,"from":"b","into":"a","key":"a0000000-0000-4000-8000-000000000001:startCommand","role":"move"}]}"#
    );
}

/// `from` with setting, mount, Volume and variable changes for the pick table.
fn changed_branch() -> Value {
    let mut from = branch();
    let api = svc(&mut from, API);
    api["config"]["startCommand"] = json!("from-start");
    api["config"]["preDeployCommand"] = json!("from-migrate");
    api["volumeAttachments"][0]["mountPath"] = json!("/moved");
    let vars = api["variables"].as_array_mut().unwrap();
    vars.push(variable(0xc000_0000, 1, "NEW_PLAIN", literal("x"), "fp-x"));
    vars.push(variable(
        0xc000_0000,
        2,
        "NEW_SECRET",
        secret("branch-cipher"),
        "fp-new",
    ));
    vars.push(variable(
        0xc000_0000,
        3,
        "UNSUPPLIED",
        secret("branch-cipher"),
        "fp-uns",
    ));
    var(&mut from, API, "PLAIN")["value"] = literal("b");
    var(&mut from, API, "PLAIN")["valueFingerprint"] = json!("fp-plain-b");
    from["volumes"][0]["name"] = json!("renamed");
    from
}

fn with_picks(
    from: &Value,
    into: &Value,
    picks: Value,
) -> Result<Value, ployz_core::config::ConfigError> {
    let mut parent_input = parent();
    var(&mut parent_input, API, "PLAIN")["value"] = literal("p");
    var(&mut parent_input, API, "PLAIN")["valueFingerprint"] = json!("fp-plain-p");
    run(request(
        Some(&parent()),
        from,
        into,
        &json!({"parent": parent_input, "picks": picks}),
    ))
}

#[test]
fn volume_storage_moves_as_one_setting_with_its_exact_limit() {
    let base = parent();
    let mut from = branch();
    let storage = json!({"kind":"provisioned","maximumBytes":7000000000_i64});
    from["volumes"][0]["storage"] = storage.clone();
    let key = format!("{DATA}:storage");
    let result = changes(Some(&base), &from, &base, &json!({"picks":[{"key":key}]}));
    assert_eq!(row(&result, &key)["role"], "move");
    assert_eq!(result["next"]["volumes"][0]["storage"], storage);
    assert_eq!(
        result["next"]["volumes"][0]["resourceId"],
        base["volumes"][0]["resourceId"]
    );
}

fn table_picks() -> Value {
    let server = json!({"kind": "secret", "encryptedValue": {"version": 1, "iv": "iv", "tag": "tag", "ciphertext": "server-cipher"}});
    json!([
        {"key": format!("{API}:startCommand")},
        {"key": format!("{API}:mounts.{DATA}")},
        {"key": format!("{API}:variables.PLAIN"), "choice": {"option": "parent"}},
        {"key": format!("{API}:variables.NEW_PLAIN"), "choice": {"option": "from"}},
        {"key": format!("{API}:variables.NEW_SECRET"), "choice": {"option": "new",
         "value": {"value": server, "valueFingerprint": "fp-server"}}},
        {"key": format!("{API}:variables.UNSUPPLIED"), "choice": {"option": "new"}},
    ])
}

#[test]
fn a_sync_lands_its_picks_and_advances_the_base_by_exactly_them() {
    let from = changed_branch();
    let mut into = parent();
    svc(&mut into, API)["config"]["startCommand"] = json!("into-start");
    let result = with_picks(&from, &into, table_picks()).unwrap();
    let next = &result["next"];
    let api = next["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["lineageId"] == API)
        .unwrap();
    let value = |key: &str| {
        api["variables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["key"] == key)
            .cloned()
    };
    // A picked conflict overrides `into`; an unpicked setting stays.
    assert_eq!(api["config"]["startCommand"], "from-start");
    assert_eq!(api["config"]["preDeployCommand"], Value::Null);
    assert_eq!(api["volumeAttachments"][0]["mountPath"], "/moved");
    assert_eq!(
        api["volumeAttachments"][0]["volumeResourceId"],
        into["volumes"][0]["resourceId"]
    );
    assert_eq!(next["volumes"][0]["name"], "data");
    assert_eq!(value("PLAIN").unwrap()["value"], literal("p"));
    let new_plain = value("NEW_PLAIN").unwrap();
    assert_eq!(new_plain["value"], literal("x"));
    assert_ne!(
        new_plain["id"],
        var(&mut from.clone(), API, "NEW_PLAIN")["id"]
    );
    let sealed = value("NEW_SECRET").unwrap();
    assert_eq!(
        sealed["value"]["encryptedValue"]["ciphertext"],
        "server-cipher"
    );
    assert_eq!(sealed["valueFingerprint"], "fp-server");
    // A secret picked `new` without a value lands without one.
    let unsupplied = value("UNSUPPLIED").unwrap();
    assert_eq!(unsupplied["value"], json!({"kind": "secret_without_value"}));
    assert_eq!(unsupplied["valueFingerprint"], "");
    // Nothing `into` had is lost.
    assert_eq!(
        value("TOKEN").unwrap()["value"]["encryptedValue"]["ciphertext"],
        "parent-cipher"
    );
    assert_eq!(next["services"].as_array().unwrap().len(), 4);

    let base = &result["base"];
    assert!(
        !base.to_string().contains("cipher"),
        "base holds sealed material"
    );
    let again = summary(&changes(Some(base), &from, next, &json!({})));
    for settled in [
        "startCommand",
        "mounts.",
        "variables.PLAIN",
        "variables.NEW_PLAIN",
        "variables.NEW_SECRET",
        "variables.UNSUPPLIED",
    ] {
        assert!(
            !again
                .iter()
                .any(|r| r.contains(&format!(":{settled}")) && r.contains(" move ")),
            "{settled} in {again:#?}"
        );
    }
    assert!(
        again.contains(&format!(
            "{API}:preDeployCommand move conflict=false default=-"
        )),
        "{again:#?}"
    );
    assert!(
        again.contains(&format!("{DATA}:name move conflict=false default=-")),
        "{again:#?}"
    );
}

#[test]
fn leaving_a_variable_out_keeps_into_own() {
    let from = changed_branch();
    let result = with_picks(
        &from,
        &parent(),
        json!([{"key": format!("{API}:variables.PLAIN"), "choice": {"option": "leave_out"}},
               {"key": format!("{API}:variables.NEW_PLAIN"), "choice": {"option": "leave_out"}}]),
    )
    .unwrap();
    let api = &result["next"]["services"][0];
    let keys: Vec<_> = api["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["PLAIN", "TOKEN", "WORKER_URL"]);
    assert_eq!(api["variables"][0]["value"], literal("a"));
}

#[test]
fn bad_picks_are_refused() {
    let from = changed_branch();
    for (picks, path) in [
        (json!([{"key": "nope"}]), "picks.key"),
        (json!([{"key": format!("{API}:routes")}]), "picks.key"),
        (
            json!([{"key": format!("{API}:startCommand"), "choice": {"option": "from"}}]),
            "picks.choice",
        ),
        (
            json!([{"key": format!("{API}:variables.PLAIN")}]),
            "picks.choice",
        ),
        (
            json!([{"key": format!("{API}:variables.NEW_PLAIN"), "choice": {"option": "parent"}}]),
            "picks.choice",
        ),
    ] {
        let error = with_picks(&from, &parent(), picks.clone()).unwrap_err();
        assert_eq!(error.path, path, "{picks}");
    }
}

#[test]
fn review_reflects_picks_but_never_new_values_or_ids() {
    let from = changed_branch();
    let review = |from: &Value, into: &Value, picks: Value| {
        with_picks(from, into, picks).unwrap()["review"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let picked = review(&from, &parent(), table_picks());
    assert!(
        !picked.contains("server-cipher") && !picked.contains("fp-server"),
        "{picked}"
    );
    assert!(picked.contains(r#""choice":"parent""#));
    assert_eq!(
        picked,
        review(
            &reid(from.clone(), 0xe000_0000),
            &reid(parent(), 0xe100_0000),
            table_picks()
        )
    );
    let mut other = table_picks();
    other[2]["choice"] = json!({"option": "from"});
    assert_ne!(picked, review(&from, &parent(), other));
    let compare_only = changes(Some(&parent()), &from, &parent(), &json!({}));
    assert_ne!(picked, compare_only["review"].as_str().unwrap());
}

const JOBS: &str = "a0000000-0000-4000-8000-000000000006";

fn empty(slug: &str) -> Value {
    json!({"version": 1, "environmentSlug": slug, "services": [], "volumes": []})
}

fn create(
    parent: &Value,
    picks: &[&str],
    into_suffix: &str,
) -> Result<Value, ployz_core::config::ConfigError> {
    let picks: Vec<_> = picks
        .iter()
        .map(|l| json!({"key": format!("{l}:node")}))
        .collect();
    run(json!({
        "base": null, "from": parent, "into": empty("pr-7"), "provided": [WORKER],
        "hostnames": {"from": "", "into": into_suffix}, "fromKept": false, "picks": picks}))
}

fn find<'a>(list: &'a Value, field: &str, lineage: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|n| n[field] == lineage)
        .unwrap()
}

#[test]
fn create_copies_picked_nodes_with_fresh_ids_and_the_branch_naming() {
    let parent = parent();
    let result = create(&parent, &[API, WEB, DATA], "-pr-7").unwrap();
    let next = &result["next"];
    let api = find(&next["services"], "lineageId", API);
    let volume = find(&next["volumes"], "resourceLineageId", DATA);
    let mut ids: Vec<_> = next["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].clone())
        .collect();
    ids.extend(
        api["variables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].clone()),
    );
    ids.push(volume["resourceId"].clone());
    let parent_text = parent.to_string();
    for id in &ids {
        assert!(
            !parent_text.contains(id.as_str().unwrap()),
            "{id} is not fresh"
        );
    }
    ids.sort_by_key(ToString::to_string);
    ids.dedup();
    assert_eq!(ids.len(), 6);
    assert_eq!(
        next["services"].as_array().unwrap().len(),
        2,
        "worker is live, cache left out"
    );
    assert_eq!(api["config"]["routes"], json!([]));
    assert_eq!(api["config"]["managedHostnames"][0]["prefix"], "api-pr-7");
    assert_eq!(
        api["config"]["source"]["credentials"]["credentialId"],
        api["id"]
    );
    assert_eq!(
        api["volumeAttachments"][0]["volumeResourceId"],
        volume["resourceId"]
    );
    let token = find(&api["variables"], "key", "TOKEN");
    assert_eq!(
        token["value"]["encryptedValue"]["ciphertext"],
        "parent-cipher"
    );
    let url = find(&api["variables"], "key", "WORKER_URL");
    assert_eq!(url["value"]["parts"][0]["owner"]["lineageId"], WORKER);

    let base = &result["base"];
    let lineages: Vec<_> = base["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lineageId"].as_str().unwrap())
        .collect();
    assert_eq!(lineages, [API, WEB, CACHE]);
    assert!(!base.to_string().contains("cipher"));

    let again = create(&parent, &[API, WEB, DATA], "-pr-7").unwrap();
    assert_eq!(again["review"], result["review"]);
    assert_ne!(again["next"]["services"][0]["id"], api["id"]);
}

#[test]
fn a_sync_introduces_a_new_branch_service_into_the_parent() {
    let mut from = branch();
    let mut jobs = service(
        0xc000_0000,
        50,
        JOBS,
        "jobs",
        json!({"version": 1, "type": "image", "image": "jobs:1", "credentials": {"type": "none"}}),
    );
    jobs["config"]["managedHostnames"] = json!([{"prefix": "jobs-pr-7", "targetPort": null}]);
    jobs["variables"] = json!([
        variable(0xc000_0000, 51, "MODE", literal("test"), "fp-mode"),
        variable(0xc000_0000, 52, "KEY", secret("test-cipher"), "fp-key"),
    ]);
    from["services"].as_array_mut().unwrap().push(jobs);
    let compared = changes(Some(&parent()), &from, &parent(), &json!({}));
    assert_eq!(row(&compared, &format!("{JOBS}:node"))["role"], "move");
    assert_eq!(
        row(&compared, &format!("{JOBS}:variables.KEY"))["choice"]["default"],
        "new"
    );
    let result = with_picks(&from, &parent(), json!([{"key": format!("{JOBS}:node")}])).unwrap();
    let jobs = find(&result["next"]["services"], "lineageId", JOBS);
    assert_ne!(jobs["id"], id(0xc000_0000, 50));
    assert_eq!(jobs["config"]["managedHostnames"][0]["prefix"], "jobs");
    let keys: Vec<_> = jobs["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["MODE", "KEY"]);
    assert_eq!(
        var(&mut result["next"].clone(), JOBS, "KEY")["value"],
        json!({"kind": "secret_without_value"}),
        "the new secret lands without its value"
    );
    // Base records the node and both variables: nothing of jobs is offered again.
    let again = changes(Some(&result["base"]), &from, &result["next"], &json!({}));
    assert!(
        !summary(&again).iter().any(|r| r.contains(JOBS)),
        "{:#?}",
        summary(&again)
    );
    // MODE landed by default, so base records it: the receiver's own later edit is not a
    // stale move or conflict.
    let mut edited = result["next"].clone();
    var(&mut edited, JOBS, "MODE")["value"] = literal("prod");
    var(&mut edited, JOBS, "MODE")["valueFingerprint"] = json!("fp-prod");
    let again = changes(Some(&result["base"]), &from, &edited, &json!({}));
    assert!(
        !summary(&again).iter().any(|r| r.contains("MODE")),
        "{:#?}",
        summary(&again)
    );

    // A name `into` already uses is refused.
    svc(&mut from, JOBS)["slug"] = json!("worker");
    let error = with_picks(&from, &parent(), json!([{"key": format!("{JOBS}:node")}])).unwrap_err();
    assert_eq!(error.path, "picks.key");
}

#[test]
fn never_synced_rows_differ_and_never_land() {
    let mut from = branch();
    let api = svc(&mut from, API);
    api["config"]["startCommand"] = json!("from-start");
    api["config"]["healthcheck"] = json!({"type": "http", "path": "/up", "timeoutSeconds": 30});
    var(&mut from, API, "PLAIN")["value"] = literal("b");
    var(&mut from, API, "PLAIN")["valueFingerprint"] = json!("fp-plain-b");
    let mut jobs = service(
        0xc000_0000,
        50,
        JOBS,
        "jobs",
        json!({"version": 1, "type": "image", "image": "jobs:1", "credentials": {"type": "none"}}),
    );
    jobs["variables"] = json!([
        variable(0xc000_0000, 51, "MODE", literal("test"), "fp-mode"),
        variable(0xc000_0000, 52, "LEVEL", literal("debug"), "fp-level"),
    ]);
    from["services"].as_array_mut().unwrap().push(jobs);
    // A mark covers the rows under it: `healthcheck` covers its path and timeout.
    let marked = json!([
        format!("{API}:variables.PLAIN"),
        format!("{API}:healthcheck"),
        format!("{JOBS}:variables.MODE"),
    ]);
    let compared = changes(
        Some(&parent()),
        &from,
        &parent(),
        &json!({"neverSynced": marked}),
    );
    let rows = summary(&compared);
    for expected in [
        format!("{API}:startCommand move conflict=false default=-"),
        format!("{API}:variables.PLAIN differ never_synced"),
        format!("{API}:healthcheck.path differ never_synced"),
        format!("{API}:healthcheck.timeoutSeconds differ never_synced"),
        format!("{JOBS}:node move conflict=false default=-"),
        format!("{JOBS}:variables.MODE differ never_synced"),
        format!("{JOBS}:variables.LEVEL move conflict=false default=from"),
    ] {
        assert!(
            rows.contains(&expected),
            "{expected} missing from {rows:#?}"
        );
    }

    // A marked row is never picked, and a node it arrives with lands without it.
    let picked = |picks: Value| {
        run(request(
            Some(&parent()),
            &from,
            &parent(),
            &json!({"neverSynced": marked, "picks": picks}),
        ))
    };
    let error = picked(json!([
        {"key": format!("{API}:variables.PLAIN"), "choice": {"option": "from"}}
    ]))
    .unwrap_err();
    assert_eq!(error.path, "picks.key");
    let result = picked(json!([{"key": format!("{JOBS}:node")}])).unwrap();
    let jobs = find(&result["next"]["services"], "lineageId", JOBS);
    assert_eq!(jobs["variables"].as_array().unwrap().len(), 1);
    assert_eq!(jobs["variables"][0]["key"], "LEVEL");
}

#[test]
fn a_sync_into_a_branch_introduces_parent_services_with_its_naming() {
    let mut from = parent();
    let mut mail = service(
        0xb000_0000,
        60,
        JOBS,
        "mail",
        json!({"version": 1, "type": "image", "image": "mail:1", "credentials": {"type": "none"}}),
    );
    mail["config"]["managedHostnames"] = json!([{"prefix": "mail", "targetPort": null}]);
    from["services"].as_array_mut().unwrap().push(mail);
    // As create returns it: the Parent without the nodes the Branch uses live.
    let mut base = parent();
    base["services"]
        .as_array_mut()
        .unwrap()
        .retain(|s| s["lineageId"] != WORKER);
    let sync = |provided: Value, picks: Value| {
        run(request(
            Some(&base),
            &from,
            &branch(),
            &json!({
            "provided": provided, "hostnames": {"from": "", "into": "-pr-7"}, "picks": picks}),
        ))
    };
    let result = sync(json!([WORKER]), json!([{"key": format!("{JOBS}:node")}])).unwrap();
    let mail = find(&result["next"]["services"], "lineageId", JOBS);
    assert_eq!(mail["config"]["managedHostnames"][0]["prefix"], "mail-pr-7");
    assert_eq!(row(&result, &format!("{CACHE}:node"))["why"], "left_out");
    // Dropping a lineage from `provided` offers it as an introduction.
    let result = sync(json!([]), json!([{"key": format!("{WORKER}:node")}])).unwrap();
    assert_eq!(row(&result, &format!("{WORKER}:node"))["role"], "move");
    find(&result["next"]["services"], "lineageId", WORKER);
}

#[test]
fn bad_introductions_are_refused() {
    let parent = parent();
    assert_eq!(
        create(&parent, &[API], "-pr-7").unwrap_err().path,
        "picks.key",
        "mount without its Volume"
    );
    assert!(create(&parent, &[API, DATA], "-Bad_Label").is_err());
    assert!(create(&parent, &[API, DATA], "-pr-7").is_ok());
}

/// The SDK contract test creates through WebAssembly and expects this review.
#[test]
fn contract_create_review_string() {
    let parent = json!({"version": 1, "environmentSlug": "e", "volumes": [], "services": [{
        "id": id(1, 1), "lineageId": API, "slug": "api", "variables": [], "volumeAttachments": [],
        "config": {"version": 2, "privateDns": "api", "preDeployCommand": null, "startCommand": null,
                   "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped",
                   "source": {"version": 1, "type": "image", "image": "api:1", "credentials": {"type": "none"}}}}]});
    let result = run(json!({
        "base": null, "from": parent, "into": empty("pr-7"), "provided": [],
        "hostnames": {"from": "", "into": "-pr-7"}, "fromKept": false,
        "picks": [{"key": format!("{API}:node")}]}))
    .unwrap();
    assert_ne!(result["next"]["services"][0]["id"], id(1, 1));
    assert_eq!(
        result["review"],
        r#"{"picks":[{"choice":null,"key":"a0000000-0000-4000-8000-000000000001:node"}],"rows":[{"base":null,"conflict":false,"from":"api","into":null,"key":"a0000000-0000-4000-8000-000000000001:node","role":"move"}]}"#
    );
}

#[test]
fn new_values_must_be_supplied_and_secrets_sealed() {
    let from = changed_branch();
    let pick = |key: &str, choice: Value| json!([{"key": format!("{API}:variables.{key}"), "choice": choice}]);
    let plain = json!({"kind": "literal", "value": "typed"});
    for (picks, why) in [
        (
            pick("NEW_PLAIN", json!({"option": "new"})),
            "plain without a value",
        ),
        (
            pick(
                "NEW_SECRET",
                json!({"option": "new", "value": {"value": plain, "valueFingerprint": "fp"}}),
            ),
            "secret as plaintext",
        ),
        (
            pick(
                "NEW_SECRET",
                json!({"option": "new", "value": {"value": {"kind": "secret"}, "valueFingerprint": "fp"}}),
            ),
            "secret without sealed material",
        ),
    ] {
        assert_eq!(
            with_picks(&from, &parent(), picks).unwrap_err().path,
            "picks.value",
            "{why}"
        );
    }
    let result = with_picks(
        &from,
        &parent(),
        pick(
            "NEW_PLAIN",
            json!({"option": "new", "value": {"value": plain, "valueFingerprint": "fp-typed"}}),
        ),
    )
    .unwrap();
    let api = find(&result["next"]["services"], "lineageId", API);
    assert_eq!(find(&api["variables"], "key", "NEW_PLAIN")["value"], plain);
}

#[test]
fn repository_authority_moves_with_the_repository() {
    let mut from = branch();
    svc(&mut from, WEB)["config"]["source"]["access"] =
        json!({"type": "github-installation", "installationId": 9});
    svc(&mut from, WEB)["config"]["source"]["repositoryId"] = json!(2);
    let result = with_picks(
        &from,
        &parent(),
        json!([{"key": format!("{WEB}:source.repository")}]),
    )
    .unwrap();
    let web = find(&result["next"]["services"], "lineageId", WEB);
    assert_eq!(web["config"]["source"]["repositoryId"], 2);
    assert_eq!(web["config"]["source"]["access"]["installationId"], 9);
}

#[test]
fn a_private_address_clash_is_refused() {
    let mut from = branch();
    let mut jobs = service(
        0xc000_0000,
        50,
        JOBS,
        "jobs",
        json!({"version": 1, "type": "image", "image": "jobs:1", "credentials": {"type": "none"}}),
    );
    jobs["config"]["privateDns"] = json!("worker");
    from["services"].as_array_mut().unwrap().push(jobs);
    let error = with_picks(&from, &parent(), json!([{"key": format!("{JOBS}:node")}])).unwrap_err();
    assert_eq!(
        (error.path.as_str(), error.message.as_str()),
        ("picks.key", "Name or private address is already used")
    );
}

#[test]
fn empty_picks_apply_nothing_but_still_create_the_base() {
    let result = create(&parent(), &[], "-pr-7").unwrap();
    assert_eq!(result["next"]["services"], json!([]));
    let lineages: Vec<_> = result["base"]["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lineageId"].as_str().unwrap())
        .collect();
    assert_eq!(
        lineages,
        [API, WEB, CACHE],
        "the Parent minus what the Branch uses live"
    );
    // Compare-only (no picks) leaves base as given.
    let compared = run(json!({
        "base": null, "from": parent(), "into": empty("pr-7"), "provided": [WORKER],
        "hostnames": {"from": "", "into": "-pr-7"}, "fromKept": false}))
    .unwrap();
    assert_eq!(compared["base"], Value::Null);
    assert_eq!(
        compared["review"].as_str().unwrap(),
        result["review"].as_str().unwrap()
    );
}
