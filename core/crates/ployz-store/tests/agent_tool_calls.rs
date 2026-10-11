//! Agent tool calls driven to the end against a SQLite Config Store, the way a
//! host drives them: every step's asks answered in order, the one write run.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

use std::collections::BTreeMap;

use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::tools::{
    Ask, Call, Fix, Gate, INSTRUCTIONS, Place, Refusal, STEP_BUDGET, Step, StoreAt, ToolResult,
    WAIT_CAP_SECONDS, Write, Writes, catalog, example, lint, resume, start,
};
use ployz_store::{
    Actor, ConfigStore, CreateProject, EnvironmentId, OrganizationId, ProjectId, ProjectName,
    SealingKey,
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

fn answer(store: &ConfigStore, ask: Ask) -> Result<Value, RpcError> {
    match ask {
        Ask::Store(query) => store
            .read(&who(), &query)
            .map(|view| serde_json::to_value(view).unwrap()),
        Ask::Cloud(query) => panic!("no Cloud in these tests: {query:?}"),
    }
}

/// Drive `tool` to its result, holding every call to the step machine's rules.
fn call(store: &ConfigStore, tool: &str, input: Value, call_key: &str) -> Run {
    let (mut step, mut state) = start(tool, input.clone(), &place(StoreAt::Cloud), call_key);
    let (mut reads, mut writes, mut waited_ms) = (0, 0, 0);
    let result = loop {
        let answers = match step {
            Step::Read { asks } => {
                reads += 1;
                asks.into_iter().map(|ask| answer(store, ask)).collect()
            }
            Step::Wait { ms, then } => {
                waited_ms += ms;
                then.into_iter().map(|ask| answer(store, ask)).collect()
            }
            Step::Write { write } => {
                writes += 1;
                match *write {
                    Write::Store(command) => vec![
                        store
                            .write(&who(), &command)
                            .map(|written| serde_json::to_value(written).unwrap()),
                    ],
                    Write::Act(act) => panic!("no Cloud in these tests: {act:?}"),
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
    if let ToolResult::Refused(refusal) = &result {
        assert_eq!(refusal.retry.tool, tool, "a retry calls the same tool");
        if refusal.code != RpcErrorCode::ConfirmationRequired && refusal.writes != Writes::Unknown {
            assert_eq!(refusal.retry.input, input, "a retry repeats the input");
        }
    }
    Run {
        result,
        reads,
        writes,
        waited_ms,
    }
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
        ]
    );
}

#[test]
fn only_publish_and_deploy_ask_a_human() {
    let gated = catalog()
        .into_iter()
        .filter(|def| def.gate != Gate::Never)
        .map(|def| (def.name, def.gate))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        gated,
        BTreeMap::from([("deploy", Gate::Store), ("publish", Gate::Store)])
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
