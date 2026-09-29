#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Conditional Saves through the Store's interface only, on SQLite and on Postgres
//! (see `backend`): saving a PR Environment's changes for its merge, withdrawing,
//! the check, freezing at the merge, landing with the push that carries the merge
//! commit (never one without it), and taking a sealed hint after PR teardown.

use ployz_core::RpcErrorCode;
use ployz_core::config::ServiceGitAccess;
use ployz_store::{
    Actor, AuthorizedRepository, Automated, BranchHead, Change, CheckSuite, Command, ConfigStore,
    CreateGitService, CreateProject, DiffQuery, Edit, EnvironmentId, EnvironmentName,
    EnvironmentRef, Landed, Move, MovePick, MoveQuery, OrganizationId, ProjectId, ProjectName,
    Publish, PullRequest, PullRequestQuery, RunnerId, SaveState, ServiceId, SetPrPlan, SettingPath,
    SystemEvent, Trusted, When, Written,
};
use serde_json::{Value, json};

mod backend;

const MERGE: &str = "3333333333333333333333333333333333333333";

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn commit(n: u8) -> String {
    format!("{n:x}").repeat(40)
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

/// Project `shop`: `production` runs Git Services `web` and `api` from `acme/web`'s
/// `main`, published; pull requests get PR Environments of it, and PR #5 is open.
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
                    name: ployz_core::ServiceName::parse(name).unwrap(),
                    repository: "acme/web".into(),
                    branch: None,
                },
                &evidence,
            )
            .unwrap();
    }
    publish(&store, &who, "production");
    store
        .set_pr_plan(
            &who,
            &SetPrPlan {
                project: None,
                repository: "acme/web".into(),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy: None,
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    let opened = observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(true, None, None, 1)),
    );
    assert_eq!(opened.admitted.len(), 1);
    (store, who)
}

fn publish(store: &ConfigStore, who: &Actor, environment: &str) {
    store
        .write(
            who,
            &Command::Publish(Publish {
                environment: at(environment),
                version: None,
            }),
        )
        .unwrap();
}

fn set(store: &ConfigStore, who: &Actor, environment: &str, changes: &[(&str, Value)]) {
    store
        .edit(
            who,
            &Edit {
                environment: at(environment),
                expect: None,
                changes: changes
                    .iter()
                    .map(|(path, value)| Change::Set {
                        path: SettingPath::parse(path).unwrap(),
                        value: value.clone(),
                    })
                    .collect(),
            },
        )
        .unwrap();
}

fn facts(open: bool, merge: Option<&str>, reached: Option<String>, second: u8) -> PullRequest {
    PullRequest {
        repository_id: 11,
        number: 5,
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: "search".into(),
        head: commit(1),
        target_branch: "main".into(),
        commits: 1,
        open,
        merge_commit: merge.map(Into::into),
        merge_reached: reached,
        updated: format!("2026-09-29T10:00:{second:02}Z"),
    }
}

fn observe(store: &ConfigStore, who: &Actor, event: SystemEvent) -> Automated {
    let Written::Automated(automated) = store.system(&who.organization, &event).unwrap() else {
        panic!("a system event writes Automated")
    };
    automated
}

fn push(store: &ConfigStore, who: &Actor, head: u8, merged: &[&str]) -> Automated {
    let base = store.branch_head(&who.organization, 11, "main").unwrap();
    observe(
        store,
        who,
        SystemEvent::BranchHead(BranchHead {
            repository_id: 11,
            branch: "main".into(),
            base,
            head: Some(commit(head)),
            changed: None,
            merged: merged.iter().map(|merge| (*merge).to_owned()).collect(),
        }),
    )
}

/// Save `rows` of `pr-5` for its merge; `[]` withdraws.
fn save(rows: &[&str], version: Option<String>) -> Move {
    Move {
        from: Some(at("pr-5")),
        picks: Some(
            rows.iter()
                .map(|row| MovePick {
                    row: (*row).to_owned(),
                    choice: Some(serde_json::from_value(json!("from")).unwrap()),
                    value: None,
                })
                .collect(),
        ),
        version,
        ..Move::default()
    }
}

fn env(store: &ConfigStore, who: &Actor, environment: &str) -> Value {
    store
        .service(
            who,
            &ployz_store::ServiceQuery {
                environment: at(environment),
                service: ployz_core::ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap()
        .values
        .get("env")
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn check(store: &ConfigStore, who: &Actor) -> (bool, String) {
    let view = store
        .pull_request(
            who,
            &PullRequestQuery {
                repository_id: 11,
                number: 5,
            },
        )
        .unwrap();
    (view.passing, view.reason)
}

/// What the Deployment resolves `web`'s `key` to at claim.
fn resolved(store: &ConfigStore, deployment: &ployz_store::DeploymentId, key: &str) -> Value {
    let claimed = store
        .claim(deployment, &RunnerId::parse("runner").unwrap())
        .unwrap();
    claimed.input["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|snapshot| snapshot["serviceId"] == uuid(3))
        .unwrap()["resolvedEnv"][key]
        .clone()
}

#[test]
fn a_conditional_save_goes_live_with_the_push_that_carries_its_merge() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[
            ("web.env.MODE", json!("fast")),
            ("web.env.TOKEN", json!({ "secret": "pr-secret" })),
        ],
    );
    // From a PR Environment a Save is for the merge, into its one Destination.
    let query = MoveQuery {
        from: Some(at("pr-5")),
        ..MoveQuery::default()
    };
    let review = store.move_view(&who, &query).unwrap();
    assert_eq!(review.into.name.as_str(), "production");
    let rows: Vec<&str> = review.rows.iter().map(|row| row.row.as_str()).collect();
    assert_eq!(rows, ["web.env.MODE", "web.env.TOKEN"]);
    let now = Move {
        when: Some(When::Now),
        ..save(&["web.env"], None)
    };
    assert_eq!(
        store.move_changes(&who, &now).unwrap_err().code,
        RpcErrorCode::InvalidArgument
    );
    assert_eq!(
        check(&store, &who),
        (false, "2 changes to save in Ployz".into())
    );

    let saved = store
        .move_changes(&who, &save(&["web.env"], Some(review.version)))
        .unwrap();
    let conditional = saved.conditional_save.unwrap();
    assert_eq!(conditional.state, SaveState::Standing);
    assert_eq!(conditional.rows, ["web.env.MODE", "web.env.TOKEN"]);
    assert!(saved.staged.is_empty());
    assert_eq!(saved.checks.len(), 1);
    // Nothing lands before the merge.
    assert!(env(&store, &who, "production").get("MODE").is_none());
    assert_eq!(
        check(&store, &who),
        (true, "2 changes go live with this PR".into())
    );

    // A settings change in the PR Environment withdraws it; saving again restores it.
    set(&store, &who, "pr-5", &[("web.env.MODE", json!("slow"))]);
    assert_eq!(
        check(&store, &who),
        (false, "Changed since saved · save again".into())
    );
    let withdrawn = store.move_changes(&who, &save(&[], None)).unwrap();
    assert!(withdrawn.conditional_save.is_none());
    assert_eq!(
        check(&store, &who),
        (false, "2 changes to save in Ployz".into())
    );
    store.move_changes(&who, &save(&["web.env"], None)).unwrap();
    set(
        &store,
        &who,
        "production",
        &[("web.waitForCi", json!(true))],
    );

    // The merge push arrived first: Cloud reports the merge, and the save freezes.
    let pending = store.pending_saves(&who.organization, 11, "main").unwrap();
    assert_eq!((pending.standing, pending.merged.len()), (vec![5], 0));
    let closed = observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(false, Some(MERGE), None, 2)),
    );
    assert_eq!(closed.removed.len(), 1, "{closed:?}");
    let pending = store.pending_saves(&who.organization, 11, "main").unwrap();
    assert_eq!(
        (pending.standing.len(), pending.merged),
        (0, vec![MERGE.to_owned()])
    );
    assert!(env(&store, &who, "production").get("MODE").is_none());

    // A push without the merge commit (a force-push) carries nothing.
    let forced = push(&store, &who, 4, &[]);
    assert_eq!(forced.waiting.len(), 1);
    assert!(env(&store, &who, "production").get("MODE").is_none());
    // The one that has it waits for CI with it, and lands it before it deploys.
    let pushed = push(&store, &who, 5, &[MERGE]);
    assert_eq!(pushed.waiting.len(), 1);
    assert!(env(&store, &who, "production").get("MODE").is_none());
    let passed = observe(
        &store,
        &who,
        SystemEvent::CheckSuite(CheckSuite {
            repository_id: 11,
            suite: 1,
            head: commit(5),
            status: "completed".into(),
            conclusion: Some("success".into()),
            updated: "2026-09-29T10:01:00Z".into(),
        }),
    );
    assert_eq!(passed.admitted.len(), 1, "{passed:?}");
    assert_eq!(env(&store, &who, "production")["MODE"], json!("slow"));
    assert_eq!(
        resolved(&store, &passed.admitted[0].deployment.id, "TOKEN"),
        json!("pr-secret")
    );
    let pending = store.pending_saves(&who.organization, 11, "main").unwrap();
    assert!(pending.merged.is_empty());
}

#[test]
fn a_hint_beside_the_destinations_own_edit_is_taken_after_pr_teardown() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[
            ("web.env.MODE", json!("pr")),
            ("web.env.TOKEN", json!({ "secret": "pr-secret" })),
        ],
    );
    store.move_changes(&who, &save(&["web.env"], None)).unwrap();
    // Production changes MODE live, then stages its own edit on top.
    set(
        &store,
        &who,
        "production",
        &[("web.env.MODE", json!("live"))],
    );
    publish(&store, &who, "production");
    set(
        &store,
        &who,
        "production",
        &[("web.env.MODE", json!("staged"))],
    );
    // The merge commit reached main's head, which deployed without waiting: it lands at the merge.
    assert_eq!(push(&store, &who, 4, &[]).admitted.len(), 1);
    let closed = observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(false, Some(MERGE), Some(commit(4)), 2)),
    );
    assert_eq!(closed.removed.len(), 1, "{closed:?}");
    let production = env(&store, &who, "production");
    assert_eq!(
        (&production["MODE"], &production["TOKEN"]),
        (&json!("staged"), &json!({ "secret": true }))
    );

    let diff = |store: &ConfigStore| {
        store
            .diff(
                &who,
                &DiffQuery {
                    environment: at("production"),
                },
            )
            .unwrap()
            .hints
    };
    let hints = diff(&store);
    assert_eq!(hints.len(), 1);
    let hint = &hints[0];
    assert_eq!(
        (
            hint.row.as_str(),
            hint.landed,
            &hint.value,
            hint.pull_request
        ),
        ("web.env.MODE", Landed::Hint, &json!("pr"), 5)
    );
    let take = |row: Option<&str>| Move {
        take: Some(hint.save.clone()),
        picks: row.map(|row| {
            vec![MovePick {
                row: row.into(),
                choice: None,
                value: None,
            }]
        }),
        ..Move::default()
    };
    assert_eq!(
        store
            .move_changes(&who, &take(Some("web.env.NOPE")))
            .unwrap_err()
            .code,
        RpcErrorCode::NotFound
    );
    // The PR Environment is gone; its value still moves.
    let taken = store
        .move_changes(&who, &take(Some("web.env.MODE")))
        .unwrap();
    assert_eq!(taken.staged, ["web"]);
    assert_eq!(taken.from.name.as_str(), "pr-5");
    assert_eq!(taken.conditional_save.unwrap().state, SaveState::Landed);
    assert_eq!(env(&store, &who, "production")["MODE"], json!("pr"));
    assert_eq!(diff(&store)[0].landed, Landed::Staged);
    // Nothing is left to take.
    assert_eq!(
        store.move_changes(&who, &take(None)).unwrap_err().code,
        RpcErrorCode::Conflict
    );
    publish(&store, &who, "production");
    assert!(diff(&store).is_empty());
    let pushed = push(&store, &who, 5, &[]);
    assert_eq!(
        resolved(&store, &pushed.admitted[0].deployment.id, "TOKEN"),
        json!("pr-secret")
    );
}
