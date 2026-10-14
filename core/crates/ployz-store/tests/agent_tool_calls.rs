//! Agent tool calls driven to the end against a SQLite Config Store, the way a
//! host drives them: every step's asks answered in order, the one write run.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

use std::collections::BTreeMap;
use std::time::Duration;

use ployz_core::{
    BuildConcurrencyUpdate, DeployOutcome, DeployPreview, MachineLogService, ServiceName,
};
use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::tools::{
    Act, Action, Ask, Call, CloudQuery, Fix, Gate, INSTRUCTIONS, MetricsQuery, MetricsSince,
    MetricsSubject, Place, Refusal, STEP_BUDGET, Step, StoreAt, Timestamp, ToolResult, VolumeRun,
    WAIT_CAP_SECONDS, Write, Writes, catalog, example, lint, resume, start,
};
use ployz_store::{
    Actor, Admit, ConfigStore, ConfigsQuery, CreateEnvironment, CreateProject, CreateService,
    CreateVolume, Deploy, DeploymentId, EnvironmentId, EnvironmentName, EnvironmentRef,
    EnvironmentsQuery, NamespacesQuery, OrganizationId, ProjectId, ProjectName, RowTracker,
    RunEvidence, RunnerId, SealingKey, ServiceLineageId, VolumeId, VolumeName,
};
use serde_json::{Value, json};

fn who() -> Actor {
    Actor::system(OrganizationId::parse("org").unwrap())
}

fn shop() -> ConfigStore {
    let store = ConfigStore::open(
        "sqlite::memory:",
        SealingKey::new(b"test-encryption-secret").unwrap(),
    )
    .unwrap();
    store
        .write(
            &who(),
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
}

fn in_org() -> StoreAt {
    StoreAt::Cloud {
        organization: OrganizationId::parse("org").unwrap(),
    }
}

fn place(store: StoreAt) -> Place {
    Place {
        project: Some(ProjectName::parse("shop").unwrap()),
        environment: None,
        store,
    }
}

/// What one call did on its way to its result.
struct Run {
    result: ToolResult,
    reads: u8,
    writes: u8,
    waited_ms: u32,
}

impl Run {
    fn ok(self) -> Value {
        match self.result {
            ToolResult::Ok(value) => value,
            ToolResult::Refused(refusal) => panic!("refused: {refusal:?}"),
        }
    }

    fn refused(self) -> Refusal {
        match self.result {
            ToolResult::Refused(refusal) => *refusal,
            ToolResult::Ok(value) => panic!("answered: {value}"),
        }
    }
}

/// What a call asks of Ployz Cloud: a read or its one act.
enum Cloud {
    Ask(CloudQuery),
    Act(Act),
}

fn no_cloud(cloud: Cloud) -> Result<Value, RpcError> {
    match cloud {
        Cloud::Ask(query) => panic!("no Cloud in this test: {query:?}"),
        Cloud::Act(act) => panic!("no Cloud in this test: {act:?}"),
    }
}

fn answer(
    store: &ConfigStore,
    who: &Actor,
    cloud: &mut dyn FnMut(Cloud) -> Result<Value, RpcError>,
    ask: Ask,
) -> Result<Value, RpcError> {
    match ask {
        Ask::Store(query) => store
            .read(who, &query)
            .map(|view| serde_json::to_value(view).unwrap()),
        Ask::Cloud(query) => cloud(Cloud::Ask(query)),
    }
}

/// Drive `tool` from `at` to its result, Cloud answered by `cloud`, holding every
/// call to the step machine's budget.
fn drive(
    store: &ConfigStore,
    at: StoreAt,
    tool: &str,
    input: Value,
    call_key: &str,
    cloud: &mut dyn FnMut(Cloud) -> Result<Value, RpcError>,
) -> Run {
    drive_as(store, &who(), at, tool, input, call_key, cloud)
}

fn drive_as(
    store: &ConfigStore,
    who: &Actor,
    at: StoreAt,
    tool: &str,
    input: Value,
    call_key: &str,
    cloud: &mut dyn FnMut(Cloud) -> Result<Value, RpcError>,
) -> Run {
    let (mut step, mut state) = start(tool, input, &place(at), call_key);
    let (mut reads, mut writes, mut waited_ms) = (0, 0, 0);
    let result = loop {
        let answers = match step {
            Step::Read { asks } => {
                reads += 1;
                asks.into_iter()
                    .map(|ask| answer(store, who, cloud, ask))
                    .collect()
            }
            Step::Wait { ms, then } => {
                waited_ms += ms;
                then.into_iter()
                    .map(|ask| answer(store, who, cloud, ask))
                    .collect()
            }
            Step::Write { write } => {
                writes += 1;
                match *write {
                    Write::Store(command) => vec![
                        store
                            .write(who, &command)
                            .map(|written| serde_json::to_value(written).unwrap()),
                    ],
                    Write::Act(act) => vec![cloud(Cloud::Act(act))],
                }
            }
            Step::Done { result } => break result,
        };
        (step, state) = resume(state, answers);
    };
    assert!(
        reads + writes <= STEP_BUDGET,
        "{tool} took {reads} reads and {writes} writes"
    );
    assert!(writes <= 1, "{tool} wrote {writes} times");
    assert!(
        waited_ms <= WAIT_CAP_SECONDS * 1_000,
        "{tool} waited {waited_ms} ms"
    );
    Run {
        result,
        reads,
        writes,
        waited_ms,
    }
}

/// Drive `tool` against the Store alone; a refusal retries the same tool.
fn call(store: &ConfigStore, tool: &str, input: Value, call_key: &str) -> Run {
    let run = drive(
        store,
        in_org(),
        tool,
        input.clone(),
        call_key,
        &mut no_cloud,
    );
    if let ToolResult::Refused(refusal) = &run.result {
        assert_eq!(refusal.retry.tool, tool, "a retry calls the same tool");
        if refusal.code != RpcErrorCode::ConfirmationRequired && refusal.writes != Writes::Unknown {
            assert_eq!(refusal.retry.input, input, "a retry repeats the input");
        }
    }
    run
}

#[test]
fn the_catalog_has_the_agreed_tools() {
    let names = catalog().iter().map(|def| def.name).collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "docs",
            "environment_show",
            "project_create",
            "project_rename",
            "service_inspect",
            "service_add",
            "service_update",
            "service_rename",
            "service_remove",
            "domain_check",
            "domain_add",
            "domain_update",
            "domain_remove",
            "volume_add",
            "volume_update",
            "volume_remove",
            "config_write",
            "config_rename",
            "config_remove",
            "review",
            "discard",
            "history",
            "publish",
            "deploy",
            "deployment_list",
            "deployment_show",
            "deployment_retry",
            "deployment_start",
            "deployment_cancel",
            "environment_pr",
            "environment_setup",
            "github_ls",
            "github_connect",
            "server_show",
            "server_logs",
            "server_metrics",
            "service_status",
            "service_logs",
            "service_metrics",
            "volume_show",
            "service_restart",
            "service_start",
            "service_stop",
            "server_set",
            "server_drain",
            "server_upgrade",
            "server_clean",
            "volume_mirror",
            "volume_mirror_remove",
            "volume_move",
            "volume_release",
            "volume_restore",
            "environment_remove",
            "project_remove",
            "server_remove",
            "server_forget",
        ]
    );
}

#[test]
fn the_store_and_cloud_decide_who_asks_a_human() {
    let gated = catalog()
        .into_iter()
        .filter(|def| def.gate != Gate::Never)
        .map(|def| (def.name, def.gate))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        gated,
        BTreeMap::from([
            ("deploy", Gate::Store),
            ("environment_remove", Gate::Store),
            ("project_remove", Gate::Store),
            ("publish", Gate::Store),
            ("server_clean", Gate::Cloud),
            ("server_drain", Gate::Cloud),
            ("server_forget", Gate::Cloud),
            ("server_remove", Gate::Cloud),
            ("server_set", Gate::Cloud),
            ("volume_mirror", Gate::Cloud),
            ("volume_mirror_remove", Gate::Cloud),
            ("volume_move", Gate::Cloud),
            ("volume_release", Gate::Cloud),
            ("volume_restore", Gate::Cloud),
        ])
    );
}

#[test]
fn every_input_schema_is_portable() {
    for def in catalog() {
        assert_eq!(
            lint(&def.input_schema),
            Vec::<String>::new(),
            "{}",
            def.name
        );
    }
}

#[test]
fn no_tool_text_names_the_cli() {
    let text = serde_json::to_string(&json!({ "instructions": INSTRUCTIONS, "tools": catalog() }))
        .unwrap();
    for marker in ["ployz ", "--"] {
        assert!(!text.contains(marker), "the catalog says {marker:?}");
    }
}

#[test]
fn every_example_decodes() {
    for def in catalog() {
        let input = example(def.name).unwrap();
        let (step, _) = start(def.name, input, &place(in_org()), "call-1");
        if let Step::Done {
            result: ToolResult::Refused(refusal),
        } = step
        {
            assert_ne!(
                refusal.code,
                RpcErrorCode::InvalidArgument,
                "{}'s example: {}",
                def.name,
                refusal.message
            );
        }
    }
}

#[test]
fn a_local_store_cannot_answer_for_github() {
    let (step, _) = start(
        "github_ls",
        json!({ "repository": "acme/api" }),
        &place(StoreAt::Local),
        "call-1",
    );
    let Step::Done {
        result: ToolResult::Refused(refusal),
    } = step
    else {
        panic!("a local github_ls is refused at once");
    };
    assert_eq!(refusal.code.as_str(), "not_here");
    assert_eq!(refusal.fix(), Some(Fix::UseCloud));
    assert_eq!(
        refusal.retry,
        Call::new("github_ls", json!({ "repository": "acme/api" }))
    );
}

#[test]
fn an_unknown_tool_names_the_closest() {
    let refusal = call(&shop(), "deploi", json!({}), "call-1").refused();
    assert_eq!(refusal.code, RpcErrorCode::NotFound);
    assert_eq!(refusal.did_you_mean.as_deref(), Some("deploy"));
    assert_eq!(refusal.valid.len(), catalog().len());
}

#[test]
fn a_bad_input_names_the_field_and_an_example() {
    let refusal = call(
        &shop(),
        "deployment_show",
        json!({ "wait_seconds": "soon" }),
        "call-1",
    )
    .refused();
    assert_eq!(refusal.code, RpcErrorCode::InvalidArgument);
    assert_eq!(refusal.field.as_deref(), Some("wait_seconds"));
    assert_eq!(refusal.example, example("deployment_show"));
}

#[test]
fn review_reads_domain_changes_as_domains() {
    let store = shop();
    let service = json!({ "name": "web", "image": "traefik/whoami" });
    call(&store, "service_add", service, "add-1").ok();
    let domain = json!({ "service": "web", "prefix": "shop" });
    call(&store, "domain_add", domain, "domain-1").ok();

    let review = call(&store, "review", json!({}), "review-1").ok();
    let lines = review["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line == "  web.domains: added shop.<cluster domain>"),
        "{review}"
    );
    assert!(
        lines.iter().all(|line| {
            let line = line.as_str().unwrap();
            !line.contains("managedHostnames") && !line.contains("routes.")
        }),
        "{review}"
    );
}

#[test]
fn domain_check_reads_one_domain_and_refuses_an_unknown_one() {
    let store = shop();
    let service = json!({ "name": "web", "image": "traefik/whoami" });
    call(&store, "service_add", service, "add-1").ok();
    let domain = json!({ "service": "web", "hostname": "app.example.com", "port": 8080 });
    call(&store, "domain_add", domain, "domain-1").ok();

    let run = call(
        &store,
        "domain_check",
        json!({ "domain": "APP.example.com" }),
        "check-1",
    );
    assert_eq!((run.reads, run.writes), (1, 0));
    let checked = run.ok();
    assert_eq!(checked["domain"]["domain"], "app.example.com", "{checked}");
    assert_eq!(checked["service"], "web", "{checked}");
    assert_eq!(checked["domain"]["port"], 8080, "{checked}");
    assert_eq!(checked["at"], "shop/production", "{checked}");
    assert_eq!(checked["domain"]["status"], "setting_up", "{checked}");

    let missing = call(
        &store,
        "domain_check",
        json!({ "domain": "nope.example.com" }),
        "check-2",
    )
    .refused();
    assert_eq!(missing.code.as_str(), "not_found", "{missing:?}");
}

#[test]
fn a_service_goes_from_staged_to_deployed() {
    let store = shop();

    let added = call(
        &store,
        "service_add",
        json!({ "name": "web", "image": "traefik/whoami" }),
        "add-1",
    );
    assert_eq!(added.writes, 1);
    let added = added.ok();
    assert_eq!(added["at"], "shop/production");

    let review = call(&store, "review", json!({}), "review-1");
    assert_eq!(review.writes, 0);
    let review = review.ok();
    assert_eq!(review["asks_approval"], false);
    assert!(
        review["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line == "create service web"),
        "{review}"
    );

    let deploy = json!({ "request_key": "deploy-0001" });
    let deployed = call(&store, "deploy", deploy.clone(), "deploy-1").ok();
    let again = call(&store, "deploy", deploy, "deploy-2").ok();
    assert_eq!(
        deployed["deployment"], again["deployment"],
        "one key, one Deployment"
    );
    assert_eq!(deployed["next"]["tool"], "deployment_show");

    let shown = call(&store, "deployment_show", json!({}), "show-1").ok();
    assert_eq!(shown["deployment"], deployed["deployment"]);
    assert_eq!(shown["in_flight"], true);
    assert_eq!(
        shown["next"],
        json!([{
            "tool": "deployment_show",
            "input": { "deployment": deployed["deployment"], "wait_seconds": 120 },
        }])
    );

    let waited = call(
        &store,
        "deployment_show",
        json!({ "wait_seconds": 1_000 }),
        "show-2",
    );
    assert_eq!(waited.waited_ms, WAIT_CAP_SECONDS * 1_000);
    assert!(waited.reads <= STEP_BUDGET);

    let cancelled = call(&store, "deployment_cancel", json!({}), "cancel-1").ok();
    assert_eq!(cancelled["deployment"], deployed["deployment"]);
    let ended = call(&store, "deployment_cancel", json!({}), "cancel-2").refused();
    assert_eq!(ended.code, RpcErrorCode::Conflict);
}

#[test]
fn a_stale_revision_is_refused_with_a_fresh_read() {
    let store = shop();
    call(
        &store,
        "service_add",
        json!({ "name": "web", "image": "traefik/whoami" }),
        "add-1",
    )
    .ok();
    let input = json!({ "service": "web", "set": { "env": { "LOG_LEVEL": "info" } }, "expect_revision": 0 });
    let stale = call(&store, "service_update", input, "update-1").refused();
    assert_eq!(stale.code, RpcErrorCode::Conflict);
    assert_eq!(stale.writes, Writes::None);
    assert_eq!(
        stale.next,
        [Call::new("service_inspect", json!({ "service": "web" }))]
    );
}

#[test]
fn config_write_sets_permissions_and_a_later_write_keeps_them() {
    let store = shop();
    let file = |store: &ConfigStore| {
        let view = store.read(&who(), &ConfigsQuery::default()).unwrap();
        let file = view.configs[0].config.files[0].clone();
        (file.mode.to_string(), file.uid, file.gid)
    };
    call(
        &store,
        "config_write",
        json!({
            "config": "app",
            "files": { "app.conf": "a" },
            "mode": "0440",
            "uid": 1000,
            "gid": 1001
        }),
        "write-1",
    )
    .ok();
    assert_eq!(file(&store), ("0440".to_owned(), 1000, 1001));
    call(
        &store,
        "config_write",
        json!({ "config": "app", "files": { "app.conf": "b" } }),
        "write-2",
    )
    .ok();
    assert_eq!(file(&store), ("0440".to_owned(), 1000, 1001));

    let refusal = call(
        &store,
        "config_write",
        json!({ "config": "app", "files": { "app.conf": "c" }, "mode": "1777" }),
        "write-3",
    )
    .refused();
    assert_eq!(refusal.code, RpcErrorCode::InvalidArgument);
    assert_eq!(refusal.field.as_deref(), Some("mode"));
}

#[test]
fn discard_drops_a_staged_service() {
    let store = shop();
    call(
        &store,
        "service_add",
        json!({ "name": "web", "image": "traefik/whoami" }),
        "add-1",
    )
    .ok();
    let discarded = call(&store, "discard", json!({ "service": "web" }), "discard-1").ok();
    assert_eq!(discarded["discarded"], "service web");
    let review = call(&store, "review", json!({}), "review-1").ok();
    assert_eq!(review["lines"], json!([]), "{review}");
}

#[test]
fn history_names_the_draft_it_overwrites_and_holds_the_member_to_it() {
    let store = shop();
    let image = |tag: &str, key: &str| {
        call(
            &store,
            "service_update",
            json!({ "service": "web", "set": { "image": format!("traefik/whoami:{tag}") } }),
            key,
        )
        .ok();
    };
    call(
        &store,
        "service_add",
        json!({ "name": "web", "image": "traefik/whoami:v1" }),
        "add-1",
    )
    .ok();
    store
        .write_trusted(
            &who(),
            &ployz_store::Publish {
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &ployz_store::Trusted {
                approval: ployz_store::Approval::NotRequired,
                ..ployz_store::Trusted::default()
            },
        )
        .unwrap();
    image("v2", "update-1");
    image("v3", "update-2");

    let unpinned = call(
        &store,
        "history",
        json!({ "revision": 1, "accept_overwrite": true }),
        "history-1",
    )
    .refused();
    assert_eq!(unpinned.code, RpcErrorCode::InvalidArgument);
    assert_eq!(unpinned.field.as_deref(), Some("version"));

    let asked = call(&store, "history", json!({ "revision": 1 }), "history-2").refused();
    assert_eq!(asked.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(asked.writes, Writes::None);
    assert!(
        asked.details["overwrites"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field == "web.source")),
        "{asked:?}"
    );
    assert_eq!(asked.retry.input["accept_overwrite"], true);
    let version = asked.retry.input["version"].as_str().unwrap().to_owned();

    image("v4", "update-3");
    let stale = call(&store, "history", asked.retry.input.clone(), "history-3").refused();
    assert_eq!(stale.code, RpcErrorCode::Conflict, "{stale:?}");
    assert_eq!(stale.writes, Writes::None);
    assert_eq!(stale.next, [Call::new("history", json!({ "revision": 1 }))]);

    let again = call(&store, "history", json!({ "revision": 1 }), "history-4").refused();
    assert_ne!(again.retry.input["version"], version.as_str());
    let staged = call(&store, "history", again.retry.input, "history-5").ok();
    assert_eq!(staged["changed"], true, "{staged}");
    let shown = call(
        &store,
        "service_inspect",
        json!({ "service": "web" }),
        "inspect-1",
    )
    .ok();
    assert!(shown.to_string().contains("traefik/whoami:v1"), "{shown}");
}

#[test]
fn project_create_makes_a_project_once_per_key() {
    let store = shop();
    let input = json!({ "name": "docs", "request_key": "project-docs-0001" });
    let created = call(&store, "project_create", input.clone(), "create-1").ok();
    assert_eq!(created["at"], "docs/production");
    assert_eq!(created["next"]["tool"], "environment_show");
    let again = call(&store, "project_create", input, "create-2").ok();
    assert_eq!(again, created, "the same key replays the same Project");
    let shown = call(
        &store,
        "environment_show",
        json!({ "project": "docs" }),
        "show-1",
    )
    .ok();
    assert_eq!(shown["at"], "docs/production", "{shown}");

    let refusal = call(
        &store,
        "project_create",
        json!({ "name": "docs", "request_key": "project-docs-0002" }),
        "create-3",
    )
    .refused();
    assert_eq!(refusal.code, RpcErrorCode::Conflict);
    assert_eq!(refusal.writes, Writes::None);
}

#[test]
fn project_rename_applies_at_once_and_repeats_harmlessly() {
    let store = shop();
    let input = json!({ "project": "shop", "to": "store" });
    let renamed = call(&store, "project_rename", input, "rename-1").ok();
    assert_eq!(renamed["project"], "store", "{renamed}");
    assert_eq!(renamed["renamed_from"], "shop");
    let shown = call(
        &store,
        "environment_show",
        json!({ "project": "store" }),
        "show-1",
    )
    .ok();
    assert_eq!(shown["at"], "store/production", "{shown}");
    assert_eq!(shown["projects"][0]["name"], "store");

    let again = call(
        &store,
        "project_rename",
        json!({ "project": "store", "to": "store" }),
        "rename-2",
    )
    .ok();
    assert_eq!(again["project"], "store");

    call(
        &store,
        "project_create",
        json!({ "name": "docs", "request_key": "project-docs-0001" }),
        "create-1",
    )
    .ok();
    let taken = call(
        &store,
        "project_rename",
        json!({ "project": "docs", "to": "store" }),
        "rename-3",
    )
    .refused();
    assert_eq!(taken.code, RpcErrorCode::Conflict, "{taken:?}");
    assert_eq!(taken.writes, Writes::None);
}

#[test]
fn config_rename_stages_the_new_name_and_keeps_its_files() {
    let store = shop();
    call(
        &store,
        "config_write",
        json!({ "config": "nginx", "files": { "nginx.conf": "events {}" } }),
        "write-1",
    )
    .ok();
    let renamed = call(
        &store,
        "config_rename",
        json!({ "config": "nginx", "to": "proxy" }),
        "rename-1",
    )
    .ok();
    assert_eq!(renamed["at"], "shop/production", "{renamed}");
    assert_eq!(renamed["config"], "proxy");
    assert_eq!(renamed["renamed_from"], "nginx");
    assert_eq!(renamed["staged"], true);
    let shown = call(&store, "environment_show", json!({}), "show-1").ok();
    assert_eq!(shown["configs"], json!(["proxy"]), "{shown}");

    let gone = call(
        &store,
        "config_rename",
        json!({ "config": "nginx", "to": "edge" }),
        "rename-2",
    )
    .refused();
    assert_eq!(gone.code, RpcErrorCode::NotFound, "{gone:?}");
}

#[test]
fn two_organizations_create_with_the_same_key() {
    let store = shop();
    let input = json!({ "name": "docs", "request_key": "k7Qm2xVpR4tN" });
    let create = |organization: &str, call_key: &str| {
        let organization = OrganizationId::parse(organization).unwrap();
        let who = Actor::system(organization.clone());
        let at = StoreAt::Cloud { organization };
        drive_as(
            &store,
            &who,
            at,
            "project_create",
            input.clone(),
            call_key,
            &mut no_cloud,
        )
        .ok()
    };
    let acme = create("acme", "acme-1");
    assert_eq!(acme["at"], "docs/production");
    assert_eq!(create("globex", "globex-1")["at"], "docs/production");
    assert_eq!(
        create("acme", "acme-2"),
        acme,
        "a retry replays its own Project"
    );
}

/// The tools that need Ployz Cloud's view of the Servers.
const CLOUD_TOOLS: [&str; 21] = [
    "server_show",
    "server_logs",
    "server_metrics",
    "service_status",
    "service_logs",
    "service_metrics",
    "volume_show",
    "service_restart",
    "service_start",
    "service_stop",
    "server_set",
    "server_drain",
    "server_upgrade",
    "server_clean",
    "volume_mirror",
    "volume_mirror_remove",
    "volume_move",
    "volume_release",
    "volume_restore",
    "server_remove",
    "server_forget",
];

fn machine(letter: char) -> String {
    letter.to_string().repeat(32)
}

fn server(name: &str, letter: char) -> Value {
    json!({ "machine": machine(letter), "name": name })
}

/// Cloud's answer listing `names` as online Servers.
fn servers(names: &[(&str, char)]) -> Value {
    let servers = names
        .iter()
        .map(|(name, letter)| {
            json!({
                "machine": machine(*letter), "name": name, "state": "online",
                "release": null, "upgrade": null, "draining": false, "disk": null,
                "namespaces": [],
            })
        })
        .collect::<Vec<_>>();
    json!({
        "servers": servers,
        "seen": {
            "seen_by": null, "observed_at": null, "read_at": 0, "age_seconds": null,
            "from": "server", "unanswered": [],
        },
    })
}

/// The row Cloud answers an act with.
fn operation(act: &Act, state: &str) -> Value {
    json!({
        "attempt": act.attempt, "run": "drain web-2", "state": state, "requested_at": 0,
        "ended_at": null, "reason": null, "servers": [], "kept": [], "unanswered": [],
    })
}

fn rpc_error(code: &str, details: Value) -> RpcError {
    serde_json::from_value(json!({ "code": code, "message": "refused", "details": details }))
        .unwrap()
}

/// [`shop`] whose `production` has Volume `pgdata`.
fn with_volume() -> ConfigStore {
    let store = shop();
    store
        .write(
            &who(),
            &CreateVolume {
                storage_locked: false,
                id: VolumeId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("pgdata").unwrap(),
                storage: ployz_core::config::VolumeKind::Docker {},
                mounts: Vec::new(),
                shared_writes: false,
            },
        )
        .unwrap();
    store
}

#[test]
fn a_local_store_refuses_every_cloud_tool_before_it_asks_or_writes() {
    let store = with_volume();
    for tool in CLOUD_TOOLS {
        let run = drive(
            &store,
            StoreAt::Local,
            tool,
            example(tool).unwrap(),
            "local-1",
            &mut no_cloud,
        );
        assert_eq!(run.writes, 0, "{tool}");
        let refusal = run.refused();
        assert_eq!(refusal.code.as_str(), "not_here", "{tool}");
        assert_eq!(refusal.fix(), Some(Fix::UseCloud), "{tool}");
    }
}

#[test]
fn there_is_no_separate_sync_tool() {
    assert!(catalog().iter().all(|tool| tool.name != "volume_sync"));
}

/// Remove what nothing deployed: one write, nothing to confirm, gone at once.
fn removed(store: &ConfigStore, tool: &str, input: Value) -> Value {
    let run = drive(
        store,
        StoreAt::Local,
        tool,
        input,
        "remove-1",
        &mut no_cloud,
    );
    assert_eq!(run.writes, 1);
    run.ok()
}

#[test]
fn an_undeployed_environment_is_removed_from_a_local_store() {
    let store = shop();
    store
        .write(
            &who(),
            &CreateEnvironment {
                id: EnvironmentId::parse("00000000-0000-4000-8000-000000000006").unwrap(),
                project: None,
                name: EnvironmentName::parse("preview").unwrap(),
            },
        )
        .unwrap();

    let removed = removed(
        &store,
        "environment_remove",
        json!({ "environment": "preview" }),
    );

    assert_eq!(removed["state"], "removed");
    assert_eq!(removed["at"], "shop/preview");
    assert_eq!(removed["left"], json!([]));
    assert!(removed.get("next").is_none_or(Value::is_null), "{removed}");
    let listed = store
        .read(&who(), &EnvironmentsQuery { project: None })
        .unwrap();
    let names = listed
        .environments
        .iter()
        .map(|environment| environment.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["production"]);
}

#[test]
fn an_undeployed_project_is_removed_in_one_call() {
    let store = shop();

    let removed = removed(&store, "project_remove", json!({ "project": "shop" }));

    assert_eq!(removed["state"], "removed");
    assert_eq!(removed["at"], "shop");
    let gone = store
        .read(
            &who(),
            &EnvironmentsQuery {
                project: Some(ProjectName::parse("shop").unwrap()),
            },
        )
        .unwrap_err();
    assert_eq!(gone.code, RpcErrorCode::NotFound);
}

#[test]
fn a_lost_reply_retries_with_the_key_that_names_the_same_attempt() {
    for (tool, input) in [
        ("server_drain", json!({ "server": "web-2" })),
        ("server_upgrade", json!({ "server": "web-2" })),
        (
            "server_set",
            json!({ "server": "web-2", "accepts_ingress": false }),
        ),
    ] {
        let store = shop();
        let mut attempts = Vec::new();
        let mut lost = true;
        let mut cloud = |cloud: Cloud| match cloud {
            Cloud::Ask(CloudQuery::Servers { .. }) => Ok(servers(&[("web-2", 'b')])),
            Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
            Cloud::Act(act) => {
                attempts.push(act.attempt);
                match std::mem::replace(&mut lost, false) {
                    true => Err(rpc_error("unavailable", json!({ "writes": "unknown" }))),
                    false => Ok(operation(&act, "done")),
                }
            }
        };

        let refusal = drive(&store, in_org(), tool, input, "act-1", &mut cloud).refused();
        assert_eq!(refusal.writes, Writes::Unknown, "{tool}");
        assert_eq!(refusal.retry.tool, tool);
        assert!(
            refusal.retry.input["request_key"].is_string(),
            "{tool}: {refusal:?}"
        );

        let done = drive(
            &store,
            in_org(),
            tool,
            refusal.retry.input,
            "act-2",
            &mut cloud,
        )
        .ok();
        assert_eq!(done["state"], "done", "{tool}");
        assert_eq!(done["at"], "web-2", "{tool}");
        assert_eq!(attempts.len(), 2, "{tool}");
        assert_eq!(
            attempts[0], attempts[1],
            "{tool}: the retry is the same attempt"
        );
    }
}

#[test]
fn a_server_that_does_not_answer_is_forgotten_without_a_reset() {
    let store = shop();
    let mut acted = Vec::new();
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Servers { .. }) => {
            let mut listed = servers(&[("web-2", 'b')]);
            listed["servers"][0]["state"] = json!("offline");
            Ok(listed)
        }
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => {
            acted.push(act.action.clone());
            Ok(operation(&act, "done"))
        }
    };

    let forgotten = drive(
        &store,
        in_org(),
        "server_forget",
        json!({ "server": "web-2" }),
        "forget-1",
        &mut cloud,
    )
    .ok();

    assert_eq!(forgotten["at"], "web-2");
    let [Action::ForgetServer { server }] = acted.as_slice() else {
        panic!("forget acts once, as a forget: {acted:?}");
    };
    assert_eq!(server.name.as_str(), "web-2");
}

#[test]
fn server_set_refuses_a_server_that_does_not_answer() {
    let store = shop();
    let mut offline = servers(&[("db-1", 'a'), ("db-2", 'b')]);
    offline["servers"][0]["state"] = json!("offline");
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Servers { .. }) => Ok(offline.clone()),
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => panic!("nothing is asked of a silent server: {act:?}"),
    };

    let refusal = drive(
        &store,
        in_org(),
        "server_set",
        json!({ "server": "db-1", "accepts_services": false }),
        "set-1",
        &mut cloud,
    )
    .refused();

    assert_eq!(refusal.code.as_str(), "unavailable");
    assert_eq!(refusal.field.as_deref(), Some("server"));
}

#[test]
fn server_set_sends_only_the_values_it_was_given() {
    let store = shop();
    let mut acted = Vec::new();
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Servers { .. }) => Ok(servers(&[("db-1", 'a')])),
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => {
            acted.push(act.action.clone());
            Ok(operation(&act, "accepted"))
        }
    };

    let set = drive(
        &store,
        in_org(),
        "server_set",
        json!({ "server": "db-1", "accepts_ingress": false, "build_concurrency": "auto" }),
        "set-1",
        &mut cloud,
    )
    .ok();

    assert_eq!(set["state"], "accepted");
    let [
        Action::SetServer {
            server,
            accepts_builds: None,
            accepts_services: None,
            accepts_ingress: Some(false),
            build_concurrency: BuildConcurrencyUpdate::Automatic,
        },
    ] = acted.as_slice()
    else {
        panic!("set acts once, with the given values: {acted:?}");
    };
    assert_eq!(server.name.as_str(), "db-1");
}

#[test]
fn server_set_leaves_turning_web_traffic_on_to_the_terminal() {
    let store = shop();
    let mut cloud = |_: Cloud| -> Result<Value, RpcError> { panic!("refused before Cloud") };
    for (input, field) in [
        (
            json!({ "server": "db-1", "accepts_ingress": true }),
            Some("accepts_ingress"),
        ),
        (json!({ "server": "db-1" }), Some("")),
        (
            json!({ "server": "db-1", "build_concurrency": "300" }),
            Some("build_concurrency"),
        ),
    ] {
        let refusal = drive(&store, in_org(), "server_set", input, "set-1", &mut cloud).refused();
        assert_eq!(refusal.code.as_str(), "invalid_argument");
        assert_eq!(refusal.field.as_deref(), field);
    }
    let empty = drive(
        &store,
        in_org(),
        "server_set",
        json!({ "server": "db-1" }),
        "set-2",
        &mut cloud,
    )
    .refused();
    assert_eq!(
        empty.valid,
        [
            "accepts_builds",
            "accepts_services",
            "accepts_ingress",
            "build_concurrency"
        ]
    );
}

#[test]
fn a_second_mirror_is_refused_rather_than_replacing_the_first() {
    let store = with_volume();
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Servers { .. }) => {
            Ok(servers(&[("db-1", 'a'), ("db-2", 'b'), ("db-3", 'c')]))
        }
        Cloud::Ask(CloudQuery::Copies { .. }) => Ok(json!({
            "volumes": [{
                "volume": "pgdata",
                "writer": server("db-1", 'a'),
                "mirror": { "server": server("db-3", 'c'), "synced_at": null },
            }],
            "seen": servers(&[])["seen"],
        })),
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => panic!("a conflict acts on nothing: {act:?}"),
    };

    let run = drive(
        &store,
        in_org(),
        "volume_mirror",
        json!({ "volume": "pgdata", "server": "db-2" }),
        "mirror-1",
        &mut cloud,
    );

    assert_eq!(run.writes, 0);
    let refusal = run.refused();
    assert_eq!(refusal.code, RpcErrorCode::Conflict);
    assert_eq!(refusal.writes, Writes::None);
    assert!(refusal.message.contains("db-3"), "{}", refusal.message);
    assert_eq!(
        refusal.next,
        [Call::new(
            "volume_mirror_remove",
            json!({ "volume": "pgdata" })
        )]
    );
}

#[test]
fn a_restore_names_the_server_it_restores_from() {
    let store = with_volume();
    let mut acted = Vec::new();
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Servers { .. }) => Ok(servers(&[("db-1", 'a'), ("db-2", 'b')])),
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => {
            acted.push(act.action.clone());
            Ok(operation(&act, "done"))
        }
    };

    drive(
        &store,
        in_org(),
        "volume_restore",
        json!({ "volume": "pgdata", "from": "db-2" }),
        "restore-1",
        &mut cloud,
    )
    .ok();

    let [
        Action::Volume {
            run: VolumeRun::Restore { from },
            ..
        },
    ] = acted.as_slice()
    else {
        panic!("restore acts once, as a restore: {acted:?}");
    };
    assert_eq!(from.name.as_str(), "db-2");
}

#[test]
fn a_release_after_the_handover_retries_as_the_move() {
    let store = with_volume();
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Act(_) => Err(rpc_error(
            "failed_precondition",
            json!({ "move_to": "db-2" }),
        )),
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
    };

    let refusal = drive(
        &store,
        in_org(),
        "volume_release",
        json!({ "volume": "pgdata" }),
        "release-1",
        &mut cloud,
    )
    .refused();

    assert_eq!(
        refusal.retry,
        Call::new("volume_move", json!({ "volume": "pgdata", "to": "db-2" }))
    );
    assert!(refusal.details.get("move_to").is_none(), "{refusal:?}");
}

/// [`shop`] with `web` published and Deployment 1 of `production` admitted.
fn deploying() -> (ConfigStore, DeploymentId) {
    let store = shop();
    store
        .write(
            &who(),
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("traefik/whoami".into()),
                template: None,
            },
        )
        .unwrap();
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write(
            &who(),
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
        )
        .unwrap();
    (store, id)
}

fn remove_container() -> Value {
    json!({ "type": "remove_container", "machine_id": machine('a'), "container_id": "0".repeat(64) })
}

#[test]
fn a_deployment_shows_its_progress_then_where_its_failure_logged() {
    let (store, id) = deploying();
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id, &runner).unwrap();
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace,
        "operations": [{
            "index": 0, "machine_id": machine('a'), "service_name": "web",
            "operation": remove_container(), "status": { "type": "pending" },
        }],
        "warnings": [], "would_remove": [], "preserved_volumes": [],
    }))
    .unwrap();
    store
        .record(&id, &runner, RunEvidence::Prepared(preview))
        .unwrap();

    let rows: Vec<ployz_core::OperationRow> = serde_json::from_value(json!([{
        "index": 0, "machine_id": machine('a'), "service_name": "web",
        "operation": remove_container(),
        "status": { "type": "running", "phase": { "type": "starting" } },
    }]))
    .unwrap();
    let mut tracker = RowTracker::default();
    store
        .record(&id, &runner, RunEvidence::Progress(tracker.changes(&rows)))
        .unwrap();

    let running = call(&store, "deployment_show", json!({}), "show-1").ok();
    assert_eq!(running["in_flight"], true);
    assert_eq!(running["progress"]["servers_done"], 0);
    assert_eq!(running["progress"]["servers_total"], 1);
    assert!(!running["progress"]["phase"].is_null(), "{running}");
    assert_eq!(running["next"][0]["tool"], "deployment_show");

    let error = json!({
        "type": "machine", "action": "RemoveContainer",
        "error": { "code": "internal", "message": "busy", "details": {} },
    });
    let rows: Vec<ployz_core::OperationRow> = serde_json::from_value(json!([{
        "index": 0, "machine_id": machine('a'), "service_name": "web",
        "operation": remove_container(), "status": { "type": "failed", "error": error },
    }]))
    .unwrap();
    let outcome: DeployOutcome<ployz_core::ExecutionError> = serde_json::from_value(json!({
        "type": "failed", "completed": [], "unexecuted": [],
        "failed": { "type": "operation", "operation": remove_container(), "error": error },
    }))
    .unwrap();
    store
        .record(
            &id,
            &runner,
            RunEvidence::Executed {
                progress: tracker.changes(&rows),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();

    let failed = call(&store, "deployment_show", json!({}), "show-2").ok();
    assert_eq!(failed["in_flight"], false);
    assert!(failed.get("progress").is_none(), "{failed}");
    let input = json!({ "service": "web", "deployment": failed["deployment"] });
    assert_eq!(
        failed["next"],
        json!([
            { "tool": "service_logs", "input": input },
            { "tool": "service_metrics", "input": input },
        ])
    );
}

/// [`deploying`] run to its end: its one row left in `row`, its runner saying
/// `outcome`.
fn ended(row: Value, outcome: Value) -> (ConfigStore, DeploymentId) {
    let (store, id) = deploying();
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id, &runner).unwrap();
    let operation = json!({
        "index": 0, "machine_id": machine('a'), "service_name": "web",
        "operation": remove_container(),
    });
    let mut pending = operation.clone();
    pending["status"] = json!({ "type": "pending" });
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace, "operations": [pending],
        "warnings": [], "would_remove": [], "preserved_volumes": [],
    }))
    .unwrap();
    store
        .record(&id, &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let mut done = operation;
    done["status"] = row;
    let rows: Vec<ployz_core::OperationRow> = serde_json::from_value(json!([done])).unwrap();
    store
        .record(
            &id,
            &runner,
            RunEvidence::Executed {
                progress: RowTracker::default().changes(&rows),
                outcome: Box::new(serde_json::from_value(outcome).unwrap()),
                removed: Vec::new(),
            },
        )
        .unwrap();
    (store, id)
}

#[test]
fn a_failed_deployments_next_calls_read_its_own_window_as_given() {
    let error = json!({
        "type": "machine", "action": "RemoveContainer",
        "error": { "code": "internal", "message": "exit 1", "details": {} },
    });
    let (store, id) = ended(
        json!({ "type": "failed", "error": error }),
        json!({
            "type": "failed", "completed": [], "unexecuted": [],
            "failed": { "type": "operation", "operation": remove_container(), "error": error },
        }),
    );
    let shown = call(
        &store,
        "deployment_show",
        json!({ "project": "shop", "environment": "production" }),
        "show-1",
    )
    .ok();
    let started = shown["started_at"]
        .as_i64()
        .expect("a run Deployment started");

    let (mut logs, mut metrics) = (Vec::new(), Vec::new());
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Logs(query)) => {
            logs.push(query);
            Ok(json!({ "servers": [], "seen": servers(&[])["seen"], "notes": [] }))
        }
        Cloud::Ask(CloudQuery::Metrics(query)) => {
            metrics.push(query);
            Ok(metrics_answer(json!([])))
        }
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => panic!("a read acts on nothing: {act:?}"),
    };
    for next in shown["next"].as_array().expect("a list of next calls") {
        drive(
            &store,
            in_org(),
            next["tool"].as_str().unwrap(),
            next["input"].clone(),
            "next-1",
            &mut cloud,
        )
        .ok();
    }

    let [logs] = logs.as_slice() else {
        panic!("asked logs {logs:?}");
    };
    assert_eq!(logs.deployment.as_ref(), Some(&id), "its containers");
    assert_eq!(logs.since, None, "not cut to a recent window");
    let [metrics] = metrics.as_slice() else {
        panic!("asked metrics {metrics:?}");
    };
    assert_eq!(metrics.deployment.as_ref(), Some(&id), "its events");
    assert_eq!(
        metrics.since,
        MetricsSince::At {
            at: Timestamp(started)
        },
        "from its start"
    );
}

#[test]
fn a_succeeded_deployment_names_no_next_call() {
    let (store, _) = ended(
        json!({ "type": "completed" }),
        json!({ "type": "success", "completed": [remove_container()] }),
    );

    let shown = call(&store, "deployment_show", json!({}), "show-1").ok();
    assert_eq!(shown["in_flight"], false);
    assert_eq!(shown["nodes"][0]["outcome"], "deployed", "{shown}");
    assert!(shown.get("next").is_none(), "{shown}");
}

#[test]
fn a_queued_deployment_starts_and_a_cancelled_one_retries_once_per_key() {
    let (store, _) = deploying();

    let started = call(&store, "deployment_start", json!({}), "start-1");
    assert_eq!(started.writes, 1);
    let started = started.ok();
    assert_eq!(started["deployment"], 1);
    assert_eq!(started["status"], "queued");
    assert_eq!(
        started["next"],
        json!({ "tool": "deployment_show", "input": { "deployment": 1, "wait_seconds": 120 } })
    );
    let unended = call(&store, "deployment_retry", json!({}), "retry-1").refused();
    assert_eq!(unended.code, RpcErrorCode::Conflict);
    assert_eq!(unended.writes, Writes::None);

    call(&store, "deployment_cancel", json!({}), "cancel-1").ok();
    let ended = call(&store, "deployment_start", json!({}), "start-2").refused();
    assert_eq!(ended.code, RpcErrorCode::Conflict);

    let retry = json!({ "deployment": 1, "request_key": "retry-0001" });
    let retried = call(&store, "deployment_retry", retry.clone(), "retry-2").ok();
    assert_eq!(retried["deployment"], 2);
    assert_eq!(retried["retries"], 1);
    assert_eq!(retried["status"], "queued");
    assert_eq!(retried["next"]["input"]["deployment"], 2);
    let again = call(&store, "deployment_retry", retry, "retry-3").ok();
    assert_eq!(again["deployment"], 2, "one key, one Deployment");
    let named = json!({ "deployment": 1 });
    let cancelled = call(&store, "deployment_start", named, "start-3").refused();
    assert_eq!(
        cancelled.code,
        RpcErrorCode::Conflict,
        "starts #1, not the latest"
    );

    let numbers = |listed: &Value| {
        listed["deployments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|deployment| deployment["deployment"].clone())
            .collect::<Vec<_>>()
    };
    let all = call(&store, "deployment_list", json!({}), "list-1");
    assert_eq!(all.writes, 0);
    let all = all.ok();
    assert_eq!(numbers(&all), [json!(2), json!(1)], "newest first");
    assert_eq!(all["deployments"][1]["status"], "cancelled");
    let source_revision = &all["deployments"][1]["saved_revision"];
    assert!(source_revision.is_u64(), "{all}");
    assert_eq!(retried["saved_revision"], *source_revision);
    assert!(all.get("next").is_none(), "{all}");

    let first = call(&store, "deployment_list", json!({ "limit": 1 }), "list-2").ok();
    assert_eq!(numbers(&first), [json!(2)]);
    let next = &first["next"][0];
    assert_eq!(next["tool"], "deployment_list");
    assert_eq!(next["input"]["limit"], 1);
    let second = call(&store, "deployment_list", next["input"].clone(), "list-3").ok();
    assert_eq!(numbers(&second), [json!(1)]);
    assert!(second.get("next").is_none(), "{second}");
}

#[test]
fn a_keyed_retry_of_the_latest_replays_after_it_becomes_the_latest() {
    let (store, _) = deploying();
    call(&store, "deployment_cancel", json!({}), "cancel-1").ok();

    let retry = json!({ "request_key": "retry-latest-1" });
    let retried = call(&store, "deployment_retry", retry.clone(), "retry-1").ok();
    assert_eq!(retried["deployment"], 2);
    assert_eq!(retried["retries"], 1);
    let again = call(&store, "deployment_retry", retry, "retry-2");
    assert_eq!(again.writes, 0);
    let again = again.ok();
    assert_eq!(again["deployment"], 2, "{again}");
    assert_eq!(again["saved_revision"], retried["saved_revision"]);

    store
        .write(
            &who(),
            &CreateEnvironment {
                id: EnvironmentId::parse("00000000-0000-4000-8000-000000000006").unwrap(),
                project: None,
                name: EnvironmentName::parse("preview").unwrap(),
            },
        )
        .unwrap();
    let elsewhere = json!({ "environment": "preview", "request_key": "retry-latest-1" });
    let elsewhere = call(&store, "deployment_retry", elsewhere, "retry-3").refused();
    assert_eq!(
        elsewhere.code,
        RpcErrorCode::NotFound,
        "not production's retry"
    );
}

#[test]
fn a_keyed_retry_that_lands_between_its_replays_reads_is_the_replay() {
    let (store, _) = deploying();
    call(&store, "deployment_cancel", json!({}), "cancel-1").ok();

    let retry = json!({ "request_key": "retry-race-1" });
    let (step, state) = start(
        "deployment_retry",
        retry.clone(),
        &place(in_org()),
        "retry-2",
    );
    let Step::Read { asks } = step else {
        panic!("reads first")
    };
    let [_, newest]: [Ask; 2] = asks.try_into().unwrap();
    let not_yet = Err(rpc_error("not_found", Value::Null));
    call(&store, "deployment_retry", retry, "retry-1").ok();
    let newest = answer(&store, &who(), &mut no_cloud, newest);
    let (Step::Done { result }, _) = resume(state, vec![not_yet, newest]) else {
        panic!("a replay writes nothing")
    };
    let ToolResult::Ok(replayed) = result else {
        panic!("refused: {result:?}")
    };
    assert_eq!(replayed["deployment"], 2, "{replayed}");
    assert_eq!(replayed["retries"], Value::Null);
}

#[test]
fn an_applied_deployment_has_nothing_to_retry() {
    let (store, _) = ended(
        json!({ "type": "completed" }),
        json!({ "type": "success", "completed": [remove_container()] }),
    );

    let refused = call(&store, "deployment_retry", json!({}), "retry-1").refused();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(refused.writes, Writes::None);
}

/// Cloud's metrics answer with `events`, one CPU row and nothing unanswered.
fn metrics_answer(events: Value) -> Value {
    json!({
        "since": 100, "until": 200,
        "metrics": [{ "metric": "CPU", "now": "0.25 vCPU", "avg": "0.20 vCPU", "peak": "0.90 vCPU" }],
        "oom_kills": 1, "exits": 1, "restarts": 0,
        "events": events, "notes": [], "seen": servers(&[])["seen"],
    })
}

#[test]
fn a_deployments_metrics_show_its_oom_kill_and_exit_code_by_deployment_number() {
    let (store, id) = deploying();
    let mut asked = Vec::new();
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Metrics(query)) => {
            let MetricsSubject::Service { namespace, .. } = &query.subject else {
                panic!("asked about a Server: {query:?}");
            };
            let event = |at: u32, kind: Value| {
                let mut event = json!({
                    "at": at, "server": "web-1", "namespace": namespace, "service": "web",
                    "deployment": id,
                });
                event
                    .as_object_mut()
                    .unwrap()
                    .extend(kind.as_object().unwrap().clone());
                event
            };
            let answer = metrics_answer(json!([
                event(150, json!({ "kind": "exit", "exit_code": 137 })),
                event(140, json!({ "kind": "oom" })),
            ]));
            asked.push(query);
            Ok(answer)
        }
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => panic!("a read acts on nothing: {act:?}"),
    };

    let shown = drive(
        &store,
        in_org(),
        "service_metrics",
        json!({ "service": "web", "deployment": 1 }),
        "metrics-1",
        &mut cloud,
    )
    .ok();

    let [query] = asked.as_slice() else {
        panic!("asked {asked:?}");
    };
    assert_eq!(query.deployment.as_ref(), Some(&id), "only its events");
    assert!(
        matches!(query.since, MetricsSince::At { .. }),
        "the range starts at the Deployment: {query:?}"
    );
    assert!(
        matches!(&query.subject, MetricsSubject::Service { service, .. } if service.as_str() == "web"),
        "{query:?}"
    );
    assert_eq!(shown["oom_kills"], 1);
    assert_eq!(shown["exits"], 1);
    assert_eq!(shown["metrics"][0]["peak"], "0.90 vCPU");
    assert_eq!(
        shown["events"],
        json!([
            { "at": 150, "server": "web-1", "kind": "exit", "exit_code": 137, "deployment": 1 },
            { "at": 140, "server": "web-1", "kind": "oom", "deployment": 1 },
        ])
    );
}

#[test]
fn a_deployments_metrics_refuse_a_service_it_did_not_deploy() {
    let store = shop();
    for (id, name) in [(3, "web"), (4, "worker")] {
        store
            .write(
                &who(),
                &CreateService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000000{id}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some("traefik/whoami".into()),
                    template: None,
                },
            )
            .unwrap();
    }
    let deploy = |id: &str, services: &[&str]| {
        let id = DeploymentId::parse(id).unwrap();
        store
            .write(
                &who(),
                &Admit::Deploy(Deploy {
                    id: id.clone(),
                    environment: EnvironmentRef::default(),
                    services: services
                        .iter()
                        .map(|name| ServiceName::parse(*name).unwrap())
                        .collect(),
                    version: None,
                    upload: None,
                    accept_volume_loss: Vec::new(),
                    message: None,
                }),
            )
            .unwrap();
        id
    };
    let first = deploy("00000000-0000-4000-8000-000000000101", &[]);
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&first, &runner).unwrap();
    let operations: Vec<Value> = ["web", "worker"]
        .iter()
        .enumerate()
        .map(|(index, service)| {
            let container = if *service == "web" { "0" } else { "1" };
            let service = *service;
            json!({
                "index": index, "machine_id": machine('a'), "service_name": service,
                "operation": {
                    "type": "remove_container", "machine_id": machine('a'),
                    "container_id": container.repeat(64),
                },
                "status": { "type": "pending" },
            })
        })
        .collect();
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace, "operations": operations,
        "warnings": [], "would_remove": [], "preserved_volumes": [],
    }))
    .unwrap();
    store
        .record(&first, &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: DeployOutcome<ployz_core::ExecutionError> = serde_json::from_value(json!({
        "type": "success",
        "completed": operations.iter().map(|row| row["operation"].clone()).collect::<Vec<_>>(),
    }))
    .unwrap();
    store
        .record(
            &first,
            &runner,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
    deploy("00000000-0000-4000-8000-000000000102", &["web"]);
    let mut cloud = |_: Cloud| -> Result<Value, RpcError> {
        panic!("read the metrics of a Service the Deployment did not deploy")
    };

    let refusal = drive(
        &store,
        in_org(),
        "service_metrics",
        json!({ "service": "worker", "deployment": 2 }),
        "metrics-1",
        &mut cloud,
    )
    .refused();

    assert_eq!(
        refusal.message,
        "That Deployment did not deploy this Service"
    );
    assert_eq!(refusal.valid, vec!["web".to_owned()]);
}

#[test]
fn a_servers_metrics_name_each_events_environment_or_its_leftover() {
    let (store, _) = deploying();
    let owned = store.read(&who(), &NamespacesQuery {}).unwrap();
    let production = owned.namespaces[0].namespace.clone();
    let mut asked = Vec::new();
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Servers { .. }) => Ok(servers(&[("web-1", 'a'), ("web-2", 'b')])),
        Cloud::Ask(CloudQuery::Metrics(query)) => {
            asked.push(query);
            Ok(metrics_answer(json!([
                {
                    "at": 150, "server": "web-2", "namespace": production, "service": "web",
                    "deployment": null, "kind": "oom",
                },
                {
                    "at": 140, "server": "web-2", "namespace": "stray", "service": "cron",
                    "deployment": null, "kind": "exit", "exit_code": 1,
                },
            ])))
        }
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => panic!("a read acts on nothing: {act:?}"),
    };

    let shown = drive(
        &store,
        in_org(),
        "server_metrics",
        json!({ "server": "web-2" }),
        "metrics-1",
        &mut cloud,
    )
    .ok();

    let [
        MetricsQuery {
            subject: MetricsSubject::Server { server },
            since: MetricsSince::MinutesAgo { minutes: 60 },
            deployment: None,
        },
    ] = asked.as_slice()
    else {
        panic!("asked {asked:?}");
    };
    assert_eq!(server.name.as_str(), "web-2");
    assert_eq!(server.machine.to_string(), "b".repeat(32));
    assert_eq!(
        shown["events"],
        json!([
            {
                "at": 150, "server": "web-2", "kind": "oom", "service": "web",
                "project": "shop", "environment": "production",
            },
            {
                "at": 140, "server": "web-2", "kind": "exit", "exit_code": 1,
                "service": "cron", "leftover": "stray",
            },
        ])
    );
}

#[test]
fn metrics_of_an_unknown_server_are_refused_before_any_are_read() {
    let store = shop();
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Servers { .. }) => Ok(servers(&[("web-1", 'a')])),
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => panic!("a read acts on nothing: {act:?}"),
    };

    let refusal = drive(
        &store,
        in_org(),
        "server_metrics",
        json!({ "server": "web-9" }),
        "metrics-1",
        &mut cloud,
    )
    .refused();

    assert_eq!(refusal.code, RpcErrorCode::NotFound);
    assert_eq!(refusal.did_you_mean.as_deref(), Some("web-1"));
}

#[test]
fn renaming_a_referenced_variable_asks_to_update_its_references() {
    let store = shop();
    for (name, env) in [
        ("web", json!({ "API_URL": "http://web:8080" })),
        ("worker", json!({ "UPSTREAM": "${{ web.API_URL }}/jobs" })),
    ] {
        call(
            &store,
            "service_add",
            json!({ "name": name, "image": "traefik/whoami" }),
            &format!("add-{name}"),
        )
        .ok();
        call(
            &store,
            "service_update",
            json!({ "service": name, "set": { "env": env } }),
            &format!("env-{name}"),
        )
        .ok();
    }

    let read = call(
        &store,
        "service_inspect",
        json!({ "service": "web" }),
        "read-1",
    )
    .ok();
    let rename = json!({
        "service": "web",
        "rename_env": { "API_URL": "PUBLIC_API_URL" },
        "expect_revision": read["revision"],
    });
    let refused = drive(
        &store,
        in_org(),
        "service_update",
        rename.clone(),
        "rename-1",
        &mut no_cloud,
    )
    .refused();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(refused.writes, Writes::None);
    assert!(refused.message.contains("worker.UPSTREAM"), "{refused:?}");
    assert_eq!(refused.details["variables"], json!(["worker.UPSTREAM"]));
    assert_eq!(refused.fix(), None, "the agent retries; no human acts");
    let mut retry = rename;
    retry["update_references"] = json!(true);
    assert_eq!(refused.retry, Call::new("service_update", retry.clone()));

    call(&store, "service_update", retry, "rename-2").ok();
    let web = call(
        &store,
        "service_inspect",
        json!({ "service": "web" }),
        "web-1",
    )
    .ok();
    assert_eq!(web["env"], json!({ "PUBLIC_API_URL": "http://web:8080" }));
    let worker = call(
        &store,
        "service_inspect",
        json!({ "service": "worker" }),
        "worker-1",
    )
    .ok();
    assert_eq!(
        worker["env"],
        json!({ "UPSTREAM": "${{ web.PUBLIC_API_URL }}/jobs" })
    );
}

// Catches service_logs telling Cloud a secret or a resolved template is plain,
// or dropping why a Server did not answer.
#[test]
fn service_logs_names_only_plain_variables_and_says_why_a_server_did_not_answer() {
    let (store, _) = deploying();
    call(
        &store,
        "service_update",
        json!({ "service": "web", "set": { "env": {
            "PLAIN": "hello-plain",
            "SHARED": { "value": "exported-plain", "exported": true },
            "KEY": { "secret": "s3cr3t-value" },
            "URL": "http://${{ web.PLAIN }}",
        } } }),
        "env-1",
    )
    .ok();
    let mut asked = Vec::new();
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Logs(query)) => {
            asked.push(query.plain);
            Ok(json!({
                "servers": [],
                "seen": servers(&[])["seen"],
                "notes": ["web-2 did not answer: log store is rebuilding"],
            }))
        }
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => panic!("logs act on nothing: {act:?}"),
    };

    let logs = drive(
        &store,
        in_org(),
        "service_logs",
        json!({ "service": "web", "deployment": 1 }),
        "logs-1",
        &mut cloud,
    )
    .ok();

    let [plain] = asked.as_slice() else {
        panic!("one logs ask: {asked:?}");
    };
    assert_eq!(plain.get("PLAIN").map(String::as_str), Some("hello-plain"));
    assert_eq!(
        plain.get("SHARED").map(String::as_str),
        Some("exported-plain")
    );
    assert!(!plain.contains_key("KEY"), "{plain:?}");
    assert_ne!(
        plain.get("URL").map(String::as_str),
        Some("http://hello-plain"),
        "a template's resolved value is never plain"
    );
    assert_eq!(
        logs["notes"],
        json!(["web-2 did not answer: log store is rebuilding"])
    );
}

// Catches server_logs asking for the wrong Server or process, or losing the
// window and line cap on the way to Cloud.
#[test]
fn server_logs_asks_for_one_servers_process_and_refuses_an_unknown_server() {
    let store = shop();
    let mut asked = Vec::new();
    let mut cloud = |cloud: Cloud| match cloud {
        Cloud::Ask(CloudQuery::Servers { .. }) => Ok(servers(&[("web-1", 'a')])),
        Cloud::Ask(CloudQuery::ServerLogs(query)) => {
            asked.push(query);
            Ok(json!({
                "servers": [{ "server": "web-1", "text": {
                    "status": "shown", "lines": ["started\n"], "cut": false, "redacted": 0,
                } }],
                "seen": servers(&[])["seen"],
                "notes": [],
            }))
        }
        Cloud::Ask(query) => panic!("unexpected read: {query:?}"),
        Cloud::Act(act) => panic!("logs act on nothing: {act:?}"),
    };
    let mut call =
        |input: Value, key: &str| drive(&store, in_org(), "server_logs", input, key, &mut cloud);

    let one = call(
        json!({ "server": "web-1", "process": "docker", "lines": 50, "since_minutes": 15 }),
        "server-logs-1",
    );
    assert_eq!(one.writes, 0);
    let one = one.ok();
    let every = call(json!({}), "server-logs-2").ok();
    let unknown = call(json!({ "server": "nope" }), "server-logs-3");

    assert_eq!(one["servers"][0]["server"], "web-1", "{one}");
    assert_eq!(every["servers"][0]["server"], "web-1", "{every}");
    assert_eq!(unknown.refused().code.as_str(), "not_found");
    let [one, every] = asked.as_slice() else {
        panic!("two server logs asks: {asked:?}");
    };
    assert_eq!(one.process, MachineLogService::Docker);
    assert_eq!(
        one.server.as_ref().map(|server| server.machine.to_string()),
        Some(machine('a'))
    );
    assert_eq!(one.lines.get(), 50);
    assert_eq!(one.since, Some(Duration::from_secs(15 * 60)));
    assert_eq!(
        every.process,
        MachineLogService::Ployz,
        "the daemon by default"
    );
    assert_eq!(every.server, None);
}

#[test]
fn docs_answer_from_the_binary_on_any_store() {
    let store = shop();
    let run = drive(
        &store,
        StoreAt::Local,
        "docs",
        json!({ "query": "add a custom domain" }),
        "docs-search",
        &mut no_cloud,
    );
    assert_eq!((run.reads, run.writes), (0, 0));
    let found = run.ok();
    assert_eq!(found["query"], "add a custom domain");
    assert_eq!(found["results"][0]["path"], "services/domains.md");
    assert_eq!(found["results"][0]["anchor"], "add-a-custom-domain");

    let page = call(
        &store,
        "docs",
        json!({ "query": "services/domains.md#add-a-custom-domain" }),
        "docs-page",
    )
    .ok();
    assert_eq!(page["path"], "services/domains.md");
    assert!(
        page["content"]
            .as_str()
            .unwrap()
            .contains("## Add a custom domain")
    );

    let refusal = call(
        &store,
        "docs",
        json!({ "query": "services/domain.md" }),
        "docs-missing",
    )
    .refused();
    assert_eq!(refusal.code, RpcErrorCode::NotFound);
    assert_eq!(refusal.did_you_mean.as_deref(), Some("services/domains.md"));
    assert!(
        refusal
            .valid
            .iter()
            .any(|path| path == "services/domains.md")
    );
}
