#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! What a Saved Environment says about who mounts and who reads what, pinned through the
//! writer, the removal queries and the Branch plan, so one vocabulary can answer them all.

use ployz_core::config::{
    At, BranchPicks, BranchPreset, Cell, ConfigError, RowId, SavedEnvironmentIntent, dependents,
    parse_environment_intent, plan_branch, put, referenced_by_others,
};
use serde_json::{Value, json};

const API: &str = "a0000000-0000-4000-8000-000000000001";
const WEB: &str = "a0000000-0000-4000-8000-000000000002";
const DB: &str = "a0000000-0000-4000-8000-000000000003";
const DATA: &str = "a0000000-0000-4000-8000-000000000004";
const ETC: &str = "a0000000-0000-4000-8000-000000000005";
const DATA_ID: &str = "b0000000-0000-4000-8000-000000000001";
const ETC_ID: &str = "b0000000-0000-4000-8000-000000000002";

fn service(n: u32, lineage: &str, slug: &str) -> Value {
    json!({"id": format!("c0000000-0000-4000-8000-{n:012x}"), "lineageId": lineage, "slug": slug,
        "variables": [], "volumeAttachments": [],
        "config": {"version": 2, "privateDns": slug,
            "source": {"version": 1, "type": "image", "image": "x:1", "credentials": {"type": "none"}},
            "preDeployCommand": null, "startCommand": null, "healthcheck": {"type": "none"},
            "restartPolicy": "unless-stopped"}})
}

fn reads(lineage: &str) -> Value {
    json!({"kind": "template", "parts": [
        {"kind": "ref", "owner": {"scope": "service", "lineageId": lineage}, "key": "URL"}]})
}

fn variable(n: u32, key: &str, value: Value) -> Value {
    json!({"id": format!("d0000000-0000-4000-8000-{n:012x}"), "key": key, "description": null,
        "exported": false, "valueFingerprint": "fp", "value": value})
}

/// `api` mounts a Volume and a Config and reads `db` and itself; `web` reads only the
/// Config's own reference of `db`; a Config reads `api`.
fn environment() -> SavedEnvironmentIntent {
    let mut api = service(1, API, "api");
    api["variables"] = json!([
        variable(1, "DB", reads(DB)),
        variable(2, "SELF", reads(API)),
    ]);
    api["volumeAttachments"] = json!([{"volumeResourceId": DATA_ID, "mountPath": "/data"}]);
    api["configAttachments"] = json!([{"configResourceId": ETC_ID, "mountDir": "/etc/app"}]);
    let mut web = service(2, WEB, "web");
    web["configAttachments"] = json!([{"configResourceId": ETC_ID, "mountDir": "/etc/web"}]);
    parse_environment_intent(json!({
        "version": 1, "environmentSlug": "production",
        "services": [api, web, service(3, DB, "db")],
        "volumes": [{"resourceId": DATA_ID, "resourceLineageId": DATA, "name": "data",
            "storage": {"kind": "docker"}}],
        "configs": [{"resourceId": ETC_ID, "resourceLineageId": ETC, "name": "app",
            "files": {"app.conf": {"content": [
                {"kind": "text", "value": "u="},
                {"kind": "ref", "owner": {"scope": "service", "lineageId": API}, "key": "URL"}],
                "mode": "0444", "uid": 0, "gid": 0}}}]
    }))
    .unwrap()
}

fn json_of(intent: &SavedEnvironmentIntent) -> Value {
    serde_json::to_value(intent).unwrap()
}

fn put_mount(at: At, lineage: &str, cell: &Cell) -> Result<Value, (String, String)> {
    put(&environment(), &RowId::of(lineage, at), cell)
        .map(|env| {
            let api = &json_of(&env)["services"][0];
            json!([api["volumeAttachments"], api["configAttachments"]])
        })
        .map_err(|ConfigError { path, message }| (path, message))
}

#[test]
fn putting_a_mount_replaces_the_mounts_of_its_resource_and_refuses_what_it_cannot_hold() {
    let volume = |cell: &Cell| put_mount(At::Mount(DATA.into()), API, cell);
    let config = |cell: &Cell| put_mount(At::ConfigMount(ETC.into()), API, cell);
    let held = |volumes: Value, configs: Value| Ok(json!([volumes, configs]));
    let data = |path: &str| json!([{"volumeResourceId": DATA_ID, "mountPath": path}]);
    let etc = |dir: &str| json!([{"configResourceId": ETC_ID, "mountDir": dir}]);

    assert_eq!(
        volume(&Cell::Value(json!("/moved"))),
        held(data("/moved"), etc("/etc/app"))
    );
    assert_eq!(volume(&Cell::Absent), held(json!([]), etc("/etc/app")));
    assert_eq!(
        config(&Cell::Value(json!("/etc/moved"))),
        held(data("/data"), etc("/etc/moved"))
    );
    assert_eq!(config(&Cell::Absent), held(data("/data"), json!(null)));

    let refused = |path: &str, message: &str| Err((path.to_owned(), message.to_owned()));
    for (result, message) in [
        (volume(&Cell::Value(json!(7))), "Not a mount path"),
        (volume(&Cell::SecretWithoutValue), "Not a mount path"),
        (config(&Cell::Value(json!(7))), "Not a mount directory"),
        (config(&Cell::SecretWithoutValue), "Not a mount directory"),
    ] {
        let at = if message.ends_with("path") {
            format!("{API}:mounts.{DATA}")
        } else {
            format!("{API}:configs.{ETC}")
        };
        assert_eq!(result, refused(&at, message));
    }

    let gone = "a0000000-0000-4000-8000-0000000000ff";
    assert_eq!(
        put_mount(At::Mount(gone.into()), API, &Cell::Value(json!("/x"))),
        refused(
            "picks",
            "Mounted Volume is not in the receiving configuration"
        )
    );
    assert_eq!(
        put_mount(At::ConfigMount(gone.into()), API, &Cell::Value(json!("/x"))),
        refused(
            "picks",
            "Mounted Config is not in the receiving configuration"
        )
    );
    assert_eq!(
        put_mount(At::Mount(gone.into()), API, &Cell::Absent),
        volume(&Cell::Absent).map(|_| json!([data("/data"), etc("/etc/app")]))
    );
    assert_eq!(
        put_mount(At::Mount(DATA.into()), gone, &Cell::Value(json!("/x"))),
        refused(
            &format!("{gone}:mounts.{DATA}"),
            "The node isn't in this configuration"
        )
    );
    assert_eq!(
        put_mount(At::ConfigMount(ETC.into()), gone, &Cell::Absent),
        Ok(json!([data("/data"), etc("/etc/app")]))
    );
}

#[test]
fn a_node_reads_or_mounts_another_by_lineage() {
    let env = environment();
    // api and the Config both read: a Service's own reading of itself counts as a read.
    assert!(referenced_by_others(&env, API));
    assert!(referenced_by_others(&env, DB));
    assert!(!referenced_by_others(&env, WEB));
    assert!(!referenced_by_others(&env, DATA));
    let show = |lineage| {
        dependents(&env, lineage)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    // Only mounts hold a node; a read leaves a broken reference, not a dependent.
    assert_eq!(show(API), [] as [String; 0]);
    assert_eq!(show(WEB), [] as [String; 0]);
    assert_eq!(show(DB), [] as [String; 0]);
    assert_eq!(show(DATA), [format!("{API}:mounts.{DATA}")]);
    assert_eq!(
        show(ETC),
        [
            format!("{API}:configs.{ETC}"),
            format!("{WEB}:configs.{ETC}")
        ]
    );
}

#[test]
fn a_branch_that_uses_what_its_picks_use_carries_mounts_and_reads_along() {
    let roles = |focus: &str| -> Vec<(String, String)> {
        let plan = plan_branch(
            &environment(),
            &[],
            &[focus.to_owned()],
            &BranchPicks::Preset {
                preset: BranchPreset::Uses,
            },
        )
        .unwrap();
        let value = serde_json::to_value(plan).unwrap();
        value["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| {
                (
                    n["lineageId"].as_str().unwrap().to_owned(),
                    n["role"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let expect = |own: &[&str]| -> Vec<(String, String)> {
        [API, WEB, DB, DATA, ETC]
            .iter()
            .map(|id| {
                let role = if own.contains(id) { "own" } else { "left_out" };
                ((*id).to_owned(), role.to_owned())
            })
            .collect()
    };
    // api reads db, mounts the Volume and the Config, and the Config reads api back.
    assert_eq!(roles(API), expect(&[API, DB, DATA, ETC]));
    // web mounts only the Config, which reads api, which reads db and mounts the Volume.
    assert_eq!(roles(WEB), expect(&[WEB, API, DB, DATA, ETC]));
    assert_eq!(roles(DB), expect(&[DB]));
}
