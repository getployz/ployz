#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! PR Environments and closing Branches through `system`, on SQLite and on Postgres
//! (see `backend`): plans, out-of-order pull request facts, teardown and the sweep.

use ployz_core::config::ServiceGitAccess;
use ployz_core::{DeployOutcome, DeployPreview, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, AuthorizedRepository, Automated, BranchHead, Change, CreateBranch,
    CreateGitService, CreateProject, CreateService, Deploy, DeploymentId, DeploymentStatus, Edit,
    EnvironmentId, EnvironmentName, EnvironmentRef, EnvironmentsQuery, OrganizationId,
    PrPlansQuery, ProjectId, ProjectName, PullRequest, PullRequestQuery, Removal, RunEvidence,
    RunnerId, ServiceLineageId, SetPrPlan, SettingPath, SetupCommand, Sweep, SystemEvent, Trusted,
    Written,
};
use serde_json::json;

mod backend;

/// A node by its name: `SERVICE`, or `volumes.VOLUME`.
fn node(name: &str) -> ployz_store::NodeName {
    ployz_store::NodeName::parse(name).unwrap()
}

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
    shop_on(backend::open())
}

/// `shop` on `store`.
fn shop_on(store: ConfigStore) -> (ConfigStore, Actor) {
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
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
            repository: backend::repo_name("acme/web"),
            repository_id: backend::repo_id(11),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: backend::git_branch("main"),
            branches: Vec::new(),
        }],
        ..Trusted::default()
    };
    for (n, name) in [(3, "web"), (4, "api")] {
        store
            .write_trusted(
                &who,
                &CreateGitService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    repository: backend::repo_name("acme/web"),
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
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    (store, who)
}

use ployz_store::ConfigStore;

fn plan(store: &ConfigStore, who: &Actor, set: SetPrPlan) {
    store.write(who, &set).unwrap();
}

fn on() -> SetPrPlan {
    SetPrPlan {
        project: None,
        repository: backend::repo_name("acme/web"),
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
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: backend::git_branch("search"),
        head: backend::sha(HEAD),
        target_branch: backend::git_branch("main"),
        commits: 1,
        open,
        merge_commit: None,
        merge_reached: None,
        updated: ployz_store::GithubTimestamp::parse(updated).unwrap(),
    }
}

fn observe(store: &ConfigStore, who: &Actor, event: SystemEvent) -> Automated {
    let Written::Automated(automated) = store
        .system(&who.organization, &event, &Trusted::default())
        .unwrap()
    else {
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
        .read(who, &EnvironmentsQuery::default())
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
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

fn remove(store: &ConfigStore, who: &Actor, environment: &str) -> DeploymentId {
    let id = DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .write_trusted(
            who,
            &Admit::Remove(Removal {
                id: id.clone(),
                environment: at(environment),
                version: None,
                accept_volume_loss: Vec::new(),
                close: false,
            }),
            &Trusted::default(),
        )
        .unwrap();
    id
}

#[test]
fn plans_name_nodes_of_the_start_from_environment() {
    let (store, who) = shop();
    let view = store.read(&who, &PrPlansQuery::default()).unwrap();
    assert_eq!(view.plans.len(), 1);
    assert!(!view.plans[0].enabled && view.plans[0].remove_on_close);
    assert_eq!(view.plans[0].start_from, None);

    // Copies and Setup Commands name the start-from's nodes, so it comes first.
    let setup = vec![SetupCommand {
        service: ServiceName::parse("api").unwrap(),
        command: " php artisan migrate ".into(),
    }];
    let early = store
        .write(
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
        .write(
            &who,
            &SetPrPlan {
                repository: backend::repo_name("acme/nope"),
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
    let view = store.read(&who, &PrPlansQuery::default()).unwrap();
    let plan = &view.plans[0];
    assert_eq!(plan.repository.as_str(), "acme/web");
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
        .read(
            &who,
            &ployz_store::BranchQuery {
                environment: at("pr-5"),
            },
        )
        .unwrap();
    assert_eq!(branch.parent.to_string(), "production");
    assert_eq!(
        branch
            .pull_request
            .map(|pr| (pr.repository_id.get(), pr.number.get())),
        Some((11, 5))
    );
    // The plan lists it as open, by the facts Cloud reported.
    let plans = store.read(&who, &PrPlansQuery::default()).unwrap();
    let open = &plans.plans[0].open;
    assert_eq!(plans.plans[0].repository_id.get(), 11);
    assert_eq!(open.len(), 1);
    assert_eq!(
        (open[0].number.get(), open[0].environment.to_string()),
        (5, "pr-5".into())
    );
    let services = store
        .read(
            &who,
            &ployz_store::ServicesQuery {
                environment: at("pr-5"),
            },
        )
        .unwrap();
    assert_eq!(services.services.len(), 2);
    let deployment = store
        .read(
            &who,
            &ployz_store::DeploymentQuery {
                id: opened.admitted[0].deployment.id.clone(),
            },
        )
        .unwrap();
    assert_eq!(deployment.deployment.status, DeploymentStatus::Queued);
    let sources = store.sources(&opened.admitted[0].deployment.id).unwrap();
    assert!(
        sources
            .iter()
            .all(|source| source.commit == Some(backend::sha(HEAD))
                && source.branch == Some(backend::git_branch("search")))
    );

    // A replay, or a later synchronize, makes no second one; a bot's needs the plan's say.
    let again = pull(&store, &who, facts(true, "2026-09-29T10:00:02Z"));
    assert!(again.admitted.is_empty() && again.checks.len() == 1);
    assert_eq!(listed(&store, &who).len(), 2);
    let view = store
        .read(
            &who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
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
        .read(
            &who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap();
    assert!(!view.passing);
    assert_eq!(view.reason, "1 change to sync in Ployz");
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
    let cancelled = store.read(
        &who,
        &ployz_store::DeploymentQuery {
            id: opened.admitted[0].deployment.id.clone(),
        },
    );
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
    renamed.head_branch = backend::git_branch("search-v2");
    pull(&store, &who, renamed);
    let redeploy = DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: redeploy.clone(),
                environment: at("pr-5"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    assert!(
        store
            .sources(&redeploy)
            .unwrap()
            .iter()
            .all(|source| source.branch == Some(backend::git_branch("search-v2")))
    );
    run(&store, &redeploy, &["web", "api"]);

    let mut closed = facts(false, "2026-09-29T11:00:00Z");
    closed.head_branch = backend::git_branch("search-v2");
    let closing = pull(&store, &who, closed);
    assert_eq!(closing.closing.len(), 1, "{closing:?}");
    assert_eq!(closing.closing[0].name.to_string(), "pr-5");
    // Closing: a push leaves it be.
    let pushed = observe(
        &store,
        &who,
        SystemEvent::BranchHead(BranchHead {
            repository_id: backend::repo_id(11),
            branch: backend::git_branch("search-v2"),
            base: None,
            head: Some(backend::sha(&"2".repeat(40))),
            changed: None,
            merged: Vec::new(),
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
fn a_kept_open_pull_request_stays_and_a_push_brings_a_shut_down_one_back() {
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
    // Shut down: removed from the Servers, every row kept; a push turns it back on,
    // all of it.
    let off = remove(&store, &who, "pr-5");
    run(&store, &off, &[]);
    let pushed = observe(
        &store,
        &who,
        SystemEvent::BranchHead(BranchHead {
            repository_id: backend::repo_id(11),
            branch: backend::git_branch("search"),
            base: None,
            head: Some(backend::sha(&"2".repeat(40))),
            changed: None,
            merged: Vec::new(),
        }),
    );
    assert_eq!(pushed.admitted.len(), 1, "{pushed:?}");
    let back = &pushed.admitted[0].deployment;
    assert!(back.services.is_empty() && !back.remove, "{back:?}");
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
            .write(
                &who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(n)).unwrap(),
                    from: EnvironmentRef::default(),
                    name: EnvironmentName::parse(name).unwrap(),
                    copy: vec![node("web")],
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
            .write_trusted(
                &who,
                &Admit::Deploy(Deploy {
                    id: id.clone(),
                    environment: at(name),
                    services: Vec::new(),
                    version: None,
                    upload: None,
                    accept_volume_loss: Vec::new(),
                    message: None,
                }),
                &Trusted::default(),
            )
            .unwrap();
        run(&store, &id, &["web"]);
    }
    // The Branch view says when: a week after its Deploy, and never for the others.
    let closes_at = |name: &str| {
        store
            .read(
                &who,
                &ployz_store::BranchQuery {
                    environment: at(name),
                },
            )
            .unwrap()
            .closes_at
    };
    let idle = closes_at("idle").unwrap();
    assert!((now() + 7 * DAY - idle).abs() < 60);
    assert_eq!((closes_at("kept"), closes_at("fresh")), (None, None));
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

/// Admit a Deploy of `services` of `environment` and run it.
fn deploy(store: &ConfigStore, who: &Actor, environment: &str, services: &[&str]) {
    let id = DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .write_trusted(
            who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: at(environment),
                services: services
                    .iter()
                    .map(|name| ServiceName::parse(*name).unwrap())
                    .collect(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    run(store, &id, services);
}

/// Cloud vouching that `acme/web` has `main` and `release`.
fn vouched() -> Trusted {
    Trusted {
        repositories: vec![AuthorizedRepository {
            repository: backend::repo_name("acme/web"),
            repository_id: backend::repo_id(11),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: backend::git_branch("main"),
            branches: vec![backend::git_branch("release")],
        }],
        ..Trusted::default()
    }
}

fn set(store: &ConfigStore, who: &Actor, environment: &str, path: &str, value: serde_json::Value) {
    store
        .write_trusted(
            who,
            &Edit {
                environment: at(environment),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse(path).unwrap(),
                    value,
                }],
            },
            &vouched(),
        )
        .unwrap();
}

fn image_service(store: &ConfigStore, who: &Actor, environment: &str, n: u8, name: &str) {
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(n)).unwrap(),
                environment: at(environment),
                name: ServiceName::parse(name).unwrap(),
                image: Some(format!("{name}:1")),
                template: None,
            },
        )
        .unwrap();
}

#[test]
fn a_destinations_count_is_the_rows_its_sync_offers_where_nodes_are_used_live() {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    // production deploys `release`, and runs `cache`, which `site` uses.
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(3)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("site").unwrap(),
                repository: backend::repo_name("acme/web"),
                branch: Some(backend::git_branch("release")),
            },
            &vouched(),
        )
        .unwrap();
    image_service(&store, &who, "production", 4, "cache");
    set(
        &store,
        &who,
        "production",
        "site.env.CACHE_URL",
        json!("${{ cache.PLOYZ_PRIVATE_DOMAIN }}"),
    );
    deploy(&store, &who, "production", &["cache"]);

    // pr-5 copies `site` and uses `cache` live; then it adds and runs its own `db`.
    plan(&store, &who, on());
    let opened = pull(&store, &who, facts(true, "2026-09-29T10:00:00Z"));
    run(&store, &opened.admitted[0].deployment.id, &["site"]);
    image_service(&store, &who, "pr-5", 5, "db");
    set(
        &store,
        &who,
        "pr-5",
        "site.env.DB_URL",
        json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"),
    );
    deploy(&store, &who, "pr-5", &["db"]);
    // Its Branch `qa` deploys `main`, so it is the Destination; it uses `db` live.
    store
        .write(
            &who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(6)).unwrap(),
                from: at("pr-5"),
                name: EnvironmentName::parse("qa").unwrap(),
                copy: vec![node("site")],
                live: Vec::new(),
                setup: Vec::new(),
                keep: true,
                fix: None,
            },
        )
        .unwrap();
    set(&store, &who, "qa", "site.branch", json!("main"));
    store
        .write(
            &who,
            &ployz_store::Command::Publish(ployz_store::Publish {
                environment: at("qa"),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    set(&store, &who, "pr-5", "site.env.MODE", json!("fast"));

    // `db` stays where qa uses it live: the page counts what the review offers.
    let review = store
        .read(
            &who,
            &ployz_store::SyncQuery {
                from: at("pr-5"),
                into: None,
                when: Some(ployz_store::When::AtMerge),
            },
        )
        .unwrap();
    assert_eq!(review.into.name.as_str(), "qa");
    let rows: Vec<String> = review.rows.iter().map(|row| row.at.to_string()).collect();
    assert_eq!(rows, ["site.env.MODE"]);
    let view = store
        .read(
            &who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap();
    let destinations = &view.environments[0].destinations;
    assert_eq!(destinations.len(), 1);
    assert_eq!(destinations[0].name.as_str(), "qa");
    assert_eq!(destinations[0].changes, review.rows.len());
    assert_eq!(view.reason, "1 change to sync in Ployz");
}

/// Run `sql` on the database at `url`, beside the Store.
fn execute(url: &str, sql: &str) -> usize {
    match url.strip_prefix("sqlite:") {
        Some(path) => rusqlite::Connection::open(path)
            .unwrap()
            .execute(sql, [])
            .unwrap(),
        None => usize::try_from(
            postgres::Client::connect(url, postgres::NoTls)
                .unwrap()
                .execute(sql, &[])
                .unwrap(),
        )
        .unwrap(),
    }
}

fn now_when() -> ployz_store::When {
    ployz_store::When::Now { close_after: false }
}

/// Include what `pr-5` offers into production as Sync `n`; the proposal, or the refusal.
fn include_pr(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
) -> Result<ployz_store::ProposalId, (RpcErrorCode, String)> {
    include_from_pr(store, who, "production", Some(now_when()), n)
}

/// Include what `pr-5` offers into `into` as Sync `n`, saying `when` as the Dashboard
/// does; the proposal, or the refusal.
fn include_from_pr(
    store: &ConfigStore,
    who: &Actor,
    into: &str,
    when: Option<ployz_store::When>,
    n: u8,
) -> Result<ployz_store::ProposalId, (RpcErrorCode, String)> {
    let view = store
        .read(
            who,
            &ployz_store::SyncQuery {
                from: at("pr-5"),
                into: Some(at(into)),
                when,
            },
        )
        .unwrap();
    let synced = store
        .write(
            who,
            &ployz_store::SyncChanges {
                from: at("pr-5"),
                into: Some(at(into)),
                when,
                version: view.version,
                picks: None,
                skip: Vec::new(),
                values: std::collections::BTreeMap::new(),
                id: Some(
                    ployz_store::SyncId::parse(format!("00000000-0000-4000-8000-0000000002{n:02}"))
                        .unwrap(),
                ),
            },
        )
        .map_err(|error| (error.code, error.message))?;
    let ployz_store::SyncedWhen::Now { proposal, .. } = synced.when else {
        panic!("an Include stages now")
    };
    Ok(proposal)
}

/// Included into one of its Destinations, a pull request's preview is the pull
/// request's proposal; included anywhere else, it is that Environment's, as any
/// other source is.
#[test]
fn a_preview_is_its_pull_requests_proposal_only_in_a_destination() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(6)).unwrap(),
                from: EnvironmentRef::default(),
                name: EnvironmentName::parse("staging").unwrap(),
                copy: vec![node("web")],
                live: Vec::new(),
                setup: Vec::new(),
                keep: true,
                fix: None,
            },
        )
        .unwrap();
    plan(&store, &who, on());
    pull(&store, &who, facts(true, "2026-09-29T10:00:00Z"));
    set(&store, &who, "pr-5", "web.env.X", json!("1"));
    include_from_pr(&store, &who, "staging", None, 1).unwrap();
    include_pr(&store, &who, 2).unwrap();

    let source = |environment: &str| {
        let diff = store
            .read(
                &who,
                &ployz_store::DiffQuery {
                    environment: at(environment),
                },
            )
            .unwrap();
        let [included] = &diff.included[..] else {
            panic!("one proposal: {:?}", diff.included)
        };
        included.source.clone()
    };
    assert!(
        matches!(&source("staging"), ployz_store::ProposalSource::Environment { name, .. } if name == "pr-5"),
        "{:?}",
        source("staging")
    );
    assert!(
        matches!(
            source("production"),
            ployz_store::ProposalSource::PullRequest { number, .. } if number.get() == 5
        ),
        "{:?}",
        source("production")
    );
}

/// `pr-5`'s facts at `updated`, open and targeting `branch`.
fn targeting(branch: &str, updated: &str) -> PullRequest {
    PullRequest {
        target_branch: backend::git_branch(branch),
        ..facts(true, updated)
    }
}

/// The one proposal included in production's draft.
fn included_in_production(store: &ConfigStore, who: &Actor) -> ployz_store::Included {
    let diff = store
        .read(
            who,
            &ployz_store::DiffQuery {
                environment: at("production"),
            },
        )
        .unwrap();
    let [included] = &diff.included[..] else {
        panic!("one proposal: {:?}", diff.included)
    };
    included.clone()
}

/// Remove `proposal` from production's draft.
fn remove_from_production(store: &ConfigStore, who: &Actor, proposal: ployz_store::ProposalId) {
    let removed = store
        .write(
            who,
            &ployz_store::RemoveProposal {
                environment: at("production"),
                proposal,
                version: None,
            },
        )
        .unwrap();
    assert!(removed.removed);
}

/// Production's `web` env now, if it has any.
fn production_env(store: &ConfigStore, who: &Actor) -> Option<serde_json::Value> {
    store
        .read(
            who,
            &ployz_store::ServiceQuery {
                environment: at("production"),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap()
        .values
        .get("env")
        .cloned()
}

/// A preview included before its pull request targeted the draft's branch is still
/// one proposal there once it does: the next Include makes it the pull request's.
#[test]
fn a_preview_included_before_it_was_destined_becomes_its_pull_requests_proposal() {
    let (store, who) = shop();
    plan(&store, &who, on());
    pull(&store, &who, targeting("release", "2026-09-29T10:00:00Z"));
    set(&store, &who, "pr-5", "web.env.X", json!("1"));
    let first = include_pr(&store, &who, 1).unwrap();

    pull(&store, &who, targeting("main", "2026-09-29T11:00:00Z"));
    set(&store, &who, "pr-5", "web.env.Y", json!("2"));
    assert_eq!(include_pr(&store, &who, 2), Ok(first.clone()));
    let included = included_in_production(&store, &who);
    assert!(
        matches!(
            included.source,
            ployz_store::ProposalSource::PullRequest { number, .. } if number.get() == 5
        ),
        "{:?}",
        included.source
    );
    assert_eq!(included.changes, 2);

    remove_from_production(&store, &who, first);
    assert_eq!(production_env(&store, &who), None);
}

/// A pull request's proposal in a draft that stops being one of its Destinations is
/// its preview's own from the next Include on.
#[test]
fn a_pull_requests_proposal_outside_its_destinations_becomes_its_previews() {
    let (store, who) = shop();
    plan(&store, &who, on());
    pull(&store, &who, facts(true, "2026-09-29T10:00:00Z"));
    set(&store, &who, "pr-5", "web.env.X", json!("1"));
    let first = include_pr(&store, &who, 1).unwrap();

    pull(&store, &who, targeting("release", "2026-09-29T11:00:00Z"));
    set(&store, &who, "pr-5", "web.env.Y", json!("2"));
    assert_eq!(include_pr(&store, &who, 2), Ok(first.clone()));
    let included = included_in_production(&store, &who);
    assert!(
        matches!(&included.source, ployz_store::ProposalSource::Environment { name, .. } if name == "pr-5"),
        "{:?}",
        included.source
    );
    assert_eq!(included.changes, 2);

    remove_from_production(&store, &who, first);
    assert_eq!(production_env(&store, &who), None);
}

/// A preview's change included in its Parent's draft is still its own: until the
/// Parent saves it, its Sync into its Destination carries it.
#[test]
fn a_change_included_in_the_previews_parent_still_syncs_to_its_destination() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(6)).unwrap(),
                from: EnvironmentRef::default(),
                name: EnvironmentName::parse("staging").unwrap(),
                copy: vec![node("web")],
                live: Vec::new(),
                setup: Vec::new(),
                keep: true,
                fix: None,
            },
        )
        .unwrap();
    plan(
        &store,
        &who,
        SetPrPlan {
            start_from: Some(EnvironmentName::parse("staging").unwrap()),
            ..on()
        },
    );
    pull(&store, &who, facts(true, "2026-09-29T10:00:00Z"));
    set(&store, &who, "pr-5", "web.env.X", json!("1"));
    include_from_pr(&store, &who, "staging", None, 1).unwrap();

    let view = store
        .read(
            &who,
            &ployz_store::SyncQuery {
                from: at("pr-5"),
                into: Some(at("production")),
                when: None,
            },
        )
        .unwrap();
    let ticked: Vec<_> = view
        .rows
        .iter()
        .filter(|row| row.ticked)
        .map(|row| row.at.to_string())
        .collect();
    assert_eq!(ticked, ["web.env.X"], "{:?}", view.rows);
}

/// A pull request's recreated preview finds the proposal its first one made, unless
/// a row the proposal owns already arrived from the new preview.
#[test]
fn a_recreated_preview_rebinds_its_proposal_and_refuses_a_colliding_row() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_on(ConfigStore::open(&url, backend::key()).unwrap());
    plan(&store, &who, on());
    pull(&store, &who, facts(true, "2026-09-29T10:00:00Z"));
    set(&store, &who, "pr-5", "web.env.X", json!("1"));
    let first = include_pr(&store, &who, 1).unwrap();
    // Closed before it deployed: the preview is deleted at once, the proposal stays.
    pull(&store, &who, facts(false, "2026-09-29T11:00:00Z"));
    assert_eq!(listed(&store, &who), ["production"]);
    pull(&store, &who, facts(true, "2026-09-29T12:00:00Z"));
    set(&store, &who, "pr-5", "web.env.Y", json!("2"));

    // A row the proposal owns arrived from the new preview too: rebinding would
    // merge the two, so Include refuses and names it.
    let collide = format!(
        "INSERT INTO config_sync_arrival \
         (environment_id, other_id, lineage, at, organization_id, how, state, value) \
         SELECT environment_id, (SELECT id FROM config_environment WHERE name = 'pr-5'), \
         lineage, at, organization_id, how, 'settled', value \
         FROM config_sync_arrival WHERE proposal_id = '{first}'"
    );
    assert_eq!(execute(&url, &collide), 1);
    let (code, message) = include_pr(&store, &who, 2).unwrap_err();
    assert_eq!(code, RpcErrorCode::Conflict, "{message}");
    assert!(message.contains("web.env.X"), "{message}");

    execute(
        &url,
        "DELETE FROM config_sync_arrival WHERE proposal_id IS NULL \
         AND other_id = (SELECT id FROM config_environment WHERE name = 'pr-5')",
    );
    assert_eq!(include_pr(&store, &who, 3), Ok(first));
    let production = store
        .read(
            &who,
            &ployz_store::ServiceQuery {
                environment: at("production"),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(production.values["env"], json!({"X": "1", "Y": "2"}));
}

/// A reopened pull request's new preview is where its proposal now comes from, before
/// any Include from it rebinds the proposal.
#[test]
fn a_recreated_preview_is_listed_as_its_proposals_source() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_on(ConfigStore::open(&url, backend::key()).unwrap());
    plan(&store, &who, on());
    pull(&store, &who, facts(true, "2026-09-29T10:00:00Z"));
    set(&store, &who, "pr-5", "web.env.X", json!("1"));
    include_pr(&store, &who, 1).unwrap();
    pull(&store, &who, facts(false, "2026-09-29T11:00:00Z"));
    let source = || {
        let diff = store
            .read(
                &who,
                &ployz_store::DiffQuery {
                    environment: at("production"),
                },
            )
            .unwrap();
        let [included] = &diff.included[..] else {
            panic!("one proposal: {:?}", diff.included)
        };
        let ployz_store::ProposalSource::PullRequest { environment, .. } = &included.source else {
            panic!("a pull request's proposal")
        };
        (environment.clone(), included.newer)
    };
    assert_eq!(source(), (None, false));

    pull(&store, &who, facts(true, "2026-09-29T12:00:00Z"));
    let preview = store
        .read(
            &who,
            &ployz_store::DiffQuery {
                environment: at("pr-5"),
            },
        )
        .unwrap()
        .environment
        .id;
    assert_eq!(source(), (Some(preview), true));
}

/// Reopened while its deployed preview is still closing, the new preview is listed.
#[test]
fn a_preview_reopened_while_the_old_one_closes_is_listed() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_on(ConfigStore::open(&url, backend::key()).unwrap());
    plan(&store, &who, on());
    let opened = pull(&store, &who, facts(true, "2026-09-29T10:00:00Z"));
    run(&store, &opened.admitted[0].deployment.id, &["web", "api"]);
    set(&store, &who, "pr-5", "web.env.X", json!("1"));
    include_pr(&store, &who, 1).unwrap();
    let source = || {
        let diff = store
            .read(
                &who,
                &ployz_store::DiffQuery {
                    environment: at("production"),
                },
            )
            .unwrap();
        let [included] = &diff.included[..] else {
            panic!("one proposal: {:?}", diff.included)
        };
        let ployz_store::ProposalSource::PullRequest { environment, .. } = &included.source else {
            panic!("a pull request's proposal")
        };
        (environment.clone(), included.newer)
    };
    let (old, _) = source();
    let closing = pull(&store, &who, facts(false, "2026-09-29T11:00:00Z"));
    assert_eq!(closing.closing.len(), 1, "{closing:?}");

    let reopened = pull(&store, &who, facts(true, "2026-09-29T12:00:00Z"));
    assert_eq!(reopened.admitted.len(), 1, "{reopened:?}");
    let (now, newer) = source();
    assert!(now.is_some() && now != old, "{now:?}");
    assert!(newer);
}
