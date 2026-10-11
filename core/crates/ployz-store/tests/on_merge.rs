#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! A PR Environment's On merge list through the Store's interface, on SQLite and on
//! Postgres (see `backend`): every change it made rides into its Destination, over
//! whatever production changed since, a removal too; Exclude keeps a row here, an
//! Edit gives a riding variable the value it lands with. The merge lands the list
//! in one Saved revision and deploys what it changed with the merge's push.

use ployz_core::config::ServiceGitAccess;
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AuthorizedRepository, Automated, BranchHead, Change, CheckSuite, ConfigStore,
    CreateGitService, CreateProject, DiffQuery, DiffView, Edit, EnvironmentId, EnvironmentName,
    EnvironmentQuery, EnvironmentRef, EnvironmentsQuery, HistoryAction, HistoryPreviewQuery,
    HistoryQuery, MergeRow, NeverSync, OnMerge, OnMergeQuery, OrganizationId, ProjectId,
    ProjectName, Publish, PullRequest, Ride, SavedRevision, ServiceLineageId, SetMergeValue,
    SetPrPlan, SettingPath, StageHistory, SyncChanges, SyncQuery, SystemEvent, Trusted, Written,
};
use serde_json::{Value, json};

mod backend;

const HEAD: &str = "1111111111111111111111111111111111111111";

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

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

/// Project `shop`: `production` runs Git Service `web` from `acme/web`'s `main`, with
/// `A=1`, `C=1` and `D=1` saved; pull request #5 opened `pr-5` from it.
fn shop() -> (ConfigStore, Actor) {
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
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(3)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                repository: backend::repo_name("acme/web"),
                branch: None,
            },
            &vouched(),
        )
        .unwrap();
    set(
        &store,
        &who,
        "production",
        &[
            ("web.env.A", json!("1")),
            ("web.env.C", json!("1")),
            ("web.env.D", json!("1")),
        ],
    );
    save(&store, &who, "production");
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy: None,
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    pull(&store, &who, "main", "2026-09-29T10:00:00Z");
    (store, who)
}

/// Pull request #5, open, onto `target`.
fn pull(store: &ConfigStore, who: &Actor, target: &str, updated: &str) {
    let event = SystemEvent::PullRequest(PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: backend::git_branch("search"),
        head: backend::sha(HEAD),
        target_branch: backend::git_branch(target),
        commits: 1,
        open: true,
        merge_commit: None,
        updated: ployz_store::GithubTimestamp::parse(updated).unwrap(),
    });
    store
        .system(&who.organization, &event, &Trusted::default())
        .unwrap();
}

fn set(store: &ConfigStore, who: &Actor, environment: &str, changes: &[(&str, Value)]) {
    edit(
        store,
        who,
        environment,
        changes
            .iter()
            .map(|(path, value)| Change::Set {
                path: SettingPath::parse(path).unwrap(),
                value: value.clone(),
            })
            .collect(),
    );
}

fn unset(store: &ConfigStore, who: &Actor, environment: &str, path: &str) {
    let path = SettingPath::parse(path).unwrap();
    edit(store, who, environment, vec![Change::Unset { path }]);
}

fn edit(store: &ConfigStore, who: &Actor, environment: &str, changes: Vec<Change>) {
    store
        .write_trusted(
            who,
            &Edit {
                environment: at(environment),
                expect: None,
                changes,
            },
            &vouched(),
        )
        .unwrap();
}

fn save(store: &ConfigStore, who: &Actor, environment: &str) {
    store
        .write(
            who,
            &ployz_store::Command::Publish(Publish {
                environment: at(environment),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
}

fn on_merge(store: &ConfigStore, who: &Actor) -> OnMerge {
    store
        .read(
            who,
            &OnMergeQuery {
                environment: at("pr-5"),
            },
        )
        .unwrap()
}

/// The rows landing in production, by label.
fn rows(store: &ConfigStore, who: &Actor) -> Vec<MergeRow> {
    let on_merge = on_merge(store, who);
    assert_eq!(on_merge.destinations.len(), 1, "{on_merge:?}");
    assert_eq!(on_merge.destinations[0].name.as_str(), "production");
    on_merge.destinations[0].rows.clone()
}

fn labels(store: &ConfigStore, who: &Actor) -> Vec<(String, Ride)> {
    rows(store, who)
        .into_iter()
        .map(|row| (row.at.to_string(), row.ride))
        .collect()
}

fn row(store: &ConfigStore, who: &Actor, label: &str) -> MergeRow {
    rows(store, who)
        .into_iter()
        .find(|row| row.at.to_string() == label)
        .unwrap_or_else(|| panic!("no {label}"))
}

fn exclude(store: &ConfigStore, who: &Actor, label: &str, off: bool) {
    store
        .write(
            who,
            &NeverSync {
                environment: at("pr-5"),
                rows: vec![label.into()],
                off,
            },
        )
        .unwrap();
}

fn merge_value(label: &str, value: Option<&str>) -> SetMergeValue {
    SetMergeValue {
        environment: at("pr-5"),
        into: None,
        row: label.into(),
        value: value.map(Into::into),
    }
}

#[test]
fn every_change_rides_over_what_production_changed_since_a_removal_too() {
    let (store, who) = shop();
    assert_eq!(labels(&store, &who), []);

    // Unsaved changes ride: a change, a new variable and a removal.
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("2")), ("web.env.B", json!("new"))],
    );
    unset(&store, &who, "pr-5", "web.env.C");
    // production changes A after pr-5 started: the merge lands over it, flagged.
    set(&store, &who, "production", &[("web.env.A", json!("3"))]);
    save(&store, &who, "production");

    let a = row(&store, &who, "web.env.A");
    assert_eq!(
        (a.production, a.lands, a.preview, a.ride, a.conflict),
        (json!("3"), json!("2"), json!("2"), Ride::Rides, true)
    );
    let b = row(&store, &who, "web.env.B");
    assert_eq!(
        (b.production, b.lands, b.ride, b.conflict),
        (Value::Null, json!("new"), Ride::Rides, false)
    );
    let c = row(&store, &who, "web.env.C");
    assert_eq!(
        (c.production, c.lands, c.preview, c.ride),
        (json!("1"), Value::Null, Value::Null, Ride::Rides)
    );
    assert_eq!(rows(&store, &who).len(), 3);
}

#[test]
fn an_excluded_row_stays_listed_but_never_rides_until_included_again() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    unset(&store, &who, "pr-5", "web.env.C");

    // A removed row is excluded by name too: it is in what pr-5 started from.
    exclude(&store, &who, "web.env.A", false);
    exclude(&store, &who, "web.env.C", false);
    assert_eq!(
        labels(&store, &who),
        [
            ("web.env.A".to_owned(), Ride::Excluded),
            ("web.env.C".to_owned(), Ride::Excluded)
        ]
    );
    // An Excluded row takes no Edit.
    let refused = store
        .write(&who, &merge_value("web.env.A", Some("9")))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{refused:?}");

    exclude(&store, &who, "web.env.C", true);
    assert_eq!(row(&store, &who, "web.env.C").ride, Ride::Rides);
}

#[test]
fn productions_own_never_sync_mark_keeps_its_value() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &NeverSync {
                environment: at("production"),
                rows: vec!["web.env.D".into()],
                off: false,
            },
        )
        .unwrap();
    set(&store, &who, "pr-5", &[("web.env.D", json!("2"))]);
    let d = row(&store, &who, "web.env.D");
    assert_eq!((d.ride, d.production), (Ride::ProductionKeeps, json!("1")));
}

#[test]
fn a_new_secret_lands_not_set_unless_edited_and_its_edit_stays_sealed() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "test-token" }))],
    );
    let token = row(&store, &who, "web.env.TOKEN");
    assert_eq!(
        (token.preview, token.lands, token.ride),
        (
            json!({ "secret": true }),
            json!({ "secret": false }),
            Ride::Rides
        )
    );

    let edited = store
        .write(&who, &merge_value("web.env.TOKEN", Some("prod-token")))
        .unwrap();
    let token = &edited.destinations[0].rows[0];
    assert_eq!(
        (&token.lands, token.ride),
        (&json!({ "secret": true }), Ride::Edited)
    );
    assert!(!format!("{edited:?}").contains("prod-token"));

    // Taking the Edit back lands it Not set again.
    store
        .write(&who, &merge_value("web.env.TOKEN", None))
        .unwrap();
    let token = row(&store, &who, "web.env.TOKEN");
    assert_eq!(
        (token.lands, token.ride),
        (json!({ "secret": false }), Ride::Rides)
    );
}

#[test]
fn an_edit_gives_a_riding_variable_its_value_and_only_a_variable() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[
            ("web.env.A", json!("2")),
            ("web.startCommand", json!("serve")),
        ],
    );
    store
        .write(&who, &merge_value("web.env.A", Some("prod")))
        .unwrap();
    let a = row(&store, &who, "web.env.A");
    assert_eq!(
        (a.lands, a.preview, a.ride),
        (json!("prod"), json!("2"), Ride::Edited)
    );

    for label in ["web", "web.startCommand", "web.env.NOPE"] {
        let refused = store.write(&who, &merge_value(label, Some("x")));
        assert!(refused.is_err(), "{label}: {refused:?}");
    }

    // pr-5 puts A back: the row leaves the list and its Edit is ignored.
    set(&store, &who, "pr-5", &[("web.env.A", json!("1"))]);
    assert_eq!(
        labels(&store, &who),
        [("web.startCommand".to_owned(), Ride::Rides)]
    );
}

#[test]
fn sizing_never_rides() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.replicas", json!(3))]);
    assert_eq!(labels(&store, &who), []);
}

#[test]
fn a_row_synced_now_leaves_the_list() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("2")), ("web.env.B", json!("new"))],
    );
    let review = store
        .read(
            &who,
            &SyncQuery {
                from: at("pr-5"),
                into: Some(at("production")),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &SyncChanges {
                from: at("pr-5"),
                into: Some(at("production")),
                close_after: false,
                version: review.version,
                picks: Some(vec!["web.env.A".into()]),
                skip: Vec::new(),
                values: std::collections::BTreeMap::new(),
                id: None,
            },
        )
        .unwrap();
    save(&store, &who, "production");
    assert_eq!(
        labels(&store, &who),
        [("web.env.B".to_owned(), Ride::Rides)]
    );
}

#[test]
fn retargeting_the_pull_request_forgets_its_edits() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    store
        .write(&who, &merge_value("web.env.A", Some("prod")))
        .unwrap();
    assert_eq!(row(&store, &who, "web.env.A").ride, Ride::Edited);

    // Onto `release`, which nothing deploys, the merge lands nowhere.
    pull(&store, &who, "release", "2026-09-29T10:00:01Z");
    assert_eq!(on_merge(&store, &who).destinations, []);
    pull(&store, &who, "main", "2026-09-29T10:00:02Z");
    assert_eq!(row(&store, &who, "web.env.A").ride, Ride::Rides);
}

// The merge.

const MERGE: &str = "3333333333333333333333333333333333333333";
const LATER: &str = "4444444444444444444444444444444444444444";

fn system(store: &ConfigStore, who: &Actor, event: &SystemEvent) -> Automated {
    let Written::Automated(automated) = store
        .system(&who.organization, event, &Trusted::default())
        .unwrap()
    else {
        panic!("a system event writes Automated")
    };
    automated
}

/// Pull request #5 merged at `commit`, as GitHub said at `updated`.
fn merged(store: &ConfigStore, who: &Actor, updated: &str, commit: &str) -> Automated {
    let event = SystemEvent::PullRequest(PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: backend::git_branch("search"),
        head: backend::sha(HEAD),
        target_branch: backend::git_branch("main"),
        commits: 1,
        open: false,
        merge_commit: Some(backend::sha(commit)),
        updated: ployz_store::GithubTimestamp::parse(updated).unwrap(),
    });
    system(store, who, &event)
}

/// A push to `main`.
fn push(base: Option<&str>, head: &str, changed: Option<&[&str]>) -> SystemEvent {
    SystemEvent::BranchHead(BranchHead {
        repository_id: backend::repo_id(11),
        branch: backend::git_branch("main"),
        base: base.map(backend::sha),
        head: Some(backend::sha(head)),
        changed: changed.map(|paths| paths.iter().map(|path| (*path).to_owned()).collect()),
    })
}

/// The Services each admitted Deployment targets.
fn deployed(automated: &Automated) -> Vec<Vec<String>> {
    automated
        .admitted
        .iter()
        .map(|admitted| {
            admitted
                .deployment
                .services
                .iter()
                .map(ToString::to_string)
                .collect()
        })
        .collect()
}

/// `environment`'s Working value at `path`.
fn value(store: &ConfigStore, who: &Actor, environment: &str, path: &str) -> Option<Value> {
    let query = EnvironmentQuery {
        environment: at(environment),
        path: None,
        all: true,
    };
    store
        .read(who, &query)
        .unwrap()
        .settings
        .into_iter()
        .find(|row| row.path.to_string() == path)
        .map(|row| row.value)
}

fn history(store: &ConfigStore, who: &Actor) -> Vec<SavedRevision> {
    store
        .read(
            who,
            &HistoryQuery {
                environment: at("production"),
            },
        )
        .unwrap()
        .revisions
}

fn production_diff(store: &ConfigStore, who: &Actor) -> DiffView {
    store
        .read(
            who,
            &DiffQuery {
                environment: at("production"),
            },
        )
        .unwrap()
}

/// Whether pr-5 is still listed.
fn preview_listed(store: &ConfigStore, who: &Actor) -> bool {
    store
        .read(who, &EnvironmentsQuery::default())
        .unwrap()
        .environments
        .iter()
        .any(|listing| listing.name.as_str() == "pr-5")
}

#[test]
fn the_merge_lands_every_riding_row_in_one_saved_revision_that_undo_takes_back() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("2")), ("web.env.B", json!("new"))],
    );
    unset(&store, &who, "pr-5", "web.env.D");
    // production changed A since: the merge lands over it.
    set(&store, &who, "production", &[("web.env.A", json!("5"))]);
    save(&store, &who, "production");
    let before = history(&store, &who).len();

    let automated = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let revisions = history(&store, &who);
    assert_eq!(revisions.len(), before + 1);
    let landed = revisions
        .iter()
        .find(|revision| revision.message.as_deref() == Some("Merged #5"))
        .unwrap();
    // Saved and Working agree: nothing is left to save.
    assert!(production_diff(&store, &who).published);
    assert_eq!(
        [
            value(&store, &who, "production", "web.env.A"),
            value(&store, &who, "production", "web.env.B"),
            value(&store, &who, "production", "web.env.C"),
            value(&store, &who, "production", "web.env.D"),
        ],
        [Some(json!("2")), Some(json!("new")), Some(json!("1")), None]
    );
    // It closes as before, once it landed.
    assert!(
        automated.closing.len() + automated.removed.len() == 1 || !preview_listed(&store, &who),
        "{automated:?}"
    );

    // Undo, from History, puts production back.
    let query = HistoryPreviewQuery {
        environment: at("production"),
        revision: landed.revision,
        action: HistoryAction::Undo,
    };
    let preview = store.read(&who, &query).unwrap();
    store
        .write(
            &who,
            &StageHistory {
                environment: at("production"),
                revision: query.revision,
                action: query.action,
                version: preview.version,
                accept_overwrite: false,
            },
        )
        .unwrap();
    assert_eq!(
        [
            value(&store, &who, "production", "web.env.A"),
            value(&store, &who, "production", "web.env.B"),
            value(&store, &who, "production", "web.env.D"),
        ],
        [Some(json!("5")), None, Some(json!("1"))]
    );
}

#[test]
fn the_merge_lands_edits_and_not_set_secrets_and_leaves_what_stays() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &NeverSync {
                environment: at("production"),
                rows: vec!["web.env.D".into()],
                off: false,
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "pr-5",
        &[
            ("web.env.A", json!("2")),
            ("web.env.B", json!("new")),
            ("web.env.C", json!("2")),
            ("web.env.D", json!("2")),
            ("web.env.TOKEN", json!({ "secret": "test-token" })),
        ],
    );
    exclude(&store, &who, "web.env.B", false);
    store
        .write(&who, &merge_value("web.env.C", Some("prod")))
        .unwrap();

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        [
            value(&store, &who, "production", "web.env.A"),
            value(&store, &who, "production", "web.env.B"),
            value(&store, &who, "production", "web.env.C"),
            value(&store, &who, "production", "web.env.D"),
            value(&store, &who, "production", "web.env.TOKEN"),
        ],
        [
            Some(json!("2")),
            None,
            Some(json!("prod")),
            Some(json!("1")),
            Some(json!({ "secret": false })),
        ]
    );
}

#[test]
fn the_merge_lands_once_whatever_order_its_events_come_in() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    // GitHub's merged event came in after a later read: still the first merge.
    pull(&store, &who, "main", "2026-09-29T12:00:00Z");
    let stale = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(stale.checks.len(), 1, "{stale:?}");
    let landed = history(&store, &who).len();
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("2"))
    );

    // production moves on; a replay or a later read lands nothing again.
    set(&store, &who, "production", &[("web.env.A", json!("7"))]);
    save(&store, &who, "production");
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    merged(&store, &who, "2026-09-29T13:00:00Z", MERGE);
    assert_eq!(history(&store, &who).len(), landed + 1);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("7"))
    );
}

#[test]
fn productions_staged_edit_stays_staged_over_what_landed() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("2")), ("web.env.B", json!("new"))],
    );
    set(&store, &who, "production", &[("web.env.A", json!("9"))]);

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("9"))
    );
    assert_eq!(
        value(&store, &who, "production", "web.env.B"),
        Some(json!("new"))
    );
    // Saved holds the landed A; the staged 9 wins at the next Save.
    let staged = serde_json::to_value(production_diff(&store, &who).draft_changes).unwrap();
    assert_eq!(
        staged[0]["settings"],
        json!([{
            "path": "web.env.A", "kind": "update", "before": "2", "after": "9",
            "canRestore": true, "row": staged[0]["settings"][0]["row"],
        }]),
        "{staged}"
    );
}

#[test]
fn what_landed_deploys_with_the_next_push_if_the_merge_came_first() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.watchPaths", json!(["web/**"]))],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(landed.admitted.is_empty(), "{landed:?}");
    // The merge's push changed nothing web watches: web deploys for what landed.
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(deployed(&pushed), [["web"]]);
    // Once.
    let next = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert!(next.admitted.is_empty(), "{next:?}");
}

#[test]
fn what_landed_deploys_at_once_if_the_push_came_first() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.watchPaths", json!(["web/**"]))],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed(&landed), [["web"]]);
}

#[test]
fn what_landed_follows_auto_deploy_and_wait_for_ci() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.autoDeploy", json!("false"))],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("2"))
    );
}

#[test]
fn what_landed_waits_for_ci_with_the_push() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[
            ("web.watchPaths", json!(["web/**"])),
            ("web.waitForCi", json!(true)),
        ],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(pushed.waiting.len(), 1, "{pushed:?}");
    let passed = system(
        &store,
        &who,
        &SystemEvent::CheckSuite(CheckSuite {
            repository_id: backend::repo_id(11),
            suite: 1,
            head: backend::sha(MERGE),
            status: serde_json::from_value(json!("completed")).unwrap(),
            conclusion: Some(serde_json::from_value(json!("success")).unwrap()),
            updated: ployz_store::GithubTimestamp::parse("2026-09-29T11:30:00Z").unwrap(),
        }),
    );
    assert_eq!(deployed(&passed), [["web"]]);
}

#[test]
fn a_merge_production_cant_take_lands_nothing_and_keeps_the_preview() {
    let (store, who) = shop();
    // A rides naming api, which stays here.
    store
        .write(
            &who,
            &ployz_store::CreateService {
                id: ServiceLineageId::parse(uuid(9)).unwrap(),
                environment: at("pr-5"),
                name: ServiceName::parse("api").unwrap(),
                image: Some("api:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("${{ api.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    exclude(&store, &who, "api", false);
    let before = history(&store, &who).len();

    let automated = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(automated.skipped.len(), 1, "{automated:?}");
    assert!(
        automated.skipped[0]
            .reason
            .starts_with("Didn't land #5 in production: "),
        "{automated:?}"
    );
    assert_eq!(history(&store, &who).len(), before);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("1"))
    );
    assert!(automated.closing.is_empty() && automated.removed.is_empty());
    assert!(preview_listed(&store, &who));
}

#[test]
fn a_kept_preview_has_nothing_left_on_merge_once_it_landed() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: None,
                start_from: None,
                copy: None,
                setup: None,
                remove_on_close: Some(false),
                include_bots: None,
            },
        )
        .unwrap();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    let main = backend::git_branch("main");
    assert_eq!(
        store
            .awaiting_merge(&who.organization, backend::repo_id(11), &main)
            .unwrap(),
        [backend::pr_number(5)]
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(preview_listed(&store, &who));
    assert!(
        store
            .awaiting_merge(&who.organization, backend::repo_id(11), &main)
            .unwrap()
            .is_empty()
    );
    let on_merge = on_merge(&store, &who);
    assert!(
        on_merge
            .destinations
            .iter()
            .all(|destination| destination.rows.is_empty()),
        "{on_merge:?}"
    );
}
