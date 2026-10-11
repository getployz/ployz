//! Agent tool calls driven to the end against a SQLite Config Store, the way a
//! host drives them: every step's asks answered in order, the one write run.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

use std::collections::BTreeMap;

use ployz_core::{DeployOutcome, DeployPreview, ServiceName};
use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::tools::{
    Act, Ask, Call, CloudQuery, Fix, Gate, INSTRUCTIONS, Place, Refusal, STEP_BUDGET, Step,
    StoreAt, ToolResult, WAIT_CAP_SECONDS, Write, Writes, catalog, example, lint, resume, start,
};
use ployz_store::{
    Actor, Admit, ConfigStore, CreateEnvironment, CreateProject, CreateService, CreateVolume,
    Deploy, DeploymentId, EnvironmentId, EnvironmentName, EnvironmentRef, EnvironmentsQuery,
    OrganizationId, ProjectId, ProjectName, RowTracker, RunEvidence, RunnerId, SealingKey,
    ServiceLineageId, VolumeId, VolumeName,
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
    cloud: &mut dyn FnMut(Cloud) -> Result<Value, RpcError>,
    ask: Ask,
) -> Result<Value, RpcError> {
    match ask {
        Ask::Store(query) => store
            .read(&who(), &query)
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
    let (mut step, mut state) = start(tool, input, &place(at), call_key);
    let (mut reads, mut writes, mut waited_ms) = (0, 0, 0);
    let result = loop {
        let answers = match step {
            Step::Read { asks } => {
                reads += 1;
                asks.into_iter()
                    .map(|ask| answer(store, cloud, ask))
                    .collect()
            }
            Step::Wait { ms, then } => {
                waited_ms += ms;
                then.into_iter()
                    .map(|ask| answer(store, cloud, ask))
                    .collect()
            }
            Step::Write { write } => {
                writes += 1;
                match *write {
                    Write::Store(command) => vec![
                        store
                            .write(&who(), &command)
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
        StoreAt::Cloud,
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
            "environment_show",
            "service_inspect",
            "service_add",
            "service_update",
            "service_rename",
            "service_remove",
            "domain_add",
            "domain_update",
            "domain_remove",
            "volume_add",
            "volume_update",
            "volume_remove",
            "config_write",
            "config_remove",
            "review",
            "discard",
            "publish",
            "deploy",
            "deployment_show",
            "deployment_cancel",
            "environment_pr",
            "environment_setup",
            "github_ls",
            "github_connect",
            "server_show",
            "service_status",
            "service_logs",
            "volume_show",
            "service_restart",
            "service_start",
            "service_stop",
            "server_drain",
            "server_upgrade",
            "server_clean",
            "volume_mirror",
            "volume_mirror_remove",
            "volume_move",
            "volume_release",
            "environment_remove",
            "project_remove",
            "server_remove",
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
            ("server_remove", Gate::Cloud),
            ("volume_mirror", Gate::Cloud),
            ("volume_mirror_remove", Gate::Cloud),
            ("volume_move", Gate::Cloud),
            ("volume_release", Gate::Cloud),
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
        let (step, _) = start(def.name, input, &place(StoreAt::Cloud), "call-1");
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
        lines.iter().any(|line| line == "  web.domains: added shop"),
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
        json!({
            "tool": "deployment_show",
            "input": { "deployment": deployed["deployment"], "wait_seconds": 120 },
        })
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

/// The tools that need Ployz Cloud's view of the Servers.
const CLOUD_TOOLS: [&str; 15] = [
    "server_show",
    "service_status",
    "service_logs",
    "volume_show",
    "service_restart",
    "service_start",
    "service_stop",
    "server_drain",
    "server_upgrade",
    "server_clean",
    "volume_mirror",
    "volume_mirror_remove",
    "volume_move",
    "volume_release",
    "server_remove",
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

    let refusal = drive(
        &store,
        StoreAt::Cloud,
        "server_drain",
        json!({ "server": "web-2" }),
        "drain-1",
        &mut cloud,
    )
    .refused();
    assert_eq!(refusal.writes, Writes::Unknown);
    assert_eq!(refusal.retry.tool, "server_drain");
    assert!(
        refusal.retry.input["request_key"].is_string(),
        "{refusal:?}"
    );

    let drained = drive(
        &store,
        StoreAt::Cloud,
        "server_drain",
        refusal.retry.input,
        "drain-2",
        &mut cloud,
    )
    .ok();
    assert_eq!(drained["state"], "done");
    assert_eq!(drained["at"], "web-2");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0], attempts[1], "the retry is the same attempt");
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
        StoreAt::Cloud,
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
        StoreAt::Cloud,
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
    assert_eq!(running["next"]["tool"], "deployment_show");

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
    assert_eq!(
        failed["next"],
        json!({
            "tool": "service_logs",
            "input": { "service": "web", "deployment": failed["deployment"] },
        })
    );
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
        StoreAt::Cloud,
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
