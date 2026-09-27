#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::config_request;
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
           "volumes": [{"resourceId": id(seed, 30), "resourceLineageId": DATA, "name": "data"}]})
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
    json!({"operation": "branch_changes", "value": value})
}

fn changes(base: Option<&Value>, from: &Value, into: &Value, extra: &Value) -> Value {
    config_request(request(base, from, into, extra)).unwrap()
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
    let parsed =
        config_request(json!({"operation": "parse_environment", "value": parent()})).unwrap();
    assert_eq!(result["next"], parsed);
    assert_eq!(result["base"], parsed);
}

#[test]
fn move_conflict_and_differ_rows_follow_the_one_rule() {
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
fn update_marks_provided_nodes_live_and_base_only_nodes_left_out() {
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
        config_request(json!({"operation": "redact_environment", "value": env})).unwrap()
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
    let error = config_request(request(None, &from, &parent(), &json!({}))).unwrap_err();
    assert_eq!(error.path, "environment");
    let error =
        config_request(json!({"operation": "branch_changes", "value": {"from": 1}})).unwrap_err();
    assert_eq!(error.path, "request");
}

/// The SDK contract test sends the same input through WebAssembly and expects this review.
#[test]
fn contract_review_string() {
    let env = |start: &str, n: u32| {
        json!({"version": 1, "environmentSlug": "e", "volumes": [], "services": [{
            "id": id(n, 1), "lineageId": API, "slug": "api", "variables": [], "volumeAttachments": [],
            "config": {"version": 2, "privateDns": "api", "preDeployCommand": null, "startCommand": start,
                       "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped",
                       "source": {"version": 1, "type": "image", "image": "api:1", "credentials": {"type": "none"}}}}]})
    };
    let result = config_request(json!({"operation": "branch_changes", "value": {
        "base": env("a", 1), "from": env("b", 2), "into": env("a", 3), "provided": [],
        "hostnames": {"from": "", "into": ""}, "fromKept": false}}))
    .unwrap();
    assert_eq!(
        result["review"],
        r#"[{"key":"a0000000-0000-4000-8000-000000000001:startCommand","role":"move","conflict":false,"base":"a","from":"b","into":"a"}]"#
    );
    let picked = config_request(json!({"operation": "branch_changes", "value": {
        "base": env("a", 1), "from": env("b", 2), "into": env("a", 3), "provided": [],
        "hostnames": {"from": "", "into": ""}, "fromKept": false,
        "picks": [{"key": format!("{API}:startCommand")}]}}))
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
    config_request(request(
        Some(&parent()),
        from,
        into,
        &json!({"parent": parent_input, "picks": picks}),
    ))
}

fn table_picks() -> Value {
    let server = json!({"kind": "secret", "encryptedValue": {"version": 1, "iv": "iv", "tag": "tag", "ciphertext": "server-cipher"}});
    json!([
        {"key": format!("{API}:startCommand")},
        {"key": format!("{API}:mounts.{DATA}")},
        {"key": format!("{API}:variables.PLAIN"), "choice": "parent"},
        {"key": format!("{API}:variables.NEW_PLAIN"), "choice": "from"},
        {"key": format!("{API}:variables.NEW_SECRET"), "choice": "new",
         "newValue": {"value": server, "valueFingerprint": "fp-server"}},
        {"key": format!("{API}:variables.UNSUPPLIED"), "choice": "new"},
    ])
}

#[test]
fn picks_land_in_next_and_advance_base_by_exactly_the_picks() {
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
    assert!(value("UNSUPPLIED").is_none());
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
        json!([{"key": format!("{API}:variables.PLAIN"), "choice": "leave_out"},
               {"key": format!("{API}:variables.NEW_PLAIN"), "choice": "leave_out"}]),
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
            json!([{"key": format!("{API}:startCommand"), "choice": "from"}]),
            "picks.choice",
        ),
        (
            json!([{"key": format!("{API}:variables.PLAIN")}]),
            "picks.choice",
        ),
        (
            json!([{"key": format!("{API}:variables.NEW_PLAIN"), "choice": "parent"}]),
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
    other[2]["choice"] = json!("from");
    assert_ne!(picked, review(&from, &parent(), other));
    let compare_only = changes(Some(&parent()), &from, &parent(), &json!({}));
    assert_ne!(picked, compare_only["review"].as_str().unwrap());
}
