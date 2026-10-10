#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Branch Sync's engine: the rows between two Environments over what they last shared,
//! and landing the picked ones. A new Branch compares the same way, over no base.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{
    Applied, Arrives, Cell, Cells, ConfigError, EncryptedSecretValue, Hostnames, NodeRef, Plan,
    PlannedRow, Policy, RowId, SavedEnvironmentIntent, SealedSecret, Sides, Verdict, Way, Why,
    dependents, name_of, parse_environment_intent, plan, put, put_back, redact_environment_intent,
    unapply,
};
use serde_json::{Value, json};

const API: &str = "a0000000-0000-4000-8000-000000000001";
const WEB: &str = "a0000000-0000-4000-8000-000000000002";
const WORKER: &str = "a0000000-0000-4000-8000-000000000003";
const CACHE: &str = "a0000000-0000-4000-8000-000000000004";
const DATA: &str = "a0000000-0000-4000-8000-000000000005";
const JOBS: &str = "a0000000-0000-4000-8000-000000000006";

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

fn image(seed: u32, n: u32, lineage: &str, slug: &str) -> Value {
    service(
        seed,
        n,
        lineage,
        slug,
        json!({"version": 1, "type": "image", "image": format!("{slug}:1"), "credentials": {"type": "none"}}),
    )
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
    json!({"version": 1, "environmentSlug": "production",
           "services": [api, web, image(seed, 3, WORKER, "worker"), image(seed, 4, CACHE, "cache")],
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

fn empty(slug: &str) -> Value {
    json!({"version": 1, "environmentSlug": slug, "services": [], "volumes": []})
}

fn jobs() -> Value {
    image(0xc000_0000, 50, JOBS, "jobs")
}

fn svc<'env>(env: &'env mut Value, lineage: &str) -> &'env mut Value {
    env["services"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|s| s["lineageId"] == lineage)
        .unwrap()
}

fn var<'env>(env: &'env mut Value, lineage: &str, key: &str) -> &'env mut Value {
    svc(env, lineage)["variables"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|v| v["key"] == key)
        .unwrap()
}

fn find<'env>(list: &'env Value, field: &str, lineage: &str) -> &'env Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|n| n[field] == lineage)
        .unwrap()
}

fn intent(env: &Value) -> SavedEnvironmentIntent {
    parse_environment_intent(env.clone()).unwrap()
}

fn json_of(intent: &SavedEnvironmentIntent) -> Value {
    serde_json::to_value(intent).unwrap()
}

fn ids(rows: &[String]) -> BTreeSet<RowId> {
    rows.iter().map(|row| row.parse().unwrap()).collect()
}

#[derive(Default)]
struct Opts {
    from_marks: Vec<String>,
    into_marks: Vec<String>,
    live: Vec<&'static str>,
    own: Option<Vec<String>>,
    held: Vec<String>,
    accepted: Vec<(String, Cell)>,
    /// Defaults to a Branch (`-pr-7`) syncing into its Parent.
    hostnames: Option<(&'static str, &'static str)>,
}

fn compare_with(base: Option<&Value>, from: &Value, into: &Value, way: Way, opts: Opts) -> Plan {
    let base = base.map(intent);
    let (suffix_from, suffix_into) = opts.hostnames.unwrap_or(("-pr-7", ""));
    plan(
        Sides {
            base: base.as_ref(),
            from: &intent(from),
            into: &intent(into),
            hostnames: Hostnames {
                from: suffix_from.to_owned(),
                into: suffix_into.to_owned(),
            },
        },
        &Policy {
            way,
            from_marks: ids(&opts.from_marks),
            into_marks: ids(&opts.into_marks),
            live: opts.live.iter().map(|l| (*l).to_owned()).collect(),
            own: opts.own.as_deref().map(ids),
            held: ids(&opts.held),
            accepted: opts
                .accepted
                .iter()
                .map(|(row, cell)| (row.parse().unwrap(), cell.clone()))
                .collect(),
        },
    )
}

fn compare(base: Option<&Value>, from: &Value, into: &Value, way: Way) -> Plan {
    compare_with(base, from, into, way, Opts::default())
}

fn land(plan: &Plan, picks: &[String]) -> Result<Applied, ConfigError> {
    plan.apply(&ids(picks), &BTreeMap::new())
}

fn next_of(plan: &Plan, picks: &[String]) -> Value {
    json_of(&land(plan, picks).unwrap().next)
}

/// Rows as `id move conflict=…` or `id differ why` lines for compact assertions.
fn summary(plan: &Plan) -> Vec<String> {
    plan.rows()
        .iter()
        .map(|row| match row.verdict {
            Verdict::Moves { conflict, .. } => format!("{} move conflict={conflict}", row.id),
            Verdict::Differs(why) => format!("{} differ {}", row.id, json!(why).as_str().unwrap()),
        })
        .collect()
}

fn row<'plan>(plan: &'plan Plan, id: &str) -> &'plan PlannedRow {
    plan.rows()
        .iter()
        .find(|row| row.id.to_string() == id)
        .unwrap_or_else(|| panic!("no row {id}"))
}

fn moves(plan: &Plan) -> Vec<String> {
    plan.rows()
        .iter()
        .filter(|row| matches!(row.verdict, Verdict::Moves { .. }))
        .map(|row| row.id.to_string())
        .collect()
}

fn assert_has(rows: &[String], expected: &[String]) {
    for expected in expected {
        assert!(rows.contains(expected), "{expected} missing from {rows:#?}");
    }
}

fn val(value: Value) -> Cell {
    Cell::Value(value)
}

fn sealed(ciphertext: &str, fingerprint: &str) -> SealedSecret {
    SealedSecret {
        fingerprint: fingerprint.to_owned(),
        value: EncryptedSecretValue {
            version: 1,
            iv: "iv".to_owned(),
            tag: "tag".to_owned(),
            ciphertext: ciphertext.to_owned(),
        },
    }
}

#[test]
fn row_ids_read_back_as_they_print() {
    for text in [
        format!("{API}:node"),
        format!("{DATA}:data"),
        format!("{DATA}:name"),
        format!("{DATA}:storage"),
        format!("{API}:source"),
        format!("{API}:healthcheck"),
        format!("{API}:build.buildMethod"),
        format!("{API}:mounts.{DATA}"),
        format!("{API}:variables.DATABASE_URL"),
    ] {
        let id: RowId = text.parse().unwrap();
        assert_eq!(id.to_string(), text);
        assert_eq!(id.lineage(), text.split_once(':').unwrap().0);
        assert_eq!(id.at().to_string(), text.split_once(':').unwrap().1);
        assert_eq!(json!(id), json!(text));
        assert_eq!(serde_json::from_value::<RowId>(json!(text)).unwrap(), id);
    }
    for bad in [
        "nope".to_owned(),
        ":node".to_owned(),
        format!("{API}:healthcheck.path"),
        format!("{API}:mounts."),
        format!("{API}:variables."),
    ] {
        let error = bad.parse::<RowId>().unwrap_err();
        assert_eq!(
            (error.path.as_str(), error.message.as_str()),
            ("row", "Unknown change")
        );
    }
}

#[test]
fn config_row_ids_read_back_as_they_print() {
    let config = "00000000-0000-4000-8000-000000000010";
    for (text, at) in [
        (
            format!("{config}:files.conf.d/a.b.yml"),
            "files.conf.d/a.b.yml",
        ),
        (format!("{config}:files..htpasswd"), "files..htpasswd"),
        (
            format!("{API}:configs.{config}"),
            "configs.00000000-0000-4000-8000-000000000010",
        ),
    ] {
        let id: RowId = text.parse().unwrap();
        assert_eq!(id.to_string(), text);
        assert_eq!(id.at().to_string(), at);
        assert_eq!(serde_json::from_value::<RowId>(json!(text)).unwrap(), id);
    }
    assert_eq!(
        format!("{config}:files.conf.d/a.b.yml")
            .parse::<RowId>()
            .unwrap()
            .lineage(),
        config
    );
    for bad in [
        format!("{config}:files."),
        format!("{config}:files./etc/a"),
        format!("{config}:files.a/../b"),
        format!("{config}:files.a/b/c/d/e"),
        format!("{API}:configs."),
    ] {
        let error = bad.parse::<RowId>().unwrap_err();
        assert_eq!(
            (error.path.as_str(), error.message.as_str()),
            ("row", "Unknown change"),
            "{bad}"
        );
    }
}

#[test]
fn own_copy_with_fresh_ids_has_only_meant_to_differ_rows() {
    let plan = compare(Some(&parent()), &branch(), &parent(), Way::Sync);
    assert_eq!(
        summary(&plan),
        [
            format!("{API}:routes differ custom_domain"),
            format!("{WORKER}:node differ live"),
            format!("{CACHE}:node differ left_out"),
            format!("{DATA}:data differ data"),
        ]
    );
    assert_eq!(
        row(&plan, &format!("{API}:routes")).into,
        val(json!(["api.example.com"]))
    );
    let applied = land(&plan, &[]).unwrap();
    assert_eq!(json_of(&applied.next), json_of(&intent(&parent())));
    assert_eq!(
        json_of(&applied.base),
        json_of(&redact_environment_intent(intent(&parent())))
    );
    assert!(applied.landed.is_empty() && applied.waiting.is_empty());
}

#[test]
fn sync_conflict_and_differ_rows_follow_the_one_rule() {
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
    let plan = compare(Some(&parent()), &from, &into, Way::Sync);
    assert_has(
        &summary(&plan),
        &[
            format!("{API}:startCommand move conflict=true"),
            format!("{API}:preDeployCommand move conflict=false"),
            format!("{API}:mounts.{DATA} move conflict=false"),
            format!("{API}:replicas differ sizing"),
            format!("{API}:memLimit differ sizing"),
            format!("{API}:managedHostnames differ generated_address"),
            format!("{WEB}:source.branch differ git_branch"),
        ],
    );
    let conflict = row(&plan, &format!("{API}:startCommand"));
    assert_eq!(
        (&conflict.base, &conflict.into, &conflict.from),
        (
            &Cell::Absent,
            &val(json!("into-start")),
            &val(json!("from-start"))
        )
    );
}

/// Each Way's row of the policy table, over the same four changes.
#[test]
fn the_policy_table_per_way() {
    let mut from = parent();
    let api = svc(&mut from, API);
    api["config"]["startCommand"] = json!("start");
    api["variables"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v["key"] != "PLAIN");
    api["variables"].as_array_mut().unwrap().push(variable(
        0xb000_0000,
        13,
        "NEW",
        secret("new-cipher"),
        "fp-new",
    ));
    var(&mut from, API, "TOKEN")["value"] = secret("rotated-cipher");
    var(&mut from, API, "TOKEN")["valueFingerprint"] = json!("fp-rotated");
    let mut own = parent();
    var(&mut own, API, "TOKEN")["valueFingerprint"] = json!("fp-own");
    let verdicts = |into: &Value, way| {
        compare(Some(&parent()), &from, into, way)
            .rows()
            .iter()
            .filter_map(|row| match row.verdict {
                Verdict::Moves {
                    conflict, arrives, ..
                } => Some(format!(
                    "{} {arrives:?} conflict={conflict}",
                    row.id.to_string().split_once(':').unwrap().1
                )),
                Verdict::Differs(_) => None,
            })
            .collect::<Vec<_>>()
    };
    let start = "startCommand AsIs conflict=false";
    let plain = "variables.PLAIN AsIs conflict=false";
    let new = "variables.NEW AsIs conflict=false";
    let token = "variables.TOKEN AsIs conflict=false";
    // Sync: removals and the receiver's secrets stay; a new secret needs its value.
    assert_eq!(
        verdicts(&parent(), Way::Sync),
        [start, "variables.NEW NeedsValue conflict=false"]
    );
    assert_eq!(
        verdicts(&own, Way::Sync),
        [start, "variables.NEW NeedsValue conflict=false"]
    );
    // Follow: removals and secrets carry, except over a secret the receiver set itself.
    assert_eq!(verdicts(&parent(), Way::Follow), [start, new, plain, token]);
    assert_eq!(verdicts(&own, Way::Follow), [start, new, plain]);
    // Copy: everything as it is, over the receiver's own secret too.
    assert_eq!(verdicts(&parent(), Way::Copy), [start, new, plain, token]);
    assert_eq!(
        verdicts(&own, Way::Copy),
        [start, new, plain, "variables.TOKEN AsIs conflict=true"]
    );
}

#[test]
fn marks_follow_the_way() {
    let mut from = branch();
    svc(&mut from, API)["config"]["startCommand"] = json!("from-start");
    let start = format!("{API}:startCommand");
    let marked = |way, from_marks: &[&String], into_marks: &[&String]| {
        let opts = Opts {
            from_marks: from_marks.iter().map(|m| (*m).clone()).collect(),
            into_marks: into_marks.iter().map(|m| (*m).clone()).collect(),
            ..Opts::default()
        };
        compare_with(Some(&parent()), &from, &parent(), way, opts)
            .rows()
            .iter()
            .any(|row| {
                row.id.to_string() == start && row.verdict == Verdict::Differs(Why::NeverSynced)
            })
    };
    // A Sync honors both sides' marks; Follow only the receiver's; Copy none.
    assert!(marked(Way::Sync, &[&start], &[]));
    assert!(marked(Way::Sync, &[], &[&start]));
    assert!(!marked(Way::Follow, &[&start], &[]));
    assert!(marked(Way::Follow, &[], &[&start]));
    assert!(!marked(Way::Copy, &[&start], &[&start]));
    // Marks match exactly: one on another row covers nothing.
    assert!(!marked(
        Way::Sync,
        &[&format!("{API}:preDeployCommand")],
        &[]
    ));
}

#[test]
fn only_the_senders_own_changes_start_ticked() {
    let mut from = branch();
    svc(&mut from, API)["config"]["startCommand"] = json!("from-start");
    svc(&mut from, API)["config"]["preDeployCommand"] = json!("from-migrate");
    let opts = Opts {
        own: Some(vec![format!("{API}:startCommand")]),
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &from, &parent(), Way::Sync, opts);
    let ticked = |id: &str| match row(&plan, id).verdict {
        Verdict::Moves { ticked, .. } => ticked,
        Verdict::Differs(_) => panic!("{id} differs"),
    };
    assert!(ticked(&format!("{API}:startCommand")));
    assert!(!ticked(&format!("{API}:preDeployCommand")));
    // Unticked rows can still be picked.
    let next = next_of(&plan, &[format!("{API}:preDeployCommand")]);
    assert_eq!(
        find(&next["services"], "lineageId", API)["config"]["preDeployCommand"],
        "from-migrate"
    );
}

#[test]
fn removals_and_equal_secrets_produce_no_rows_in_a_sync_and_land_as_they_are() {
    let mut from = branch();
    let api = svc(&mut from, API);
    api["variables"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v["key"] != "PLAIN");
    api["volumeAttachments"] = json!([]);
    // Same secret value under a new id and new ciphertext.
    var(&mut from, API, "TOKEN")["value"] = secret("branch-cipher");
    let rows = summary(&compare(Some(&parent()), &from, &parent(), Way::Sync));
    assert!(rows.iter().all(|r| !r.contains(":variables.")), "{rows:#?}");
    assert!(rows.iter().all(|r| !r.contains(":mounts.")), "{rows:#?}");
    // api's credential has another id here, but credentials compare by presence.
    assert!(rows.iter().all(|r| !r.contains(":source")), "{rows:#?}");

    // As it is, each removal lands.
    let copy = compare(Some(&parent()), &from, &parent(), Way::Copy);
    let mut next = next_of(
        &copy,
        &[
            format!("{API}:variables.PLAIN"),
            format!("{API}:mounts.{DATA}"),
        ],
    );
    let api = svc(&mut next, API);
    assert_eq!(api["volumeAttachments"], json!([]));
    assert!(
        api["variables"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["key"] != "PLAIN")
    );

    // Credentials compare by presence, as part of the source: adding one where `into`
    // has none changes the source.
    let mut into = parent();
    svc(&mut into, API)["config"]["source"]["credentials"] = json!({"type": "none"});
    let rows = summary(&compare(None, &branch(), &into, Way::Sync));
    assert!(
        rows.contains(&format!("{API}:source move conflict=true")),
        "{rows:#?}"
    );
}

#[test]
fn a_secret_never_changes_where_it_is_and_arrives_without_a_value() {
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
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    assert!(!summary(&plan).iter().any(|r| r.contains("TOKEN")));
    let new_secret = format!("{API}:variables.NEW_SECRET");
    let arriving = row(&plan, &new_secret);
    assert_eq!(
        arriving.from,
        Cell::Secret {
            fingerprint: "fp-new".to_owned(),
        }
    );
    assert!(matches!(
        arriving.verdict,
        Verdict::Moves {
            arrives: Arrives::NeedsValue,
            ..
        }
    ));

    // Picked without a value, it lands without one and waits; TOKEN keeps `into`'s value.
    let applied = land(&plan, std::slice::from_ref(&new_secret)).unwrap();
    assert_eq!(
        applied.waiting,
        ids(std::slice::from_ref(&new_secret))
            .into_iter()
            .collect::<Vec<_>>()
    );
    let mut next = json_of(&applied.next);
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
    let again = compare(Some(&json_of(&applied.base)), &from, &next, Way::Sync);
    assert!(!summary(&again).iter().any(|r| r.contains("SECRET")));

    // A secret without a value, sent on, arrives as a secret without a value.
    let mut sender = next.clone();
    sender["environmentSlug"] = json!("pr-7");
    let onward = compare(Some(&parent()), &sender, &parent(), Way::Sync);
    assert_eq!(row(&onward, &new_secret).from, Cell::SecretWithoutValue);
    let applied = land(&onward, std::slice::from_ref(&new_secret)).unwrap();
    assert_eq!(applied.waiting.len(), 1);
    assert_eq!(
        var(&mut json_of(&applied.next), API, "NEW_SECRET")["value"],
        json!({"kind": "secret_without_value"})
    );
}

#[test]
fn a_needs_value_pick_lands_the_sealed_value_it_is_given() {
    let mut from = branch();
    svc(&mut from, API)["config"]["startCommand"] = json!("from-start");
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
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    let (new_secret, start): (RowId, RowId) = (
        format!("{API}:variables.NEW_SECRET").parse().unwrap(),
        format!("{API}:startCommand").parse().unwrap(),
    );
    let picks = BTreeSet::from([new_secret.clone(), start.clone()]);
    // A value for a row that arrives as it is is ignored.
    let values = BTreeMap::from([
        (new_secret.clone(), sealed("given-cipher", "fp-given")),
        (start.clone(), sealed("ignored", "fp-ignored")),
    ]);
    let applied = plan.apply(&picks, &values).unwrap();
    assert!(applied.waiting.is_empty());
    let mut next = json_of(&applied.next);
    let arrived = var(&mut next, API, "NEW_SECRET").clone();
    assert_eq!(
        arrived["value"]["encryptedValue"]["ciphertext"],
        "given-cipher"
    );
    assert_eq!(arrived["valueFingerprint"], "fp-given");
    assert_eq!(svc(&mut next, API)["config"]["startCommand"], "from-start");
    let landed = applied.landed.iter().find(|l| l.row == new_secret).unwrap();
    assert_eq!(
        landed.value,
        Cell::Secret {
            fingerprint: "fp-given".to_owned(),
        },
        "what landed is redacted"
    );
}

#[test]
fn following_a_rotated_secret_reaches_only_a_receiver_that_never_set_its_own() {
    let mut from = parent();
    var(&mut from, API, "TOKEN")["value"] = secret("rotated-cipher");
    var(&mut from, API, "TOKEN")["valueFingerprint"] = json!("fp-rotated");
    let token = format!("{API}:variables.TOKEN");
    let opts = || Opts {
        hostnames: Some(("", "-pr-7")),
        ..Opts::default()
    };
    // A Sync never changes a secret the receiver has.
    let synced = compare_with(Some(&parent()), &from, &branch(), Way::Sync, opts());
    assert!(!summary(&synced).iter().any(|r| r.contains("TOKEN")));

    // Following, the rotation reaches a Branch that still holds the shared value.
    let followed = compare_with(Some(&parent()), &from, &branch(), Way::Follow, opts());
    assert!(summary(&followed).contains(&format!("{token} move conflict=false")));
    let mut next = next_of(&followed, std::slice::from_ref(&token));
    assert_eq!(
        var(&mut next, API, "TOKEN")["value"]["encryptedValue"]["ciphertext"],
        "rotated-cipher"
    );

    // One that set its own keeps it: nothing is offered.
    let mut own = branch();
    var(&mut own, API, "TOKEN")["valueFingerprint"] = json!("fp-own");
    let kept = compare_with(Some(&parent()), &from, &own, Way::Follow, opts());
    assert!(!summary(&kept).iter().any(|r| r.contains("TOKEN")));
}

#[test]
fn following_fills_a_secret_the_branch_holds_without_a_value() {
    let mut base = parent();
    svc(&mut base, API)["variables"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v["key"] != "TOKEN");
    let mut into = branch();
    var(&mut into, API, "TOKEN")["value"] = json!({"kind": "secret_without_value"});
    var(&mut into, API, "TOKEN")["valueFingerprint"] = json!("");
    let token = format!("{API}:variables.TOKEN");
    let opts = Opts {
        hostnames: Some(("", "-pr-7")),
        ..Opts::default()
    };
    let followed = compare_with(Some(&base), &parent(), &into, Way::Follow, opts);
    assert!(summary(&followed).contains(&format!("{token} move conflict=false")));
    let mut next = next_of(&followed, &[token]);
    assert_eq!(
        var(&mut next, API, "TOKEN")["value"]["encryptedValue"]["ciphertext"],
        "parent-cipher"
    );
}

#[test]
fn a_sync_never_changes_a_secret_the_receiver_holds() {
    let mut from = branch();
    // `from` turns TOKEN into a plain value; `into` holds it as a secret.
    var(&mut from, API, "TOKEN")["value"] = literal("plain");
    var(&mut from, API, "TOKEN")["valueFingerprint"] = json!("fp-plain");
    let token = format!("{API}:variables.TOKEN");
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    assert!(!summary(&plan).iter().any(|r| r.contains("TOKEN")));
    // Nor one it holds without a value.
    let mut into = parent();
    var(&mut into, API, "TOKEN")["value"] = json!({"kind": "secret_without_value"});
    var(&mut into, API, "TOKEN")["valueFingerprint"] = json!("");
    let held = compare(Some(&parent()), &from, &into, Way::Sync);
    assert!(!summary(&held).iter().any(|r| r.contains("TOKEN")));
    // Picking it anyway is refused.
    let error = land(&plan, &[token]).unwrap_err();
    assert_eq!(
        (error.path.as_str(), error.message.as_str()),
        ("picks", "Unknown change")
    );
}

/// The Store reads what an Environment changed itself this way: a secret that lost
/// its value, or got one, is a change.
#[test]
fn a_copy_sees_a_secret_lose_or_get_its_value() {
    let valued = parent();
    let mut valueless = parent();
    var(&mut valueless, API, "TOKEN")["value"] = json!({"kind": "secret_without_value"});
    var(&mut valueless, API, "TOKEN")["valueFingerprint"] = json!("");
    let token = format!("{API}:variables.TOKEN");
    for (base, working) in [(&valued, &valueless), (&valueless, &valued)] {
        let plan = compare(Some(base), working, base, Way::Copy);
        assert_eq!(moves(&plan), std::slice::from_ref(&token));
    }
}

#[test]
fn a_sync_into_a_branch_marks_its_live_nodes_live_and_base_only_nodes_left_out() {
    let opts = Opts {
        live: vec![WORKER],
        hostnames: Some(("", "-pr-7")),
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &parent(), &branch(), Way::Sync, opts);
    assert_eq!(
        summary(&plan),
        [
            format!("{API}:routes differ custom_domain"),
            format!("{WORKER}:node differ live"),
            format!("{CACHE}:node differ left_out"),
            format!("{DATA}:data differ data"),
        ]
    );
}

#[test]
fn the_digest_is_redacted_and_ignores_ids_and_address_suffixes() {
    let mut from = branch();
    var(&mut from, API, "PLAIN")["value"] = literal("b");
    var(&mut from, API, "PLAIN")["valueFingerprint"] = json!("fp-plain-b");
    let digest = |base: &Value, from: &Value, into: &Value, suffix| {
        let opts = Opts {
            hostnames: Some((suffix, "")),
            ..Opts::default()
        };
        compare_with(Some(base), from, into, Way::Sync, opts).digest()
    };
    let result = digest(&parent(), &from, &parent(), "-pr-7");
    assert!(
        !result.contains("cipher") && !result.contains("0000000028"),
        "{result}"
    );
    for seed in [0xb000_0000_u32, 0xc000_0000] {
        assert!(
            !result.contains(&format!("{seed:08x}")),
            "digest holds an id: {result}"
        );
    }
    let redact = |env: &Value| json_of(&redact_environment_intent(intent(env)));
    assert_eq!(
        digest(
            &redact(&parent()),
            &redact(&from),
            &redact(&parent()),
            "-pr-7"
        ),
        result
    );
    let reided = digest(
        &reid(parent(), 0xd000_0000),
        &reid(from.clone(), 0xd100_0000),
        &reid(parent(), 0xd200_0000),
        "-pr-7",
    );
    assert_eq!(reided, result);
    // Another Branch's generated address is the same address.
    let mut renamed = from.clone();
    svc(&mut renamed, API)["config"]["managedHostnames"][0]["prefix"] = json!("api-pr-9");
    assert_eq!(digest(&parent(), &renamed, &parent(), "-pr-9"), result);
    // A changed value or mount changes it.
    let mut changed = from.clone();
    var(&mut changed, API, "PLAIN")["value"] = literal("c");
    assert_ne!(digest(&parent(), &changed, &parent(), "-pr-7"), result);
    let mut moved = from.clone();
    svc(&mut moved, API)["volumeAttachments"][0]["mountPath"] = json!("/moved");
    assert_ne!(digest(&parent(), &moved, &parent(), "-pr-7"), result);
}

/// The digest is a canonical string, stable across ids.
#[test]
fn contract_digest_string() {
    let env = |start: &str, n: u32| {
        json!({"version": 1, "environmentSlug": "e", "volumes": [], "services": [{
            "id": id(n, 1), "lineageId": API, "slug": "api", "variables": [], "volumeAttachments": [],
            "config": {"version": 2, "privateDns": "api", "preDeployCommand": null, "startCommand": start,
                       "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped",
                       "source": {"version": 1, "type": "image", "image": "api:1", "credentials": {"type": "none"}}}}]})
    };
    let plan = compare(Some(&env("a", 1)), &env("b", 2), &env("a", 3), Way::Sync);
    assert_eq!(
        plan.digest(),
        r#"[{"id":"a0000000-0000-4000-8000-000000000001:startCommand","base":{"value":"a"},"from":{"value":"b"},"into":{"value":"a"},"requires":null,"verdict":{"moves":{"conflict":false,"ticked":true,"arrives":"as_is"}}}]"#
    );
    let next = next_of(&plan, &[format!("{API}:startCommand")]);
    assert_eq!(next["services"][0]["config"]["startCommand"], "b");
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

fn table_picks() -> Vec<String> {
    vec![
        format!("{API}:startCommand"),
        format!("{API}:mounts.{DATA}"),
        format!("{API}:variables.PLAIN"),
        format!("{API}:variables.NEW_PLAIN"),
        format!("{API}:variables.NEW_SECRET"),
    ]
}

#[test]
fn a_sync_lands_its_picks_and_advances_the_base_by_exactly_them() {
    let from = changed_branch();
    let mut into = parent();
    svc(&mut into, API)["config"]["startCommand"] = json!("into-start");
    let applied = land(
        &compare(Some(&parent()), &from, &into, Way::Sync),
        &table_picks(),
    )
    .unwrap();
    let next = json_of(&applied.next);
    let api = find(&next["services"], "lineageId", API);
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
    assert_eq!(value("PLAIN").unwrap()["value"], literal("b"));
    let new_plain = value("NEW_PLAIN").unwrap();
    assert_eq!(new_plain["value"], literal("x"));
    assert_eq!(new_plain["valueFingerprint"], "fp-x");
    assert_ne!(
        new_plain["id"],
        var(&mut from.clone(), API, "NEW_PLAIN")["id"]
    );
    // A secret lands without its value; an unpicked one stays out.
    let new_secret = value("NEW_SECRET").unwrap();
    assert_eq!(new_secret["value"], json!({"kind": "secret_without_value"}));
    assert_eq!(value("UNSUPPLIED"), None);
    // Nothing `into` had is lost.
    assert_eq!(
        value("TOKEN").unwrap()["value"]["encryptedValue"]["ciphertext"],
        "parent-cipher"
    );
    assert_eq!(next["services"].as_array().unwrap().len(), 4);
    assert_eq!(applied.landed.len(), 5);

    let base = json_of(&applied.base);
    assert!(
        !base.to_string().contains("cipher"),
        "base holds sealed material"
    );
    let again = summary(&compare(Some(&base), &from, &next, Way::Sync));
    for settled in [
        "startCommand",
        "mounts.",
        "variables.PLAIN",
        "variables.NEW_PLAIN",
        "variables.NEW_SECRET",
    ] {
        assert!(
            !again
                .iter()
                .any(|r| r.contains(&format!(":{settled}")) && r.contains(" move ")),
            "{settled} in {again:#?}"
        );
    }
    assert_has(
        &again,
        &[
            format!("{API}:preDeployCommand move conflict=false"),
            format!("{DATA}:name move conflict=false"),
        ],
    );

    // As it is, the sealed value lands.
    let mut carried = next_of(
        &compare(Some(&parent()), &from, &into, Way::Copy),
        &table_picks(),
    );
    assert_eq!(
        var(&mut carried, API, "NEW_SECRET")["value"]["encryptedValue"]["ciphertext"],
        "branch-cipher"
    );
}

#[test]
fn bad_picks_are_refused() {
    let plan = compare(Some(&parent()), &changed_branch(), &parent(), Way::Sync);
    for (pick, message) in [
        (format!("{API}:cpuLimit"), "Unknown change"),
        (format!("{API}:routes"), "Change is meant to differ"),
        (format!("{DATA}:data"), "Change is meant to differ"),
        (format!("{WORKER}:node"), "Change is meant to differ"),
    ] {
        let error = land(&plan, std::slice::from_ref(&pick)).unwrap_err();
        assert_eq!(
            (error.path.as_str(), error.message.as_str()),
            ("picks", message),
            "{pick}"
        );
    }
}

#[test]
fn volume_storage_moves_as_one_setting_with_its_exact_limit() {
    let mut from = branch();
    let storage = json!({"kind":"provisioned","maximumBytes":7_000_000_000_i64});
    from["volumes"][0]["storage"] = storage.clone();
    let key = format!("{DATA}:storage");
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    assert_eq!(moves(&plan), std::slice::from_ref(&key));
    let next = next_of(&plan, &[key]);
    assert_eq!(next["volumes"][0]["storage"], storage);
    assert_eq!(
        next["volumes"][0]["resourceId"],
        parent()["volumes"][0]["resourceId"]
    );
}

#[test]
fn healthcheck_is_one_row() {
    let mut from = branch();
    let check = json!({"type": "http", "path": "/up", "timeoutSeconds": 30});
    svc(&mut from, API)["config"]["healthcheck"] = check.clone();
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    let key = format!("{API}:healthcheck");
    assert_eq!(moves(&plan), std::slice::from_ref(&key));
    assert_eq!(row(&plan, &key).from, val(check.clone()));
    let mut next = next_of(&plan, std::slice::from_ref(&key));
    assert_eq!(svc(&mut next, API)["config"]["healthcheck"], check);
    // Its removal is a removal: back to none.
    let back = compare(Some(&next), &branch(), &next, Way::Copy);
    let mut next = next_of(&back, &[key]);
    assert_eq!(
        svc(&mut next, API)["config"]["healthcheck"],
        json!({"type": "none"})
    );
}

#[test]
fn repository_authority_moves_with_the_repository() {
    let mut from = branch();
    svc(&mut from, WEB)["config"]["source"]["access"] =
        json!({"type": "github-installation", "installationId": 9});
    svc(&mut from, WEB)["config"]["source"]["repositoryId"] = json!(2);
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    assert_eq!(moves(&plan), [format!("{WEB}:source")]);
    let next = next_of(&plan, &moves(&plan));
    let web = find(&next["services"], "lineageId", WEB);
    assert_eq!(web["config"]["source"]["repositoryId"], 2);
    assert_eq!(web["config"]["source"]["access"]["installationId"], 9);
}

#[test]
fn a_source_of_another_kind_switches_the_source() {
    let mut from = branch();
    svc(&mut from, WEB)["config"]["source"] =
        json!({"version": 1, "type": "image", "image": "web:2", "credentials": {"type": "none"}});
    let plan = compare(Some(&parent()), &from, &parent(), Way::Copy);
    let next = next_of(&plan, &moves(&plan));
    assert_eq!(
        find(&next["services"], "lineageId", WEB)["config"]["source"],
        json!({"version": 1, "type": "image", "image": "web:2", "credentials": {"type": "none"}})
    );
    // And back: the repository brings its authority, the branch stays each side's.
    let back = compare(Some(&next), &parent(), &next, Way::Copy);
    let next = next_of(&back, &moves(&back));
    assert_eq!(
        find(&next["services"], "lineageId", WEB)["config"]["source"],
        json!({"version": 2, "type": "git", "repository": "acme/web", "repositoryId": 1,
               "access": {"type": "public"}, "rootDir": "/", "branch": {"type": "disconnected", "previousName": null}})
    );
}

/// Each picked node with every row under it, as the Store creates a Branch.
fn create(
    parent: &Value,
    nodes: &[&str],
    suffix: &'static str,
) -> (String, Result<Applied, ConfigError>) {
    let opts = Opts {
        live: vec![WORKER],
        hostnames: Some(("", suffix)),
        ..Opts::default()
    };
    let plan = compare_with(None, parent, &empty("pr-7"), Way::Copy, opts);
    let picks: Vec<_> = moves(&plan)
        .into_iter()
        .filter(|row| {
            nodes
                .iter()
                .any(|node| row.starts_with(&format!("{node}:")))
        })
        .collect();
    (plan.digest(), land(&plan, &picks))
}

#[test]
fn create_copies_picked_nodes_with_fresh_ids_and_the_branch_naming() {
    let parent = parent();
    let (digest, applied) = create(&parent, &[API, WEB, DATA], "-pr-7");
    let applied = applied.unwrap();
    let next = json_of(&applied.next);
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

    let base = json_of(&applied.base);
    let lineages: Vec<_> = base["services"]
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
    assert!(!base.to_string().contains("cipher"));

    let (again_digest, again) = create(&parent, &[API, WEB, DATA], "-pr-7");
    assert_eq!(again_digest, digest);
    assert_ne!(
        json_of(&again.unwrap().next)["services"][0]["id"],
        api["id"]
    );
}

#[test]
fn empty_picks_apply_nothing_but_still_create_the_base() {
    let (_, applied) = create(&parent(), &[], "-pr-7");
    let applied = applied.unwrap();
    assert!(applied.next.services.is_empty());
    let lineages: Vec<_> = applied
        .base
        .services
        .iter()
        .map(|s| s.lineage_id.as_str())
        .collect();
    assert_eq!(lineages, [API, WEB, CACHE]);
}

#[test]
fn bad_introductions_are_refused() {
    let parent = parent();
    let error = create(&parent, &[API], "-pr-7").1.unwrap_err();
    assert_eq!(
        (error.path.as_str(), error.message.as_str()),
        (
            "picks",
            "Mounted Volume is not in the receiving configuration"
        )
    );
    assert!(create(&parent, &[API, DATA], "-Bad_Label").1.is_err());
    assert!(create(&parent, &[API, DATA], "-pr-7").1.is_ok());
}

#[test]
fn a_sync_introduces_a_new_branch_service_into_the_parent() {
    let mut from = branch();
    let mut jobs = jobs();
    jobs["config"]["managedHostnames"] = json!([{"prefix": "jobs-pr-7", "targetPort": null}]);
    jobs["variables"] = json!([
        variable(0xc000_0000, 51, "MODE", literal("test"), "fp-mode"),
        variable(0xc000_0000, 52, "KEY", secret("test-cipher"), "fp-key"),
    ]);
    from["services"].as_array_mut().unwrap().push(jobs);
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    let node = format!("{JOBS}:node");
    let (mode, key) = (
        format!("{JOBS}:variables.MODE"),
        format!("{JOBS}:variables.KEY"),
    );
    // The node row carries its source and address; everything else is a row of its own.
    assert_eq!(
        plan.rows()
            .iter()
            .filter(|r| r.id.lineage() == JOBS)
            .map(|r| r.id.to_string())
            .collect::<Vec<_>>(),
        [node.clone(), key.clone(), mode.clone()]
    );
    let node_row = row(&plan, &node);
    assert_eq!(
        (&node_row.from, &node_row.into, node_row.requires.as_ref()),
        (
            &val(json!({
                "name": "jobs",
                "managedHostnames": [{ "prefix": "jobs", "targetPort": null }],
                "privateDns": "jobs",
                "source": {"type": "image", "version": 1, "image": "jobs:1", "credentials": false},
            })),
            &Cell::Absent,
            None
        )
    );
    for child in [&mode, &key] {
        assert_eq!(
            row(&plan, child).requires.as_ref().map(ToString::to_string),
            Some(node.clone())
        );
    }

    // A change picked without its node is refused.
    let error = land(&plan, std::slice::from_ref(&mode)).unwrap_err();
    assert_eq!(
        (error.path.as_str(), error.message.as_str()),
        ("picks", "Pick the new node this change belongs to")
    );

    // MODE is left unticked: it stays behind.
    let applied = land(&plan, &[node.clone(), key.clone()]).unwrap();
    assert_eq!(
        applied
            .waiting
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        [key]
    );
    let mut next = json_of(&applied.next);
    let jobs = find(&next["services"], "lineageId", JOBS);
    assert_ne!(jobs["id"], id(0xc000_0000, 50));
    assert_eq!(jobs["config"]["managedHostnames"][0]["prefix"], "jobs");
    let keys: Vec<_> = jobs["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["KEY"]);
    assert_eq!(
        var(&mut next, JOBS, "KEY")["value"],
        json!({"kind": "secret_without_value"}),
        "the new secret lands without its value"
    );
    // Base records the node and KEY, not MODE: only MODE is offered again.
    let again = compare(Some(&json_of(&applied.base)), &from, &next, Way::Sync);
    assert_eq!(
        summary(&again)
            .into_iter()
            .filter(|r| r.contains(JOBS))
            .collect::<Vec<_>>(),
        [format!("{mode} move conflict=false")]
    );

    // A name `into` already uses is refused.
    svc(&mut from, JOBS)["slug"] = json!("worker");
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    let error = land(&plan, &[node]).unwrap_err();
    assert_eq!(
        (error.path.as_str(), error.message.as_str()),
        ("picks", "Name or private address is already used")
    );
}

#[test]
fn a_private_address_clash_is_refused() {
    let mut from = branch();
    let mut jobs = jobs();
    jobs["config"]["privateDns"] = json!("worker");
    from["services"].as_array_mut().unwrap().push(jobs);
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    let error = land(&plan, &[format!("{JOBS}:node")]).unwrap_err();
    assert_eq!(
        (error.path.as_str(), error.message.as_str()),
        ("picks", "Name or private address is already used")
    );
}

#[test]
fn never_synced_rows_differ_and_never_land() {
    let mut from = branch();
    let api = svc(&mut from, API);
    api["config"]["startCommand"] = json!("from-start");
    api["config"]["healthcheck"] = json!({"type": "http", "path": "/up", "timeoutSeconds": 30});
    var(&mut from, API, "PLAIN")["value"] = literal("b");
    var(&mut from, API, "PLAIN")["valueFingerprint"] = json!("fp-plain-b");
    let mut jobs = jobs();
    jobs["variables"] = json!([
        variable(0xc000_0000, 51, "MODE", literal("test"), "fp-mode"),
        variable(0xc000_0000, 52, "LEVEL", literal("debug"), "fp-level"),
    ]);
    from["services"].as_array_mut().unwrap().push(jobs);
    let opts = Opts {
        from_marks: vec![
            format!("{API}:variables.PLAIN"),
            format!("{API}:healthcheck"),
            format!("{JOBS}:variables.MODE"),
        ],
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &from, &parent(), Way::Sync, opts);
    assert_has(
        &summary(&plan),
        &[
            format!("{API}:startCommand move conflict=false"),
            format!("{API}:variables.PLAIN differ never_synced"),
            format!("{API}:healthcheck differ never_synced"),
            format!("{JOBS}:node move conflict=false"),
            format!("{JOBS}:variables.MODE differ never_synced"),
            format!("{JOBS}:variables.LEVEL move conflict=false"),
        ],
    );

    // A marked row is never picked, and a node it arrives with lands without it.
    for marked in [
        format!("{API}:variables.PLAIN"),
        format!("{JOBS}:variables.MODE"),
    ] {
        let error = land(&plan, &[format!("{JOBS}:node"), marked]).unwrap_err();
        assert_eq!(error.message, "Change is meant to differ");
    }
    let next = next_of(
        &plan,
        &[format!("{JOBS}:node"), format!("{JOBS}:variables.LEVEL")],
    );
    let jobs = find(&next["services"], "lineageId", JOBS);
    assert_eq!(jobs["variables"].as_array().unwrap().len(), 1);
    assert_eq!(jobs["variables"][0]["key"], "LEVEL");
}

#[test]
fn a_new_service_arrives_without_its_never_synced_settings() {
    let mut from = branch();
    let mut jobs = jobs();
    jobs["config"]["startCommand"] = json!("jobs --test");
    jobs["config"]["healthcheck"] = json!({"type": "http", "path": "/up", "timeoutSeconds": 30});
    jobs["volumeAttachments"] =
        json!([{"volumeResourceId": from["volumes"][0]["resourceId"], "mountPath": "/jobs"}]);
    from["services"].as_array_mut().unwrap().push(jobs);
    let marks = vec![
        format!("{JOBS}:startCommand"),
        format!("{JOBS}:healthcheck"),
        format!("{JOBS}:mounts.{DATA}"),
    ];
    let opts = Opts {
        from_marks: marks.clone(),
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &from, &parent(), Way::Sync, opts);
    let mut expected = vec![format!("{JOBS}:node move conflict=false")];
    expected.extend(
        marks
            .iter()
            .map(|mark| format!("{mark} differ never_synced")),
    );
    assert_has(&summary(&plan), &expected);
    let next = next_of(&plan, &[format!("{JOBS}:node")]);
    let jobs = find(&next["services"], "lineageId", JOBS);
    assert_eq!(jobs["config"]["startCommand"], Value::Null);
    assert_eq!(jobs["config"]["healthcheck"], json!({"type": "none"}));
    assert_eq!(jobs["config"]["source"]["image"], "jobs:1");
    assert_eq!(jobs["volumeAttachments"], json!([]));
}

#[test]
fn a_mark_on_what_a_new_node_needs_keeps_the_node_out() {
    let mut from = branch();
    let mut jobs = jobs();
    jobs["config"]["startCommand"] = json!("jobs --test");
    from["services"].as_array_mut().unwrap().push(jobs);
    let opts = |mark: &str| Opts {
        from_marks: vec![format!("{JOBS}:{mark}")],
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &from, &parent(), Way::Sync, opts("source"));
    assert_has(
        &summary(&plan),
        &[
            format!("{JOBS}:node differ never_synced"),
            format!("{JOBS}:startCommand move conflict=false"),
        ],
    );
    let error = land(&plan, &[format!("{JOBS}:startCommand")]).unwrap_err();
    assert_eq!(error.message, "Pick the new node this change belongs to");
    // A new node arrives without custom domains anyway.
    let plan = compare_with(Some(&parent()), &from, &parent(), Way::Sync, opts("routes"));
    assert!(summary(&plan).contains(&format!("{JOBS}:node move conflict=false")));
}

#[test]
fn a_sync_into_a_branch_introduces_parent_services_with_its_naming() {
    let mut from = parent();
    let mut mail = image(0xb000_0000, 60, JOBS, "mail");
    mail["config"]["managedHostnames"] = json!([{"prefix": "mail", "targetPort": null}]);
    from["services"].as_array_mut().unwrap().push(mail);
    // As create returns it: the Parent without the nodes the Branch uses live.
    let mut base = parent();
    base["services"]
        .as_array_mut()
        .unwrap()
        .retain(|s| s["lineageId"] != WORKER);
    let sync = |live: Vec<&'static str>| {
        let opts = Opts {
            live,
            hostnames: Some(("", "-pr-7")),
            ..Opts::default()
        };
        compare_with(Some(&base), &from, &branch(), Way::Sync, opts)
    };
    let plan = sync(vec![WORKER]);
    let next = next_of(&plan, &[format!("{JOBS}:node")]);
    let mail = find(&next["services"], "lineageId", JOBS);
    assert_eq!(mail["config"]["managedHostnames"][0]["prefix"], "mail-pr-7");
    assert_has(
        &summary(&plan),
        &[
            format!("{CACHE}:node differ left_out"),
            format!("{WORKER}:node differ live"),
        ],
    );
    // A lineage no longer used live is offered as an introduction.
    let plan = sync(vec![]);
    assert!(summary(&plan).contains(&format!("{WORKER}:node move conflict=false")));
    let next = next_of(&plan, &[format!("{WORKER}:node")]);
    find(&next["services"], "lineageId", WORKER);
}

/// Every landing is undone by putting back what `into` held, and the base rewound by
/// putting back what it held, whatever the row: a setting, null, a secret, a mount or
/// variable removal, a Volume's storage.
#[test]
fn put_undoes_every_landing() {
    let mut shared = parent();
    svc(&mut shared, API)["config"]["startCommand"] = json!("start");
    let mut from = shared.clone();
    let api = svc(&mut from, API);
    api["config"]["preDeployCommand"] = json!("migrate");
    api["config"]["startCommand"] = json!(null);
    api["volumeAttachments"] = json!([]);
    api["variables"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v["key"] != "PLAIN");
    api["variables"].as_array_mut().unwrap().push(variable(
        0xb000_0000,
        13,
        "NEW",
        literal("n"),
        "fp-n",
    ));
    var(&mut from, API, "TOKEN")["value"] = secret("rotated-cipher");
    var(&mut from, API, "TOKEN")["valueFingerprint"] = json!("fp-rotated");
    svc(&mut from, WEB)["config"]["healthcheck"] =
        json!({"type": "http", "path": "/up", "timeoutSeconds": 5});
    from["volumes"][0]["storage"] = json!({"kind":"provisioned","maximumBytes":1_000_000_000_i64});
    let mut into = shared.clone();
    svc(&mut into, API)["config"]["replicas"] = json!(2);

    let plan = compare(Some(&shared), &from, &into, Way::Copy);
    let picks = moves(&plan);
    assert_eq!(picks.len(), 8, "{picks:#?}");
    let applied = land(&plan, &picks).unwrap();
    assert_eq!(applied.landed.len(), picks.len());
    let next = unapply(&applied.next, "", &applied.landed).unwrap();
    let mut base = applied.base;
    for landed in applied.landed.iter().rev() {
        base = put_back(&base, &landed.row, &landed.prior).unwrap();
    }
    let unchanged = |a: &SavedEnvironmentIntent, b: &Value| {
        let rows = moves(&compare(None, &json_of(a), b, Way::Copy));
        assert!(rows.is_empty(), "{rows:#?}");
    };
    unchanged(&next, &into);
    unchanged(&base, &json_of(&redact_environment_intent(intent(&shared))));
    let mut next = json_of(&next);
    assert_eq!(
        var(&mut next, API, "TOKEN")["value"]["encryptedValue"]["ciphertext"],
        "parent-cipher"
    );
    assert_eq!(
        svc(&mut next, API)["volumeAttachments"][0]["mountPath"],
        "/data"
    );
    assert_eq!(svc(&mut next, API)["config"]["startCommand"], "start");
    assert_eq!(svc(&mut next, API)["config"]["replicas"], 2);
}

/// Undo takes a new node whole, so it is refused once the node holds anything it
/// didn't land with, even a custom domain, which no new node arrives with.
#[test]
fn a_new_node_is_not_undone_once_it_holds_a_row_it_didnt_land_with() {
    let mut from = parent();
    from["services"].as_array_mut().unwrap().push(jobs());
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    let applied = land(&plan, &[format!("{JOBS}:node")]).unwrap();
    unapply(&applied.next, "", &applied.landed).unwrap();
    let mut next = json_of(&applied.next);
    svc(&mut next, JOBS)["config"]["routes"] =
        json!([{"id": id(0xd000_0000, 1), "hostname": "jobs.example.com", "targetPort": null}]);
    let refused = unapply(&intent(&next), "", &applied.landed).unwrap_err();
    assert_eq!(
        refused.to_string(),
        format!("{JOBS}:routes changed since it landed")
    );
}

/// A source minus its git branch, as its row holds it.
fn source_of(env: &mut Value, lineage: &str) -> Value {
    let mut source = svc(env, lineage)["config"]["source"].clone();
    source.as_object_mut().unwrap().remove("branch");
    source
}

/// Switching a source's kind is one row: Undo puts the source back, and is refused
/// once it changed since.
#[test]
fn undoing_a_source_switch_puts_the_source_back() {
    let mut from = parent();
    svc(&mut from, WEB)["config"]["source"] =
        json!({"version": 1, "type": "image", "image": "web:9", "credentials": {"type": "none"}});
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    let row = format!("{WEB}:source");
    assert_eq!(moves(&plan), std::slice::from_ref(&row));
    let applied = land(&plan, std::slice::from_ref(&row)).unwrap();
    let mut undone = json_of(&unapply(&applied.next, "", &applied.landed).unwrap());
    // Its git branch too, which the Sync never moved.
    assert_eq!(
        svc(&mut undone, WEB)["config"]["source"],
        svc(&mut parent(), WEB)["config"]["source"]
    );

    let mut next = json_of(&applied.next);
    svc(&mut next, WEB)["config"]["source"]["credentials"] =
        json!({"type": "configured", "credentialId": id(0xd000_0000, 1)});
    let refused = unapply(&intent(&next), "", &applied.landed).unwrap_err();
    assert_eq!(
        refused.to_string(),
        format!("{row} changed since it landed")
    );
}

/// A discarded Follow that switched a source's kind puts the base's source back.
#[test]
fn a_discarded_source_switch_rewinds_the_base() {
    let mut from = parent();
    svc(&mut from, WEB)["config"]["source"] =
        json!({"version": 1, "type": "image", "image": "web:9", "credentials": {"type": "none"}});
    let plan = compare(Some(&parent()), &from, &parent(), Way::Follow);
    let landed = land(&plan, &moves(&plan)).unwrap();
    assert_eq!(
        source_of(&mut json_of(&landed.base), WEB),
        source_of(&mut from, WEB)
    );
    let mut base = landed.base.clone();
    for one in &landed.landed {
        base = put_back(&base, &one.row, &one.prior).unwrap();
    }
    assert_eq!(
        source_of(&mut json_of(&base), WEB),
        source_of(&mut parent(), WEB)
    );
}

/// The git branch is each side's own: a source landing on a git source keeps its branch.
#[test]
fn a_branch_keeps_its_git_branch_when_a_source_lands() {
    let mut from = parent();
    let source = &mut svc(&mut from, WEB)["config"]["source"];
    source["repository"] = json!("acme/docs");
    source["repositoryId"] = json!(2);
    source["branch"] = json!({"type": "connected", "name": "fix"});
    let plan = compare(Some(&parent()), &from, &parent(), Way::Sync);
    let row = format!("{WEB}:source");
    assert_eq!(moves(&plan), std::slice::from_ref(&row));
    let mut next = next_of(&plan, &[row]);
    let source = &svc(&mut next, WEB)["config"]["source"];
    assert_eq!(source["repository"], "acme/docs");
    assert_eq!(
        source["branch"],
        json!({"type": "connected", "name": "main"})
    );
}

#[test]
fn put_refuses_what_a_row_cannot_hold() {
    let parent = intent(&parent());
    let at = |row: String| row.parse::<RowId>().unwrap();
    let start = at(format!("{API}:startCommand"));
    let set = put(&parent, &start, &val(json!("x"))).unwrap();
    assert_eq!(set.services[0].config.start_command.as_deref(), Some("x"));
    let unset = put(&set, &start, &Cell::Absent).unwrap();
    assert_eq!(unset.services[0].config.start_command, None);
    let removed = put(&parent, &at(format!("{API}:node")), &Cell::Absent).unwrap();
    assert!(removed.services.iter().all(|s| s.lineage_id != API));
    // Clearing on a node that isn't there is already done.
    put(&parent, &at(format!("{JOBS}:startCommand")), &Cell::Absent).unwrap();
    for (row, cell) in [
        (format!("{JOBS}:startCommand"), val(json!("x"))),
        (format!("{JOBS}:node"), val(json!("jobs"))),
        (format!("{DATA}:data"), Cell::Absent),
        (format!("{API}:routes"), Cell::Absent),
        (format!("{API}:managedHostnames"), Cell::Absent),
        (format!("{API}:replicas"), val(json!("many"))),
        (format!("{API}:mounts.{JOBS}"), val(json!("/x"))),
        (format!("{API}:variables.X"), val(json!("bare"))),
    ] {
        assert!(put(&parent, &at(row.clone()), &cell).is_err(), "{row}");
    }
    // Putting a base back refuses the same, but leaves a row whose node is gone.
    let back = |row: String, cell| put_back(&parent, &at(row), &cell);
    assert!(back(format!("{API}:replicas"), val(json!("many"))).is_err());
    assert_eq!(
        back(format!("{JOBS}:node"), val(json!("jobs"))).unwrap(),
        parent
    );
    let mount = back(format!("{API}:mounts.{JOBS}"), val(json!("/x")));
    assert_eq!(mount.unwrap(), parent);
}

#[test]
fn name_of_names_a_mount_by_its_volume() {
    let parent = intent(&parent());
    let at = |row: String| row.parse::<RowId>().unwrap();
    let (node, place) = name_of(&parent, &at(format!("{API}:mounts.{DATA}"))).unwrap();
    assert!(matches!(node, NodeRef::Service(service) if service.slug == "api"));
    assert_eq!(place, "mounts.data");
    let (node, place) = name_of(&parent, &at(format!("{DATA}:storage"))).unwrap();
    assert!(matches!(node, NodeRef::Volume(volume) if volume.name == "data"));
    assert_eq!(place, "storage");
    assert!(name_of(&parent, &at(format!("{JOBS}:node"))).is_none());
}

#[test]
fn a_cell_reads_as_the_plan_reads_it() {
    let plan = compare(Some(&parent()), &branch(), &parent(), Way::Sync);
    let into = Cells::of(&intent(&parent()), "");
    for row in plan.rows() {
        assert_eq!(*into.at(&row.id), row.into, "{}", row.id);
    }
    let secret = into.at(&format!("{API}:variables.TOKEN").parse().unwrap());
    assert!(secret.is_secret(), "{secret:?}");
    assert!(!Cell::Absent.is_secret());
}

fn ticked(plan: &Plan, id: &str) -> bool {
    match row(plan, id).verdict {
        Verdict::Moves { ticked, .. } => ticked,
        Verdict::Differs(why) => panic!("{id} differs: {why:?}"),
    }
}

/// A proposal compares against the source cells it last included, not the pair base.
#[test]
fn accepted_cells_replace_the_pair_base() {
    let start = format!("{API}:startCommand");
    let mut from = branch();
    svc(&mut from, API)["config"]["startCommand"] = json!("one");
    let mut into = parent();
    svc(&mut into, API)["config"]["startCommand"] = json!("one");
    // The pair base never saw `one`: on its own, `from` = `into` and nothing moves.
    let plain = compare(Some(&parent()), &from, &into, Way::Sync);
    assert!(plain.rows().iter().all(|r| r.id.to_string() != start));
    // Accepted at `one`, the source change to `two` is offered against it.
    svc(&mut from, API)["config"]["startCommand"] = json!("two");
    let opts = Opts {
        accepted: vec![(start.clone(), val(json!("one")))],
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &from, &into, Way::Sync, opts);
    let moved = row(&plan, &start);
    assert_eq!(
        (&moved.base, &moved.from, &moved.into),
        (&val(json!("one")), &val(json!("two")), &val(json!("one")))
    );
    assert_eq!(
        moved.verdict,
        Verdict::Moves {
            conflict: false,
            ticked: true,
            arrives: Arrives::AsIs
        }
    );
    // What the pair base holds stays the prior: accepted is a base override only.
    let applied = land(&plan, std::slice::from_ref(&start)).unwrap();
    assert_eq!(applied.landed[0].prior, val(json!("one")));
}

/// A row another proposal holds is disclosed as included, even when both bring the
/// same value, and only when it is the sender's own change.
#[test]
fn held_row_differs_as_included_even_when_equal() {
    let start = format!("{API}:startCommand");
    let pre = format!("{API}:preDeployCommand");
    let mut from = branch();
    svc(&mut from, API)["config"]["startCommand"] = json!("one");
    svc(&mut from, API)["config"]["preDeployCommand"] = json!("two");
    let mut into = parent();
    svc(&mut into, API)["config"]["startCommand"] = json!("one");
    svc(&mut into, API)["config"]["preDeployCommand"] = json!("other");
    let opts = Opts {
        held: vec![start.clone(), pre.clone()],
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &from, &into, Way::Sync, opts);
    assert_eq!(row(&plan, &start).verdict, Verdict::Differs(Why::Included));
    assert_eq!(row(&plan, &pre).verdict, Verdict::Differs(Why::Included));
    // Not the sender's own: an inherited row is not disclosed.
    let opts = Opts {
        held: vec![start.clone()],
        own: Some(Vec::new()),
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &from, &into, Way::Sync, opts);
    assert!(plan.rows().iter().all(|r| r.id.to_string() != start));
    // A row the sender left at the base is not disclosed either.
    let opts = Opts {
        held: vec![start.clone()],
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &parent(), &into, Way::Sync, opts);
    assert!(plan.rows().iter().all(|r| r.id.to_string() != start));
}

#[test]
fn pick_of_included_row_is_refused() {
    let start = format!("{API}:startCommand");
    let mut from = branch();
    svc(&mut from, API)["config"]["startCommand"] = json!("two");
    let opts = Opts {
        held: vec![start.clone()],
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &from, &parent(), Way::Sync, opts);
    let error = land(&plan, &[start]).unwrap_err();
    assert_eq!(
        (error.path.as_str(), error.message.as_str()),
        ("picks", "Change is meant to differ")
    );
}

/// A Sync never overwrites the receiver's own change by default; a Follow still does.
#[test]
fn sync_conflicts_start_unticked_follow_unchanged() {
    let start = format!("{API}:startCommand");
    let pre = format!("{API}:preDeployCommand");
    let mut from = branch();
    svc(&mut from, API)["config"]["startCommand"] = json!("from-start");
    svc(&mut from, API)["config"]["preDeployCommand"] = json!("from-pre");
    let mut into = parent();
    svc(&mut into, API)["config"]["startCommand"] = json!("into-start");
    let sync = compare(Some(&parent()), &from, &into, Way::Sync);
    assert!(!ticked(&sync, &start));
    assert!(ticked(&sync, &pre));
    // Unticked, a conflict can still be picked.
    let next = next_of(&sync, std::slice::from_ref(&start));
    assert_eq!(
        find(&next["services"], "lineageId", API)["config"]["startCommand"],
        "from-start"
    );
    let follow = compare(Some(&parent()), &from, &into, Way::Follow);
    assert!(ticked(&follow, &start));
    assert!(ticked(&follow, &pre));
}

#[test]
fn dependents_lists_mounts_and_config_mounts() {
    let config = "a0000000-0000-4000-8000-000000000007";
    let mut env = parent();
    env["configs"] = json!([{
        "resourceId": id(0xb000_0000, 60), "resourceLineageId": config, "name": "app",
        "files": {"app.conf": {"content": [
            {"kind": "text", "value": "upstream="},
            {"kind": "ref", "owner": {"scope": "service", "lineageId": WORKER}, "key": "URL"}],
            "mode": "0444", "uid": 0, "gid": 0}}}]);
    svc(&mut env, WEB)["configAttachments"] =
        json!([{"configResourceId": id(0xb000_0000, 60), "mountDir": "/etc/app"}]);
    let env = intent(&env);
    let show = |lineage| {
        dependents(&env, lineage)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(show(DATA), [format!("{API}:mounts.{DATA}")]);
    assert_eq!(show(config), [format!("{WEB}:configs.{config}")]);
    let mut worker = vec![
        format!("{API}:variables.WORKER_URL"),
        format!("{config}:files.app.conf"),
    ];
    worker.sort();
    assert_eq!(show(WORKER), worker);
    // A node's own rows are not its dependents; nothing uses cache.
    assert!(show(API).is_empty());
    assert!(show(CACHE).is_empty());
}

/// Remove of a proposal that introduced a node inverts it whole, so a child the
/// destination added since is a refusal, never a silent loss.
#[test]
fn unapply_refuses_introduced_node_with_unowned_child() {
    let mut from = parent();
    from["services"].as_array_mut().unwrap().push(jobs());
    let opts = Opts {
        accepted: vec![(format!("{JOBS}:node"), Cell::Absent)],
        ..Opts::default()
    };
    let plan = compare_with(Some(&parent()), &from, &parent(), Way::Sync, opts);
    let applied = land(&plan, &[format!("{JOBS}:node")]).unwrap();
    assert_eq!(
        json_of(&unapply(&applied.next, "", &applied.landed).unwrap()),
        json_of(&intent(&parent()))
    );
    let mut next = json_of(&applied.next);
    svc(&mut next, JOBS)["variables"] =
        json!([variable(0xd000_0000, 1, "LOCAL", literal("x"), "fp-local")]);
    let refused = unapply(&intent(&next), "", &applied.landed).unwrap_err();
    assert_eq!(
        refused.to_string(),
        format!("{JOBS}:variables.LOCAL changed since it landed")
    );
}
