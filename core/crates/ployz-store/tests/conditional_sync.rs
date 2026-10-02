#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Conditional Syncs through the Store's interface only, on SQLite and on Postgres
//! (see `backend`): syncing a PR Environment's changes for its merge, withdrawing,
//! the check, freezing at the merge, landing with the push that carries the merge
//! commit (never one without it), taking a hint after PR teardown, a secret that
//! waits for the Destination's value, syncing now where the merge doesn't reach, and
//! a Follow that leaves it standing.

use ployz_core::RpcErrorCode;
use ployz_core::config::ServiceGitAccess;
use ployz_store::{
    Actor, AuthorizedRepository, Automated, Batch, BatchCommand, BranchHead, BranchQuery, Change,
    CheckSuite, Command, ConditionalSyncState, ConfigStore, CreateBranch, CreateGitService,
    CreateProject, DiffQuery, Edit, EnvironmentId, EnvironmentName, EnvironmentRef, HoldSecret,
    Landed, OrganizationId, ProjectId, ProjectName, Publish, PullRequest, PullRequestQuery,
    RunnerId, ServiceLineageId, SetPrPlan, SettingPath, SyncChanges, SyncQuery, SyncView,
    SystemEvent, Take, Trusted, When, Written,
};
use serde_json::{Value, json};

mod backend;

/// Items as their text, to compare with literals.
fn texts<T: ToString>(items: &[T]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

const MERGE: &str = "3333333333333333333333333333333333333333";

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn commit(n: u8) -> ployz_store::CommitSha {
    backend::sha(&format!("{n:x}").repeat(40))
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
    shop_in(backend::open(), "production")
}

/// [`shop`] in `store`, its PR Environments Branches of `start_from`: a kept Branch
/// of production with its own `web`, unless it is production.
fn shop_in(store: ConfigStore, start_from: &str) -> (ConfigStore, Actor) {
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
                    name: ployz_core::ServiceName::parse(name).unwrap(),
                    repository: backend::repo_name("acme/web"),
                    branch: None,
                },
                &evidence,
            )
            .unwrap();
    }
    publish(&store, &who, "production");
    if start_from != "production" {
        store
            .write(
                &who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(20)).unwrap(),
                    from: at("production"),
                    name: EnvironmentName::parse(start_from).unwrap(),
                    copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                    live: Vec::new(),
                    setup: Vec::new(),
                    keep: true,
                    fix: None,
                },
            )
            .unwrap();
    }
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse(start_from).unwrap()),
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
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
}

fn set(store: &ConfigStore, who: &Actor, environment: &str, changes: &[(&str, Value)]) {
    store
        .write(
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

fn facts(
    open: bool,
    merge: Option<&str>,
    reached: Option<ployz_store::CommitSha>,
    second: u8,
) -> PullRequest {
    PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: backend::git_branch("search"),
        head: commit(1),
        target_branch: backend::git_branch("main"),
        commits: 1,
        open,
        merge_commit: merge.map(backend::sha),
        merge_reached: reached,
        updated: ployz_store::GithubTimestamp::parse(format!("2026-09-29T10:00:{second:02}Z"))
            .unwrap(),
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

fn push(store: &ConfigStore, who: &Actor, head: u8, merged: &[&str]) -> Automated {
    let base = store
        .branch_head(
            &who.organization,
            backend::repo_id(11),
            &backend::git_branch("main"),
        )
        .unwrap();
    observe(
        store,
        who,
        SystemEvent::BranchHead(BranchHead {
            repository_id: backend::repo_id(11),
            branch: backend::git_branch("main"),
            base,
            head: Some(commit(head)),
            changed: None,
            merged: merged.iter().map(|merge| backend::sha(merge)).collect(),
        }),
    )
}

/// What a Sync from `pr-5` into `into` (omitted: its only Destination) carries.
fn offered(store: &ConfigStore, who: &Actor, into: Option<&str>) -> SyncView {
    store
        .read(
            who,
            &SyncQuery {
                from: at("pr-5"),
                into: into.map(at),
            },
        )
        .unwrap()
}

/// Sync `review`'s rows named by or under `labels` (omitted: those ticked) as reviewed.
fn sync(review: &SyncView, labels: Option<&[&str]>) -> SyncChanges {
    let named = |label: &str| {
        labels.is_none_or(|labels| {
            labels.iter().any(|asked| {
                label == *asked
                    || label
                        .strip_prefix(asked)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
        })
    };
    SyncChanges {
        from: at("pr-5"),
        into: Some(at(review.into.name.as_str())),
        picks: Some(
            review
                .rows
                .iter()
                .filter(|row| named(&row.label))
                .map(|row| row.key.clone())
                .collect(),
        ),
        version: Some(review.version.clone()),
        ..SyncChanges::default()
    }
}

/// Withdraw `pr-5`'s Conditional Sync, as the Sync button's Undo does.
fn withdraw() -> SyncChanges {
    SyncChanges {
        from: at("pr-5"),
        when: Some(When::Withdraw),
        ..SyncChanges::default()
    }
}

/// How many changes the pull request's page counts for `pr-5` into production.
fn destination_changes(store: &ConfigStore, who: &Actor) -> usize {
    let view = store
        .read(
            who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap();
    view.environments[0].destinations[0].changes
}

/// The Sync button's count on `pr-5`, into its Parent.
fn to_parent(store: &ConfigStore, who: &Actor) -> usize {
    store
        .read(
            who,
            &BranchQuery {
                environment: at("pr-5"),
            },
        )
        .unwrap()
        .to_parent
}

fn env(store: &ConfigStore, who: &Actor, environment: &str) -> Value {
    store
        .read(
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

/// Hold production's value of `path` for PR #5's merge.
fn hold(path: &str, value: &str) -> HoldSecret {
    HoldSecret {
        environment: at("production"),
        pull_request: backend::pr_number(5),
        path: SettingPath::parse(path).unwrap(),
        value: value.into(),
        repository: None,
    }
}

fn check(store: &ConfigStore, who: &Actor) -> (bool, String) {
    let view = store
        .read(
            who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
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
fn a_conditional_sync_goes_live_with_the_push_that_carries_its_merge() {
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
    // From a PR Environment a Sync is for the merge, into its one Destination.
    let review = offered(&store, &who, None);
    assert_eq!(
        (review.into.name.as_str(), review.at_merge),
        ("production", Some(backend::pr_number(5)))
    );
    let rows: Vec<(&str, bool)> = review
        .rows
        .iter()
        .map(|row| (row.label.as_str(), row.ticked))
        .collect();
    assert_eq!(rows, [("web.env.MODE", true), ("web.env.TOKEN", true)]);
    // Its secret goes by name only: production has no value of it yet.
    let secrets: Vec<(bool, bool)> = review
        .rows
        .iter()
        .map(|row| (row.secret, row.value_set))
        .collect();
    assert_eq!(secrets, [(false, false), (true, false)]);
    // Nothing to hold a value for before it syncs.
    let refused = store
        .write(&who, &hold("web.env.TOKEN", "prod-secret"))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        refused.details["next"],
        json!("ployz env sync --to production --project shop --env pr-5")
    );
    // The pull request's page and the Sync button count what it ticks.
    assert_eq!(destination_changes(&store, &who), 2);
    assert_eq!(to_parent(&store, &who), 2);
    let closing = SyncChanges {
        close_after: true,
        ..sync(&review, None)
    };
    assert_eq!(
        store.write(&who, &closing).unwrap_err().code,
        RpcErrorCode::InvalidArgument
    );
    assert_eq!(
        check(&store, &who),
        (false, "2 changes to sync in Ployz".into())
    );

    let committed = store
        .commit(
            &who,
            &Command::Sync(sync(&review, Some(&["web.env"]))),
            &Trusted::default(),
        )
        .unwrap();
    // Cloud publishes the PR's check again.
    assert_eq!(committed.checks.len(), 1);
    let Written::Synced(synced) = committed.written else {
        panic!("a Sync writes Synced")
    };
    let conditional = synced.conditional_sync.unwrap();
    assert_eq!(conditional.state, ConditionalSyncState::Standing);
    assert_eq!(conditional.rows, ["web.env.MODE", "web.env.TOKEN"]);
    assert!(synced.staged.is_empty());
    // Nothing lands before the merge.
    assert!(env(&store, &who, "production").get("MODE").is_none());
    // The check waits for production's value of the secret, held ahead of the merge.
    assert_eq!(
        check(&store, &who),
        (false, "Waiting for production's value of TOKEN".into())
    );
    assert_eq!(
        store
            .write(&who, &hold("web.env.NOPE", "prod-secret"))
            .unwrap_err()
            .code,
        RpcErrorCode::NotFound
    );
    let committed = store
        .commit(
            &who,
            &Command::HoldSecret(hold("web.env.TOKEN", "prod-secret")),
            &Trusted::default(),
        )
        .unwrap();
    assert_eq!(committed.checks.len(), 1);
    assert_eq!(
        check(&store, &who),
        (true, "2 changes go live with this PR".into())
    );
    let token = |review: &SyncView| {
        let row = review
            .rows
            .iter()
            .find(|row| row.label == "web.env.TOKEN")
            .unwrap();
        (row.secret, row.value_set)
    };
    assert_eq!(token(&offered(&store, &who, None)), (true, true));
    // Held values are never shown back.
    assert_eq!(env(&store, &who, "production").get("TOKEN"), None);

    // A settings change in the PR Environment withdraws it; syncing again restores it.
    set(&store, &who, "pr-5", &[("web.env.MODE", json!("slow"))]);
    assert_eq!(
        check(&store, &who),
        (false, "Changed since synced · sync again".into())
    );
    let withdrawn = store.write(&who, &withdraw()).unwrap();
    assert!(withdrawn.conditional_sync.is_none());
    assert_eq!(
        check(&store, &who),
        (false, "2 changes to sync in Ployz".into())
    );
    let review = offered(&store, &who, None);
    // The held value survives a withdraw and sync again.
    assert_eq!(token(&review), (true, true));
    store.write(&who, &sync(&review, None)).unwrap();
    assert_eq!(
        check(&store, &who),
        (true, "2 changes go live with this PR".into())
    );
    set(
        &store,
        &who,
        "production",
        &[("web.waitForCi", json!(true))],
    );

    // The merge push arrived first: Cloud reports the merge, and the Conditional Sync freezes.
    let pending = store
        .pending_syncs(
            &who.organization,
            backend::repo_id(11),
            &backend::git_branch("main"),
        )
        .unwrap();
    assert_eq!(
        (pending.standing, pending.merged.len()),
        (vec![backend::pr_number(5)], 0)
    );
    let closed = observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(false, Some(MERGE), None, 2)),
    );
    assert_eq!(closed.removed.len(), 1, "{closed:?}");
    let pending = store
        .pending_syncs(
            &who.organization,
            backend::repo_id(11),
            &backend::git_branch("main"),
        )
        .unwrap();
    assert_eq!(
        (pending.standing.len(), pending.merged),
        (0, vec![backend::sha(MERGE)])
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
            repository_id: backend::repo_id(11),
            suite: 1,
            head: commit(5),
            status: ployz_store::CheckStatus::Completed,
            conclusion: Some(ployz_store::CheckConclusion::Success),
            updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:01:00Z").unwrap(),
        }),
    );
    assert_eq!(passed.admitted.len(), 1, "{passed:?}");
    assert_eq!(env(&store, &who, "production")["MODE"], json!("slow"));
    // It lands with production's held value; the pull request's never travels.
    assert_eq!(
        resolved(&store, &passed.admitted[0].deployment.id, "TOKEN"),
        json!("prod-secret")
    );
    let pending = store
        .pending_syncs(
            &who.organization,
            backend::repo_id(11),
            &backend::git_branch("main"),
        )
        .unwrap();
    assert!(pending.merged.is_empty());
}

#[test]
fn a_hint_beside_the_destinations_own_edit_is_taken_after_pr_teardown() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.MODE", json!("pr"))]);
    let review = offered(&store, &who, None);
    store
        .write(&who, &sync(&review, Some(&["web.env"])))
        .unwrap();
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
    assert_eq!(env(&store, &who, "production")["MODE"], json!("staged"));

    let diff = |store: &ConfigStore| {
        store
            .read(
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
        (
            "web.env.MODE",
            Landed::Hint,
            &json!("pr"),
            backend::pr_number(5)
        )
    );
    let take = |row: Option<&str>| Take {
        from: ployz_store::HintSource::ConditionalSync(hint.conditional_sync.clone()),
        into: None,
        rows: row.map(|row| vec![row.to_owned()]),
        version: store
            .read(
                &who,
                &DiffQuery {
                    environment: at("production"),
                },
            )
            .unwrap()
            .version,
    };
    assert_eq!(
        store
            .write(&who, &take(Some("web.env.NOPE")))
            .unwrap_err()
            .code,
        RpcErrorCode::NotFound
    );
    // The PR Environment is gone; its value still moves.
    let taken = store.write(&who, &take(Some("web.env.MODE"))).unwrap();
    assert_eq!(texts(&taken.staged), ["web"]);
    assert_eq!(taken.from.name.as_str(), "pr-5");
    assert_eq!(
        taken.conditional_sync.unwrap().state,
        ConditionalSyncState::Landed
    );
    assert_eq!(env(&store, &who, "production")["MODE"], json!("pr"));
    assert_eq!(diff(&store)[0].landed, Landed::Staged);
    // Nothing is left to take.
    assert_eq!(
        store.write(&who, &take(None)).unwrap_err().code,
        RpcErrorCode::Conflict
    );
    publish(&store, &who, "production");
    assert!(diff(&store).is_empty());
    let pushed = push(&store, &who, 5, &[]);
    assert_eq!(
        resolved(&store, &pushed.admitted[0].deployment.id, "MODE"),
        json!("pr")
    );
}

/// PR #5 syncs TOKEN and production's value is held for the merge, but production
/// sets its own first: the check stops waiting, and the merge leaves production's
/// own value, with no hint.
#[test]
fn a_secret_the_destination_sets_itself_leaves_the_held_value_unused() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "pr-secret" }))],
    );
    let review = offered(&store, &who, None);
    store
        .write(&who, &sync(&review, Some(&["web.env.TOKEN"])))
        .unwrap();
    assert_eq!(
        check(&store, &who),
        (false, "Waiting for production's value of TOKEN".into())
    );
    store.write(&who, &hold("web.env.TOKEN", "held")).unwrap();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": "own" }))],
    );
    assert_eq!(
        check(&store, &who),
        (true, "1 change goes live with this PR".into())
    );
    publish(&store, &who, "production");
    assert_eq!(push(&store, &who, 4, &[]).admitted.len(), 1);
    observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(false, Some(MERGE), Some(commit(4)), 2)),
    );
    let hints = store
        .read(
            &who,
            &DiffQuery {
                environment: at("production"),
            },
        )
        .unwrap()
        .hints;
    assert!(hints.is_empty(), "{hints:?}");
    set(&store, &who, "production", &[("web.env.MODE", json!("x"))]);
    publish(&store, &who, "production");
    let pushed = push(&store, &who, 5, &[]);
    assert_eq!(
        resolved(&store, &pushed.admitted[0].deployment.id, "TOKEN"),
        json!("own")
    );
}

/// A value held for a merge that never comes drops with the pull request: reopened
/// and synced again, it waits for both secrets again.
#[test]
fn a_held_value_drops_when_the_pull_request_closes_unmerged() {
    let (store, who) = shop();
    let secrets = [
        ("web.env.TOKEN", json!({ "secret": "pr-token" })),
        ("web.env.KEY", json!({ "secret": "pr-key" })),
    ];
    set(&store, &who, "pr-5", &secrets);
    store
        .write(&who, &sync(&offered(&store, &who, None), None))
        .unwrap();
    assert_eq!(
        check(&store, &who),
        (false, "Waiting for production's value of 2 secrets".into())
    );
    store.write(&who, &hold("web.env.TOKEN", "held")).unwrap();
    assert_eq!(
        check(&store, &who),
        (false, "Waiting for production's value of KEY".into())
    );
    let closed = observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(false, None, None, 2)),
    );
    assert_eq!(closed.removed.len(), 1, "{closed:?}");
    assert!(env(&store, &who, "production").get("TOKEN").is_none());

    observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(true, None, None, 3)),
    );
    set(&store, &who, "pr-5", &secrets);
    store
        .write(&who, &sync(&offered(&store, &who, None), None))
        .unwrap();
    assert_eq!(
        check(&store, &who),
        (false, "Waiting for production's value of 2 secrets".into())
    );
}

#[test]
fn a_pr_environment_syncs_into_an_environment_its_merge_doesnt_reach_now() {
    let (store, who) = shop();
    // staging is made from production, which deploys main: it isn't a Destination.
    store
        .write(
            &who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(20)).unwrap(),
                from: at("production"),
                name: EnvironmentName::parse("staging").unwrap(),
                copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                live: Vec::new(),
                setup: Vec::new(),
                keep: true,
                fix: None,
            },
        )
        .unwrap();
    set(&store, &who, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let review = offered(&store, &who, Some("staging"));
    assert_eq!(review.at_merge, None);
    let synced = store.write(&who, &sync(&review, None)).unwrap();
    assert!(synced.conditional_sync.is_none());
    assert_eq!(texts(&synced.staged), ["web"]);
    assert_eq!(env(&store, &who, "staging")["MODE"], json!("fast"));
    // It is still waiting to go to production at the merge.
    assert_eq!(
        check(&store, &who),
        (false, "1 change to sync in Ployz".into())
    );
}

#[test]
fn a_follow_into_the_pr_environment_leaves_its_conditional_sync_standing() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.MODE", json!("fast"))]);
    store
        .write(&who, &sync(&offered(&store, &who, None), None))
        .unwrap();
    // production deploys a change of its own, which follows into pr-5.
    set(
        &store,
        &who,
        "production",
        &[("web.env.SHARED", json!("1"))],
    );
    publish(&store, &who, "production");
    let pushed = push(&store, &who, 4, &[]);
    run(&store, &pushed.admitted[0].deployment.id, &["web", "api"]);
    assert_eq!(env(&store, &who, "pr-5")["SHARED"], json!("1"));
    assert_eq!(
        check(&store, &who),
        (true, "1 change goes live with this PR".into())
    );
    // The author's own edit still withdraws it.
    set(&store, &who, "pr-5", &[("web.env.MODE", json!("slow"))]);
    assert_eq!(
        check(&store, &who),
        (false, "Changed since synced · sync again".into())
    );
}

#[test]
fn a_pr_environment_syncs_into_its_parent_now_and_into_its_destination_at_the_merge() {
    // pr-5 is a Branch of staging, which the merge doesn't reach.
    let (store, who) = shop_in(backend::open(), "staging");
    set(&store, &who, "pr-5", &[("web.env.MODE", json!("fast"))]);
    assert_eq!(to_parent(&store, &who), 1);
    let into_parent = offered(&store, &who, Some("staging"));
    assert_eq!(into_parent.at_merge, None);
    let synced = store.write(&who, &sync(&into_parent, None)).unwrap();
    assert!(synced.conditional_sync.is_none());
    assert_eq!(env(&store, &who, "staging")["MODE"], json!("fast"));
    // Unnamed, the Sync is into production, at the merge.
    let review = offered(&store, &who, None);
    assert_eq!(
        (review.into.name.as_str(), review.at_merge),
        ("production", Some(backend::pr_number(5)))
    );
}

#[test]
fn now_syncs_a_pr_environment_into_its_destination_at_once() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let now = SyncChanges {
        when: Some(When::Now),
        ..sync(&offered(&store, &who, None), None)
    };
    let synced = store.write(&who, &now).unwrap();
    assert!(synced.conditional_sync.is_none());
    assert_eq!(texts(&synced.staged), ["web"]);
    assert_eq!(env(&store, &who, "production")["MODE"], json!("fast"));
}

/// Every `saved` document of a Conditional Sync in the Store at `url`.
fn saved_documents(url: &str) -> Vec<String> {
    let sql = "SELECT saved FROM config_conditional_sync";
    match url.strip_prefix("sqlite:") {
        Some(path) => {
            let connection = rusqlite::Connection::open(path).unwrap();
            let mut statement = connection.prepare(sql).unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        }
        None => postgres::Client::connect(url, postgres::NoTls)
            .unwrap()
            .query(sql, &[])
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect(),
    }
}

#[test]
fn a_conditional_sync_keeps_no_secret_value() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let opened = ConfigStore::open(&url, backend::key()).unwrap();
    let (store, who) = shop_in(opened, "production");
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "pr-secret" }))],
    );
    store
        .write(&who, &sync(&offered(&store, &who, None), None))
        .unwrap();
    let saved = saved_documents(&url);
    assert_eq!(saved.len(), 1);
    assert!(!saved[0].contains("ciphertext"), "{}", saved[0]);
}

#[test]
fn a_value_held_for_a_pull_request_number_two_repositories_share_names_the_repository() {
    let (store, who) = shop();
    // acme/docs deploys into production too, and its PR #5 copies web.
    let evidence = Trusted {
        repositories: vec![AuthorizedRepository {
            repository: backend::repo_name("acme/docs"),
            repository_id: backend::repo_id(12),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: backend::git_branch("main"),
            branches: Vec::new(),
        }],
        ..Trusted::default()
    };
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ServiceName::parse("docs").unwrap(),
                repository: backend::repo_name("acme/docs"),
                branch: None,
            },
            &evidence,
        )
        .unwrap();
    publish(&store, &who, "production");
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/docs"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy: Some(vec![ployz_store::NodeName::parse("web").unwrap()]),
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    let docs = PullRequest {
        repository_id: backend::repo_id(12),
        ..facts(true, None, None, 1)
    };
    let opened = observe(&store, &who, SystemEvent::PullRequest(docs));
    assert_eq!(opened.admitted.len(), 1);
    let token = [("web.env.TOKEN", json!({ "secret": "pr-secret" }))];
    for pr in ["pr-5", "pr-5-2"] {
        set(&store, &who, pr, &token);
        let review = store
            .read(
                &who,
                &SyncQuery {
                    from: at(pr),
                    into: None,
                },
            )
            .unwrap();
        let synced = SyncChanges {
            from: at(pr),
            ..sync(&review, None)
        };
        store.write(&who, &synced).unwrap();
    }

    let refused = store
        .write(&who, &hold("web.env.TOKEN", "prod-secret"))
        .unwrap_err();
    assert_eq!(
        (refused.code, refused.message.as_str()),
        (
            RpcErrorCode::InvalidArgument,
            "#5 of more than one repository brings web.env.TOKEN: name the repository"
        )
    );
    let named = HoldSecret {
        repository: Some(backend::repo_id(11)),
        ..hold("web.env.TOKEN", "prod-secret")
    };
    store.write(&who, &named).unwrap();
    assert_eq!(
        check(&store, &who),
        (true, "1 change goes live with this PR".into())
    );
}

#[test]
fn a_value_for_a_secret_lands_with_its_sync_or_neither_does() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "pr-secret" }))],
    );
    let batch = |held: &str| Batch {
        environment: at("production"),
        commands: vec![
            BatchCommand::Sync(sync(&offered(&store, &who, None), None)),
            BatchCommand::HoldSecret(hold(held, "prod-secret")),
        ],
    };
    // Nothing to hold the value for: the Sync doesn't stand either.
    store.write(&who, &batch("web.env.NOPE")).unwrap_err();
    assert_eq!(
        check(&store, &who),
        (false, "1 change to sync in Ployz".into())
    );
    store.write(&who, &batch("web.env.TOKEN")).unwrap();
    assert_eq!(
        check(&store, &who),
        (true, "1 change goes live with this PR".into())
    );

    // A Sync now lands with the value set beside it.
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.KEY", json!({ "secret": "pr-key" }))],
    );
    let review = offered(&store, &who, None);
    let now = Batch {
        environment: at("production"),
        commands: vec![
            BatchCommand::Sync(SyncChanges {
                when: Some(When::Now),
                ..sync(&review, Some(&["web.env.KEY"]))
            }),
            BatchCommand::Edit(Edit {
                environment: at("production"),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("web.env.KEY").unwrap(),
                    value: json!({ "secret": "prod-key" }),
                }],
            }),
        ],
    };
    store.write(&who, &now).unwrap();
    publish(&store, &who, "production");
    let pushed = push(&store, &who, 4, &[]);
    assert_eq!(
        resolved(&store, &pushed.admitted[0].deployment.id, "KEY"),
        json!("prod-key")
    );
}

/// Run Deployment `id` to `applied`, touching `services`.
fn run(store: &ConfigStore, id: &ployz_store::DeploymentId, services: &[&str]) {
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(id, &runner).unwrap();
    let operation = |index: usize| {
        json!({"type": "remove_container", "machine_id": "a".repeat(32),
               "container_id": format!("{index:x}").repeat(64)})
    };
    let preview: ployz_core::DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace,
        "operations": services.iter().enumerate().map(|(index, name)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": name,
            "operation": operation(index), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(id, &runner, ployz_store::RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: ployz_core::DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(json!({
            "type": "success",
            "completed": (0..services.len()).map(operation).collect::<Vec<_>>()
        }))
        .unwrap();
    store
        .record(
            id,
            &runner,
            ployz_store::RunEvidence::Executed {
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}
