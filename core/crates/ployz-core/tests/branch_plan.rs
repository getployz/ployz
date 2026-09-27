#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::{ConfigError, config_request};
use serde_json::{Value, json};

fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

// Lineages: web=1 uses api=2 (variable) and mounts uploads=10; api uses db=3; db mounts data=11;
// web also uses 99, a lineage the Parent itself uses live.
fn service(n: u32, slug: &str, refs: &[u32], mounts: &[u32]) -> Value {
    let variables: Vec<Value> = refs
        .iter()
        .enumerate()
        .map(|(i, target)| {
            json!({"id":id(n * 1000 + 500 + u32::try_from(i).unwrap()),"key":format!("REF_{i}"),"description":null,
                "exported":false,"valueFingerprint":"f","value":{"kind":"template","parts":[
                {"kind":"ref","owner":{"scope":"service","lineageId":id(*target)},"key":"HOST"}]}})
        })
        .collect();
    let attachments: Vec<Value> = mounts
        .iter()
        .map(|v| json!({"volumeResourceId":id(v + 100),"mountPath":format!("/data/{v}")}))
        .collect();
    json!({"id":id(n + 100),"lineageId":id(n),"slug":slug,
        "config":{"version":2,"privateDns":slug,"source":{"version":1,"type":"image","image":"nginx:stable","credentials":{"type":"none"}},"preDeployCommand":null,"startCommand":null,"healthcheck":{"type":"none"},"restartPolicy":"unless-stopped"},
        "variables":variables,"volumeAttachments":attachments})
}

fn parent() -> Value {
    json!({"version":1,"environmentSlug":"production",
        "services":[service(1,"web",&[2,99],&[10]),service(2,"api",&[3],&[]),service(3,"db",&[],&[11]),service(4,"cron",&[],&[])],
        "volumes":[{"resourceId":id(110),"resourceLineageId":id(10),"name":"uploads"},
                   {"resourceId":id(111),"resourceLineageId":id(11),"name":"data"}]})
}

fn plan(deployed: &[u32], focus: &[u32], picks: Value) -> Result<Value, ConfigError> {
    let ids = |ns: &[u32]| ns.iter().map(|n| id(*n)).collect::<Vec<_>>();
    config_request(json!({"operation":"plan_branch","parent":parent(),
        "deployed":ids(deployed),"focus":ids(focus),"picks":picks}))
}

fn own(ns: &[u32]) -> Value {
    json!({"own": ns.iter().map(|n| id(*n)).collect::<Vec<_>>()})
}

/// Each node as (lineage n, role, because) in plan order.
fn roles(plan: &Value) -> Vec<(u32, String, Value)> {
    plan["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| {
            let lineage = node["lineageId"].as_str().unwrap();
            let n = lineage.rsplit('-').next().unwrap().parse().unwrap();
            (
                n,
                node["role"].as_str().unwrap().into(),
                node["because"].clone(),
            )
        })
        .collect()
}

fn row(n: u32, role: &str, because: Option<&str>) -> (u32, String, Value) {
    (n, role.into(), json!(because))
}

const EVERYTHING: [u32; 7] = [1, 2, 3, 4, 10, 11, 99];

#[test]
fn own_copies_use_deployed_nodes_live_and_leave_the_rest_out() {
    let result = plan(&EVERYTHING, &[1], own(&[1])).unwrap();
    assert_eq!(
        roles(&result),
        vec![
            row(1, "own", Some("picked")),
            row(2, "live", Some("used")),
            row(3, "left_out", None),
            row(4, "left_out", None),
            row(10, "own", Some("used")),
            row(11, "left_out", None),
            row(99, "live", Some("used")),
        ]
    );
    assert_eq!(result["nodes"][0]["nodeType"], "service");
    assert_eq!(result["nodes"][4]["nodeType"], "volume");
    assert_eq!(result["nodes"][6]["nodeType"], "service");
}

#[test]
fn undeployed_uses_are_copied_transitively() {
    // The Parent runs nothing: whatever the picks use is copied, all the way down.
    let result = plan(&[], &[1], own(&[1])).unwrap();
    assert_eq!(
        roles(&result),
        vec![
            row(1, "own", Some("picked")),
            row(2, "own", Some("parent_not_deployed")),
            row(3, "own", Some("parent_not_deployed")),
            row(4, "left_out", None),
            row(10, "own", Some("used")),
            row(11, "own", Some("used")),
            row(99, "live", Some("used")),
        ]
    );
    // Lineages the Parent no longer has are ignored; a deployed api stops the walk.
    let result = plan(&[2, 42], &[1], own(&[1])).unwrap();
    assert_eq!(roles(&result)[1], row(2, "live", Some("used")));
    assert_eq!(roles(&result)[2], row(3, "left_out", None));
}

#[test]
fn presets_round_trip_and_hand_picks_report_null() {
    for (preset, owned) in [
        ("only", vec![1]),
        ("uses", vec![1, 2, 3, 10, 11]),
        ("all", vec![1, 2, 3, 4, 10, 11]),
    ] {
        let result = plan(&EVERYTHING, &[1], json!({"preset":preset})).unwrap();
        assert_eq!(result["preset"], preset);
        let picked: Vec<u32> = roles(&result)
            .into_iter()
            .filter(|(_, _, because)| because == "picked")
            .map(|(n, _, _)| n)
            .collect();
        assert_eq!(picked, owned, "{preset}");
        assert_eq!(
            plan(&EVERYTHING, &[1], own(&owned)).unwrap()["preset"],
            preset
        );
    }
    // cron uses nothing: "uses" picks the same as "only", so the earlier preset wins.
    let result = plan(&EVERYTHING, &[4], json!({"preset":"uses"})).unwrap();
    assert_eq!(result["preset"], "only");
    assert_eq!(
        plan(&EVERYTHING, &[1], own(&[1, 4])).unwrap()["preset"],
        Value::Null
    );
}

#[test]
fn unknown_lineages_and_invalid_parents_are_refused() {
    assert_eq!(plan(&[], &[42], own(&[])).unwrap_err().path, "focus");
    assert_eq!(plan(&[], &[1], own(&[42])).unwrap_err().path, "picks.own");
    // A lineage the Parent only uses live has no configuration to copy.
    assert_eq!(plan(&[], &[1], own(&[99])).unwrap_err().path, "picks.own");
    let error = config_request(json!({"operation":"plan_branch","parent":{"version":2},
        "deployed":[],"focus":[],"picks":{"preset":"all"}}))
    .unwrap_err();
    assert_eq!(error.path, "environment");
}

#[test]
fn the_name_check_follows_the_project_name_rule() {
    let check = |name: &str| config_request(json!({"operation":"check_branch_name","name":name}));
    assert_eq!(check("shop-pr-12").unwrap(), json!("shop-pr-12"));
    for (name, message) in [
        ("", "Project name is empty"),
        (&"a".repeat(64), "Project name is longer than 63 characters"),
        ("Shop_PR", "Project name must be a lowercase DNS label"),
        ("-shop", "Project name must be a lowercase DNS label"),
        (
            "ployz-system",
            "Project name is reserved for the system Project",
        ),
    ] {
        let error = check(name).unwrap_err();
        assert_eq!(
            (error.path.as_str(), error.message.as_str()),
            ("projectName", message)
        );
    }
}
