#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! PR Environments and closing Branches through `system`, on SQLite and on Postgres
//! (see `backend`): plans, out-of-order pull request facts, teardown and the sweep.

use ployz_core::config::ServiceGitAccess;
use ployz_core::{DeployOutcome, DeployPreview, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, AuthorizedRepository, Automated, BranchHead, CreateBranch, CreateGitService,
    CreateProject, DeploymentId, DeploymentStatus, EnvironmentId, EnvironmentName, EnvironmentRef,
    EnvironmentsQuery, OrganizationId, PrPlansQuery, ProjectId, ProjectName, PullRequest,
    PullRequestQuery, RunEvidence, RunnerId, ServiceId, SetPrPlan, SetupCommand, Sweep,
    SystemEvent, Trusted, Written,
};
use serde_json::json;

mod backend;

const HEAD: &str = "1111111111111111111111111111111111111111";
const DAY: i64 = 24 * 60 * 60;

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

/// Project `shop`: `production` runs Git Services `web` and `api` from `acme/web`'s
/// `main`, published.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor {
        organization: OrganizationId::parse("org").unwrap(),
    };
    store
        .create_project(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    let evidence = Trusted {
        repositories: vec![AuthorizedRepository {
            repository: "acme/web".into(),
            repository_id: 11,
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: "main".into(),
            branches: Vec::new(),
        }],
        ..Trusted::default()
    };
    for (n, name) in [(3, "web"), (4, "api")] {
        store
            .create_git_service(
                &who,
                &CreateGitService {
                    id: ServiceId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    repository: "acme/web".into(),
                    branch: None,
                },
                &evidence,
            )
            .unwrap();
    }
    store
        .write(
            &who,
            &ployz_store::Command::Publish(ployz_store::Publish {
                environment: EnvironmentRef::default(),
                version: None,
            }),
        )
        .unwrap();
    (store, who)
}

use ployz_store::ConfigStore;

fn plan(store: &ConfigStore, who: &Actor, set: SetPrPlan) {
    store.set_pr_plan(who, &set).unwrap();
}

fn on() -> SetPrPlan {
    SetPrPlan {
        project: None,
        repository: "acme/web".into(),
        enabled: Some(true),
        start_from: Some(EnvironmentName::parse("production").unwrap()),
        copy: None,
        setup: None,
        remove_on_close: None,
        include_bots: None,
    }
}

fn facts(open: bool, updated: &str) -> PullRequest {
    PullRequest {
        repository_id: 11,
        number: 5,
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: "search".into(),
        head: HEAD.into(),
        target_branch: "main".into(),
        commits: 1,
        open,
        merge_commit: None,
        updated: updated.into(),
    }
}

fn observe(store: &ConfigStore, who: &Actor, event: SystemEvent) -> Automated {
    let Written::Automated(automated) = store.system(&who.organization, &event).unwrap() else {
        panic!("a system event writes Automated")
    };
    automated
}

fn pull(store: &ConfigStore, who: &Actor, event: PullRequest) -> Automated {
    observe(store, who, SystemEvent::PullRequest(event))
}

fn sweep(store: &ConfigStore, who: &Actor, now: i64) -> Automated {
    observe(store, who, SystemEvent::Sweep(Sweep { now }))
}

fn now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

/// Each Environment's name, `<` naming its Parent.
fn listed(store: &ConfigStore, who: &Actor) -> Vec<String> {
    store
        .environments(who, &EnvironmentsQuery::default())
        .unwrap()
        .environments
        .into_iter()
        .map(|listing| match listing.parent {
            Some(parent) => format!("{}<{parent}", listing.name),
            None => listing.name.to_string(),
        })
        .collect()
}

fn runner() -> RunnerId {
    RunnerId::parse("runner").unwrap()
}

/// Run Deployment `id` to `applied`, touching `services`.
fn run(store: &ConfigStore, id: &DeploymentId, services: &[&str]) {
    let claimed = store.claim(id, &runner()).unwrap();
    let operation = |index: usize| {
        json!({"type": "remove_container", "machine_id": "a".repeat(32),
               "container_id": format!("{index:x}").repeat(64)})
    };
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace,
        "operations": services.iter().enumerate().map(|(index, name)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": name,
            "operation": operation(index), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(id, &runner(), RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: DeployOutcome<ployz_core::ExecutionError> = serde_json::from_value(json!({
        "type": "success",
        "completed": (0..services.len()).map(operation).collect::<Vec<_>>()
    }))
    .unwrap();
    store
        .record(
            id,
            &runner(),
            RunEvidence::Executed {
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

fn remove(store: &ConfigStore, who: &Actor, environment: &str) -> DeploymentId {
    let id = DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .admit(
            who,
            &Admit {
                id: id.clone(),
                environment: at(environment),
                services: Vec::new(),
                version: None,
                upload: None,
                retry: None,
                remove: true,
                accept_volume_loss: Vec::new(),
            },
            &Trusted::default(),
        )
        .unwrap();
    id
}

#[test]
fn plans_name_nodes_of_the_start_from_environment() {
    let (store, who) = shop();
    let view = store.pr_plans(&who, &PrPlansQuery::default()).unwrap();
    assert_eq!(view.plans.len(), 1);
    assert!(!view.plans[0].enabled && view.plans[0].remove_on_close);
    assert_eq!(view.plans[0].start_from, None);

    // Copies and Setup Commands name the start-from's nodes, so it comes first.
    let setup = vec![SetupCommand {
        service: ServiceName::parse("api").unwrap(),
        command: " php artisan migrate ".into(),
    }];
    let early = store
        .set_pr_plan(
            &who,
            &SetPrPlan {
                setup: Some(setup.clone()),
                start_from: None,
                ..on()
            },
        )
        .unwrap_err();
    assert_eq!(early.code, RpcErrorCode::InvalidArgument);
    let missing = store
        .set_pr_plan(
            &who,
            &SetPrPlan {
                repository: "acme/nope".into(),
                ..on()
            },
        )
        .unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::NotFound);

    plan(
        &store,
        &who,
        SetPrPlan {
            setup: Some(setup),
            include_bots: Some(true),
            ..on()
        },
    );
    // Only the fields given change.
    plan(
        &store,
        &who,
        SetPrPlan {
            enabled: None,
            start_from: None,
            remove_on_close: Some(false),
            ..on()
        },
    );
    let view = store.pr_plans(&who, &PrPlansQuery::default()).unwrap();
    let plan = &view.plans[0];
    assert_eq!(plan.repository, "acme/web");
    assert!(plan.enabled && plan.include_bots && !plan.remove_on_close);
    assert_eq!(
        plan.start_from.as_ref().map(ToString::to_string).as_deref(),
        Some("production")
    );
    assert_eq!(plan.setup[0].command, "php artisan migrate");
}

#[test]
fn an_opened_pull_request_gets_a_deployed_pr_environment_once() {
    let (store, who) = shop();
    // Off: nothing happens, but the facts are kept.
    assert_eq!(
        pull(&store, &who, facts(true, "2026-09-29T10:00:00Z")),
        Automated::default()
    );

    plan(&store, &who, on());
    let opened = pull(&store, &who, facts(true, "2026-09-29T10:00:01Z"));
    assert_eq!(opened.admitted.len(), 1, "{opened:?}");
    assert_eq!(opened.checks.len(), 1);
    assert_eq!(listed(&store, &who), ["pr-5<production", "production"]);
    // Its Git Services track the head branch and run one replica, pinned to the head.
    let branch = store
        .branch(
            &who,
            &ployz_store::BranchQuery {
                environment: at("pr-5"),
            },
        )
        .unwrap();
    assert_eq!(branch.parent.to_string(), "production");
    let services = store
        .services(
            &who,
            &ployz_store::ServicesQuery {
                environment: at("pr-5"),
            },
        )
        .unwrap();
    assert_eq!(services.services.len(), 2);
    let deployment = store
        .deployment(&who, &opened.admitted[0].deployment.id)
        .unwrap();
    assert_eq!(deployment.deployment.status, DeploymentStatus::Queued);
    let sources = store.sources(&opened.admitted[0].deployment.id).unwrap();
    assert!(
        sources
            .iter()
            .all(|source| source.commit.as_deref() == Some(HEAD)
                && source.branch.as_deref() == Some("search"))
    );

    // A replay, or a later synchronize, makes no second one; a bot's needs the plan's say.
    let again = pull(&store, &who, facts(true, "2026-09-29T10:00:02Z"));
    assert!(again.admitted.is_empty() && again.checks.len() == 1);
    assert_eq!(listed(&store, &who).len(), 2);
    let view = store
        .pull_request(
            &who,
            &PullRequestQuery {
                repository_id: 11,
                number: 5,
            },
        )
        .unwrap();
    assert_eq!(view.environments.len(), 1);
    assert_eq!(
        view.environments[0].destinations[0].name.to_string(),
        "production"
    );
    // Tracking the head branch is its own; nothing waits to be saved.
    assert!(view.passing, "{view:?}");
    assert_eq!(view.reason, "No changes for production");
    store
        .write(
            &who,
            &ployz_store::Command::Edit(ployz_store::Edit {
                environment: at("pr-5"),
                expect: None,
                changes: vec![ployz_store::Change::Set {
                    path: ployz_store::SettingPath::parse("web.startCommand").unwrap(),
                    value: json!("npm run serve"),
                }],
            }),
        )
        .unwrap();
    let view = store
        .pull_request(
            &who,
            &PullRequestQuery {
                repository_id: 11,
                number: 5,
            },
        )
        .unwrap();
    assert!(!view.passing);
    assert_eq!(view.reason, "1 change to save in Ployz");
}

#[test]
fn a_late_open_never_restores_a_closed_pull_request() {
    let (store, who) = shop();
    plan(&store, &who, on());
    let opened = pull(&store, &who, facts(true, "2026-09-29T10:00:00Z"));
    let closed = pull(&store, &who, facts(false, "2026-09-29T11:00:00Z"));
    // Never deployed: the queued Deployment is cancelled and the PR Environment deleted at once.
    assert_eq!(closed.removed.len(), 1, "{closed:?}");
    assert_eq!(listed(&store, &who), ["production"]);
    let cancelled = store.deployment(&who, &opened.admitted[0].deployment.id);
    assert!(cancelled.is_err(), "deleted with its Environment");

    // The open GitHub sent first arrives last.
    assert_eq!(
        pull(&store, &who, facts(true, "2026-09-29T10:30:00Z")),
        Automated::default()
    );
    assert_eq!(listed(&store, &who), ["production"]);
    // Reopened: a new one.
    let reopened = pull(&store, &who, facts(true, "2026-09-29T12:00:00Z"));
    assert_eq!(reopened.admitted.len(), 1);
    assert_eq!(listed(&store, &who), ["pr-5<production", "production"]);
}

#[test]
fn a_closed_pull_request_leaves_the_servers_before_the_store() {
    let (store, who) = shop();
    plan(&store, &who, on());
    let opened = pull(&store, &who, facts(true, "2026-09-29T10:00:00Z"));
    run(&store, &opened.admitted[0].deployment.id, &["web", "api"]);

    // A renamed head branch: it tracks the new name, in Working and Saved State.
    let mut renamed = facts(true, "2026-09-29T10:10:00Z");
    renamed.head_branch = "search-v2".into();
    pull(&store, &who, renamed);
    let redeploy = DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .admit(
            &who,
            &Admit {
                id: redeploy.clone(),
                environment: at("pr-5"),
                services: Vec::new(),
                version: None,
                upload: None,
                retry: None,
                remove: false,
                accept_volume_loss: Vec::new(),
            },
            &Trusted::default(),
        )
        .unwrap();
    assert!(
        store
            .sources(&redeploy)
            .unwrap()
            .iter()
            .all(|source| source.branch.as_deref() == Some("search-v2"))
    );
    run(&store, &redeploy, &["web", "api"]);

    let mut closed = facts(false, "2026-09-29T11:00:00Z");
    closed.head_branch = "search-v2".into();
    let closing = pull(&store, &who, closed);
    assert_eq!(closing.closing.len(), 1, "{closing:?}");
    assert_eq!(closing.closing[0].name.to_string(), "pr-5");
    // Closing: a push leaves it be.
    let pushed = observe(
        &store,
        &who,
        SystemEvent::BranchHead(BranchHead {
            repository_id: 11,
            branch: "search-v2".into(),
            base: None,
            head: Some("2".repeat(40)),
            changed: None,
        }),
    );
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    // Cloud admits the removal; until it applied, the sweep waits.
    let removal = remove(&store, &who, "pr-5");
    assert!(sweep(&store, &who, now()).removed.is_empty());
    run(&store, &removal, &[]);
    let swept = sweep(&store, &who, now());
    assert_eq!(swept.removed.len(), 1);
    assert_eq!(listed(&store, &who), ["production"]);
}

#[test]
fn a_kept_open_pull_request_stays_and_a_shut_down_one_stays_off() {
    let (store, who) = shop();
    plan(
        &store,
        &who,
        SetPrPlan {
            remove_on_close: Some(false),
            ..on()
        },
    );
    let opened = pull(&store, &who, facts(true, "2026-09-29T10:00:00Z"));
    run(&store, &opened.admitted[0].deployment.id, &["web", "api"]);
    // Shut down: removed from the Servers, every row kept, and a push leaves it off.
    let off = remove(&store, &who, "pr-5");
    run(&store, &off, &[]);
    let pushed = observe(
        &store,
        &who,
        SystemEvent::BranchHead(BranchHead {
            repository_id: 11,
            branch: "search".into(),
            base: None,
            head: Some("2".repeat(40)),
            changed: None,
        }),
    );
    assert!(pushed.admitted.is_empty(), "{pushed:?}");
    let closed = pull(&store, &who, facts(false, "2026-09-29T11:00:00Z"));
    assert!(closed.closing.is_empty() && closed.removed.is_empty());
    assert_eq!(listed(&store, &who), ["pr-5<production", "production"]);
}

#[test]
fn idle_branches_close_after_a_week_unless_kept() {
    let (store, who) = shop();
    for (n, name, keep) in [
        (20, "idle", false),
        (21, "kept", true),
        (22, "fresh", false),
    ] {
        store
            .create_branch(
                &who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(n)).unwrap(),
                    from: EnvironmentRef::default(),
                    name: EnvironmentName::parse(name).unwrap(),
                    copy: vec!["web".into()],
                    live: Vec::new(),
                    setup: Vec::new(),
                    keep,
                    fix: None,
                },
            )
            .unwrap();
    }
    for name in ["idle", "kept"] {
        let id = DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
        store
            .admit(
                &who,
                &Admit {
                    id: id.clone(),
                    environment: at(name),
                    services: Vec::new(),
                    version: None,
                    upload: None,
                    retry: None,
                    remove: false,
                    accept_volume_loss: Vec::new(),
                },
                &Trusted::default(),
            )
            .unwrap();
        run(&store, &id, &["web"]);
    }
    // Six days on, nothing; eight days on, the idle one closes. `fresh` never deployed.
    let early = sweep(&store, &who, now() + 6 * DAY);
    assert!(early.closing.is_empty() && early.removed.is_empty());
    let late = sweep(&store, &who, now() + 8 * DAY);
    assert_eq!(
        late.closing
            .iter()
            .map(|summary| summary.name.to_string())
            .collect::<Vec<_>>(),
        ["idle"]
    );
    // Asked again, it's still closing until Cloud's removal applied.
    let again = sweep(&store, &who, now() + 8 * DAY);
    assert_eq!(again.closing.len(), 1);
    let removal = remove(&store, &who, "idle");
    run(&store, &removal, &[]);
    sweep(&store, &who, now() + 8 * DAY);
    assert_eq!(
        listed(&store, &who),
        ["fresh<production", "kept<production", "production"]
    );
}
