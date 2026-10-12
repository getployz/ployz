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
    HistoryQuery, MergeRow, NeverSync, NodeName, OnMerge, OnMergeCheck, OnMergeConclusion,
    OnMergeQuery, OrganizationId, ProjectId, ProjectName, Publish, PullRequest, PullRequestQuery,
    Ride, SavedRevision, ServiceLineageId, SetMergeValue, SetPrPlan, SettingPath, StageHistory,
    SyncChanges, SyncQuery, SystemEvent, Trusted, Written,
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
    shop_with(|_, _| {})
}

/// [`shop`], with `setup` run on `production` before pull request #5 opens.
fn shop_with(setup: impl FnOnce(&ConfigStore, &Actor)) -> (ConfigStore, Actor) {
    shop_copying(setup, None)
}

/// [`shop_with`], with pr-5 also copying `copy`.
fn shop_copying(
    setup: impl FnOnce(&ConfigStore, &Actor),
    copy: Option<Vec<NodeName>>,
) -> (ConfigStore, Actor) {
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
    setup(&store, &who);
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy,
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
        target_head: None,
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
    merged_within(store, who, updated, commit, None)
}

/// Pull request #5 merged at `commit`, with GitHub saying `main`'s head `target_head`
/// contains it.
fn merged_within(
    store: &ConfigStore,
    who: &Actor,
    updated: &str,
    commit: &str,
    target_head: Option<&str>,
) -> Automated {
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
        target_head: target_head.map(backend::sha),
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
                limit: None,
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
/// Whether the listing reads `pr-5` merged; none when it isn't listed.
fn listed_merged(store: &ConfigStore, who: &Actor) -> Option<bool> {
    store
        .read(who, &EnvironmentsQuery::default())
        .unwrap()
        .environments
        .iter()
        .find(|listing| listing.name.as_str() == "pr-5")
        .map(|listing| listing.merged)
}

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
fn an_edit_of_a_plain_variable_the_preview_made_secret_never_lands_in_plain_text() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.NEW", json!("2"))]);
    store
        .write(&who, &merge_value("web.env.NEW", Some("prod-token")))
        .unwrap();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.NEW", json!({ "secret": "test-token" }))],
    );

    // The Edit was for a plain value: the secret lands Not set, as unedited.
    let new = row(&store, &who, "web.env.NEW");
    assert_eq!(
        (new.lands, new.ride),
        (json!({ "secret": false }), Ride::Rides)
    );
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "web.env.NEW"),
        Some(json!({ "secret": false }))
    );
}

#[test]
fn an_edit_of_a_secret_the_preview_made_plain_lands_the_preview_value() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.NEW", json!({ "secret": "test-token" }))],
    );
    store
        .write(&who, &merge_value("web.env.NEW", Some("prod-token")))
        .unwrap();
    // A secret never turns plain in place: the preview unsets it, then sets it.
    unset(&store, &who, "pr-5", "web.env.NEW");
    set(&store, &who, "pr-5", &[("web.env.NEW", json!("2"))]);

    let new = row(&store, &who, "web.env.NEW");
    assert_eq!((new.lands, new.ride), (json!("2"), Ride::Rides));
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "web.env.NEW"),
        Some(json!("2"))
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
fn a_merge_whose_commit_was_never_pushed_deploys_from_the_head_that_contains_it() {
    let (store, who) = shop();
    watching(&store, &who);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    // Two merges in quick succession: Cloud read the branch at the second, so the
    // Store never heard of the first.
    let later = system(&store, &who, &push(Some(HEAD), LATER, Some(&["README.md"])));
    assert!(later.admitted.is_empty(), "{later:?}");

    let landed = merged_within(&store, &who, "2026-09-29T11:00:00Z", MERGE, Some(LATER));
    assert_eq!(deployed(&landed), [["web"]]);
}

#[test]
fn a_merge_contained_in_a_head_the_store_has_not_seen_waits_for_its_push() {
    let (store, who) = shop();
    watching(&store, &who);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);

    let landed = merged_within(&store, &who, "2026-09-29T11:00:00Z", MERGE, Some(LATER));
    assert!(landed.admitted.is_empty(), "{landed:?}");
    let pushed = system(&store, &who, &push(Some(HEAD), LATER, Some(&["README.md"])));
    assert_eq!(deployed(&pushed), [["web"]]);
}

#[test]
fn what_landed_deploys_with_auto_deploy_off() {
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
    // web follows no push, but its config changed: the merge's push deploys it.
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(deployed(&pushed), [["web"]]);
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
fn a_merge_into_a_branch_nothing_deploys_keeps_the_preview() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    pull(&store, &who, "release", "2026-09-29T10:00:01Z");
    let automated = system(
        &store,
        &who,
        &SystemEvent::PullRequest(PullRequest {
            repository_id: backend::repo_id(11),
            number: backend::pr_number(5),
            title: "Add search".into(),
            author: "ada".into(),
            bot: false,
            head_branch: backend::git_branch("search"),
            head: backend::sha(HEAD),
            target_branch: backend::git_branch("release"),
            commits: 1,
            open: false,
            merge_commit: Some(backend::sha(MERGE)),
            target_head: None,
            updated: ployz_store::GithubTimestamp::parse("2026-09-29T11:00:00Z").unwrap(),
        }),
    );
    // Nothing landed its change anywhere, so the preview holding it stays.
    assert!(
        automated.closing.is_empty() && automated.removed.is_empty(),
        "{automated:?}"
    );
    assert_eq!(listed_merged(&store, &who), Some(true));
    assert_eq!(value(&store, &who, "pr-5", "web.env.A"), Some(json!("2")));
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
    assert_eq!(listed_merged(&store, &who), Some(false));

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
    // Listed while it exists, read merged.
    assert_eq!(listed_merged(&store, &who), Some(true));
    let stuck = check(&store, &who).unwrap();
    assert_eq!(
        (stuck.conclusion, stuck.title.as_str()),
        (OnMergeConclusion::Neutral, "Didn't land in production")
    );
    assert!(
        stuck
            .summary
            .contains("web references a removed Service (A)"),
        "{stuck:?}"
    );
    // It links production's History: the PR Environment may be gone.
    assert_eq!(
        stuck.merged_into,
        Some(EnvironmentRef {
            project: Some(ProjectName::parse("shop").unwrap()),
            environment: Some(EnvironmentName::parse("production").unwrap()),
        })
    );
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
    assert_eq!(check(&store, &who), None);
}

/// What pull request #5's check says.
fn check(store: &ConfigStore, who: &Actor) -> Option<OnMergeCheck> {
    store
        .read(
            who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap()
        .check
}

#[test]
fn the_check_says_what_lands_and_reads_neutral_over_a_conflict_or_a_missing_secret() {
    let (store, who) = shop();
    assert_eq!(
        check(&store, &who),
        Some(OnMergeCheck {
            conclusion: OnMergeConclusion::Success,
            title: "No config changes".into(),
            summary: "No config changes.".into(),
            merged_into: None,
        })
    );

    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("2")), ("web.env.B", json!("new"))],
    );
    unset(&store, &who, "pr-5", "web.env.C");
    assert_eq!(
        check(&store, &who),
        Some(OnMergeCheck {
            conclusion: OnMergeConclusion::Success,
            title: "3 config changes".into(),
            summary: "- `web · A`: 1 → 2\n- `web · B`: new → new\n- `web · C`: 1 → removed".into(),
            merged_into: None,
        })
    );

    // production changed A since: the merge lands over it, said apart.
    set(&store, &who, "production", &[("web.env.A", json!("3"))]);
    save(&store, &who, "production");
    let conflict = check(&store, &who).unwrap();
    assert_eq!(conflict.conclusion, OnMergeConclusion::Neutral);
    assert!(
        conflict.summary.contains(
            "### Changed in production\n\n- `web · A`: production 3 → 2\n\n\
             The merge sets these. Undo in Ployz History reverts it."
        ),
        "{conflict:?}"
    );
}

#[test]
fn the_check_names_a_missing_secret_without_its_value_and_what_stays_here() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[
            ("web.env.TOKEN", json!({ "secret": "test-token" })),
            ("web.env.D", json!("2")),
        ],
    );
    exclude(&store, &who, "web.env.D", false);
    let missing = check(&store, &who).unwrap();
    assert_eq!(
        missing,
        OnMergeCheck {
            conclusion: OnMergeConclusion::Neutral,
            title: "1 config change · TOKEN not set".into(),
            summary: "- `web · TOKEN`: not set\n\n### Stays in pr-5\n\n- `web · D`: 1 → 2".into(),
            merged_into: None,
        }
    );

    // An Edit gives it a value, which the check never shows.
    store
        .write(&who, &merge_value("web.env.TOKEN", Some("prod-token")))
        .unwrap();
    let edited = check(&store, &who).unwrap();
    assert_eq!(
        (edited.conclusion, edited.title.as_str()),
        (OnMergeConclusion::Success, "1 config change")
    );
    assert!(edited.summary.contains("`web · TOKEN`: set"), "{edited:?}");
    assert!(!format!("{edited:?}").contains("prod-token"));
}

#[test]
fn the_check_reads_a_secret_the_preview_removed_as_removed() {
    let (store, who) = shop_with(|store, who| {
        set(
            store,
            who,
            "production",
            &[("web.env.TOKEN", json!({ "secret": "prod-token" }))],
        );
        save(store, who, "production");
    });
    unset(&store, &who, "pr-5", "web.env.TOKEN");
    let removed = check(&store, &who).unwrap();
    assert_eq!(
        removed.summary, "- `web · TOKEN`: set → removed",
        "{removed:?}"
    );
}

#[test]
fn the_check_says_nothing_deploys_from_a_branch_no_environment_tracks() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    pull(&store, &who, "release", "2026-09-29T10:00:01Z");
    assert_eq!(
        check(&store, &who),
        Some(OnMergeCheck {
            conclusion: OnMergeConclusion::Neutral,
            title: "Nothing deploys from release".into(),
            summary: "No Environment deploys from `release`, so the merge lands nowhere. \
                      Changes stay in the preview."
                .into(),
            merged_into: None,
        })
    );
}

// Review findings.

/// Pull request `number` from `head_branch` onto `target`, as GitHub said at `updated`.
fn event(
    number: u64,
    head_branch: &str,
    target: &str,
    merge: Option<&str>,
    open: bool,
    updated: &str,
) -> SystemEvent {
    SystemEvent::PullRequest(PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(number),
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: backend::git_branch(head_branch),
        head: backend::sha(HEAD),
        target_branch: backend::git_branch(target),
        commits: 1,
        open,
        merge_commit: merge.map(backend::sha),
        target_head: None,
        updated: ployz_store::GithubTimestamp::parse(updated).unwrap(),
    })
}

/// A push to `branch`.
fn push_on(branch: &str, base: Option<&str>, head: &str, changed: &[&str]) -> SystemEvent {
    SystemEvent::BranchHead(BranchHead {
        repository_id: backend::repo_id(11),
        branch: backend::git_branch(branch),
        base: base.map(backend::sha),
        head: Some(backend::sha(head)),
        changed: Some(changed.iter().map(|path| (*path).to_owned()).collect()),
    })
}

/// CI passed on `head`.
fn passed(store: &ConfigStore, who: &Actor, head: &str) -> Automated {
    system(
        store,
        who,
        &SystemEvent::CheckSuite(CheckSuite {
            repository_id: backend::repo_id(11),
            suite: 1,
            head: backend::sha(head),
            status: serde_json::from_value(json!("completed")).unwrap(),
            conclusion: Some(serde_json::from_value(json!("success")).unwrap()),
            updated: ployz_store::GithubTimestamp::parse("2026-09-29T11:30:00Z").unwrap(),
        }),
    )
}

/// PR Environments stay when their pull request closes.
fn keep(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
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
}

/// Every Service the admitted Deployments target, sorted.
fn deployed_services(automated: &Automated) -> Vec<String> {
    let mut services: Vec<String> = deployed(automated).into_iter().flatten().collect();
    services.sort();
    services
}

/// `web` deploys only for changes under `web/`; main's head is `HEAD`.
fn watching(store: &ConfigStore, who: &Actor) {
    set(
        store,
        who,
        "production",
        &[("web.watchPaths", json!(["web/**"]))],
    );
    system(store, who, &push(None, HEAD, None));
}

#[test]
fn a_kept_preview_closed_unmerged_awaits_no_merge() {
    let (store, who) = shop();
    keep(&store, &who);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    system(
        &store,
        &who,
        &event(5, "search", "main", None, false, "2026-09-29T11:00:00Z"),
    );
    assert!(preview_listed(&store, &who));
    let main = backend::git_branch("main");
    assert!(
        store
            .awaiting_merge(&who.organization, backend::repo_id(11), &main)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn what_landed_deploys_at_once_if_the_branch_already_moved_past_the_merge() {
    let (store, who) = shop();
    watching(&store, &who);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let later = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert!(later.admitted.is_empty(), "{later:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed(&landed), [["web"]]);
}

#[test]
fn a_merge_that_changes_only_a_mounted_config_deploys_its_service() {
    let config = || ployz_core::ConfigName::parse("sentry").unwrap();
    let file = |environment: EnvironmentRef, content: &str| ployz_store::PutConfigFile {
        environment,
        config: config(),
        file: ployz_core::ConfigFileName::parse("config.yml").unwrap(),
        content: content.into(),
        mode: None,
        uid: None,
        gid: None,
    };
    let (store, who) = shop_with(|store, who| {
        store
            .write(
                who,
                &ployz_store::CreateConfig {
                    id: ployz_store::ConfigId::parse(uuid(9)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: config(),
                    mounts: vec![ployz_store::ConfigMountAt {
                        service: ServiceName::parse("web").unwrap(),
                        dir: "/etc/sentry".into(),
                    }],
                },
            )
            .unwrap();
        store
            .write(who, &file(EnvironmentRef::default(), "level: info\n"))
            .unwrap();
        save(store, who, "production");
    });
    watching(&store, &who);
    store
        .write(&who, &file(at("pr-5"), "level: debug\n"))
        .unwrap();
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed(&landed), [["web"]]);
}

#[test]
fn a_merge_deploys_an_image_service_it_changed_and_the_services_that_read_it() {
    let (store, who) = shop_with(|store, who| {
        store
            .write(
                who,
                &ployz_store::CreateService {
                    id: ServiceLineageId::parse(uuid(9)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("api").unwrap(),
                    image: Some("api:1".into()),
                    template: None,
                },
            )
            .unwrap();
        set(
            store,
            who,
            "production",
            &[
                ("api.env.X", json!("1")),
                ("web.env.API", json!("${{ api.X }}")),
            ],
        );
        save(store, who, "production");
    });
    watching(&store, &who);
    set(&store, &who, "pr-5", &[("api.env.X", json!("2"))]);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed_services(&landed), ["api", "web"]);
}

#[test]
fn a_merge_that_renames_a_private_dns_deploys_the_services_that_read_its_address() {
    let (store, who) = shop_with(|store, who| {
        store
            .write(
                who,
                &ployz_store::CreateService {
                    id: ServiceLineageId::parse(uuid(9)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("api").unwrap(),
                    image: Some("api:1".into()),
                    template: None,
                },
            )
            .unwrap();
        set(
            store,
            who,
            "production",
            &[("web.env.API", json!("${{ api.PLOYZ_PRIVATE_DOMAIN }}"))],
        );
        save(store, who, "production");
    });
    watching(&store, &who);
    set(
        &store,
        &who,
        "pr-5",
        &[("api.privateDns", json!("backend"))],
    );
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed_services(&landed), ["api", "web"]);
}

#[test]
fn a_deploy_warns_only_about_the_services_it_ships() {
    let (store, who) = shop_with(|store, who| {
        store
            .write(
                who,
                &ployz_store::CreateService {
                    id: ServiceLineageId::parse(uuid(9)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("db").unwrap(),
                    image: Some("db:1".into()),
                    template: None,
                },
            )
            .unwrap();
        // web reads db, so the preview has db too.
        set(
            store,
            who,
            "production",
            &[("web.env.DB", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"))],
        );
        save(store, who, "production");
    });
    watching(&store, &who);
    set(
        &store,
        &who,
        "pr-5",
        &[("db.env.TOKEN", json!({ "secret": "test-token" }))],
    );
    system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed_services(&landed), ["db"]);
    let warnings = |automated: &Automated| {
        let claimed = store
            .claim(
                &automated.admitted[0].deployment.id,
                &ployz_store::RunnerId::parse("runner").unwrap(),
            )
            .unwrap();
        json!(claimed.deployment.warnings)
    };
    let not_set = json!([{ "service": "db", "variable": "TOKEN" }]);
    assert_eq!(warnings(&landed), not_set);

    // A push under web/ ships web alone: db's Not set secret isn't its to warn about.
    let pushed = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["web/app.rs"])),
    );
    assert_eq!(deployed_services(&pushed), ["web"]);
    assert_eq!(warnings(&pushed), json!([]));
}

#[test]
fn what_landed_waits_for_ci_on_the_newest_head() {
    let (store, who) = shop();
    watching(&store, &who);
    set(
        &store,
        &who,
        "production",
        &[("web.waitForCi", json!(true))],
    );
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(pushed.waiting.len(), 1, "{pushed:?}");
    // Another push before CI passed: what landed waits on it instead.
    let later = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert_eq!(later.waiting.len(), 1, "{later:?}");

    assert_eq!(deployed(&passed(&store, &who, LATER)), [["web"]]);
}

#[test]
fn a_push_without_a_head_keeps_what_landed() {
    let (store, who) = shop();
    // web follows no push: only what landed deploys it, after the branch comes back.
    set(
        &store,
        &who,
        "production",
        &[("web.autoDeploy", json!("false"))],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let deleted = SystemEvent::BranchHead(BranchHead {
        repository_id: backend::repo_id(11),
        branch: backend::git_branch("main"),
        base: Some(backend::sha(HEAD)),
        head: None,
        changed: None,
    });
    system(&store, &who, &deleted);

    let pushed = system(&store, &who, &push(None, MERGE, Some(&["README.md"])));
    assert_eq!(deployed(&pushed), [["web"]]);
}

#[test]
fn a_new_git_service_from_the_merged_repository_lands_tracking_the_target_branch() {
    let (store, who) = shop();
    system(&store, &who, &push(None, HEAD, None));
    // The preview adds worker from its own repository, built from another branch.
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(8)).unwrap(),
                environment: at("pr-5"),
                name: ServiceName::parse("worker").unwrap(),
                repository: backend::repo_name("acme/web"),
                branch: Some(backend::git_branch("release")),
            },
            &vouched(),
        )
        .unwrap();
    save(&store, &who, "pr-5");

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "worker.branch"),
        Some(json!("main"))
    );
    // The merge push deploys it, and so does the next push to main.
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(deployed(&pushed), [["web", "worker"]]);
    let later = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert_eq!(deployed(&later), [["web", "worker"]]);
}

#[test]
fn merges_from_two_branches_into_one_environment_each_deploy() {
    let (store, who) = shop_with(|store, who| {
        store
            .write_trusted(
                who,
                &CreateGitService {
                    id: ServiceLineageId::parse(uuid(8)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("worker").unwrap(),
                    repository: backend::repo_name("acme/web"),
                    branch: Some(backend::git_branch("release")),
                },
                &vouched(),
            )
            .unwrap();
        save(store, who, "production");
    });
    set(
        &store,
        &who,
        "production",
        &[("worker.watchPaths", json!(["worker/**"]))],
    );
    watching(&store, &who);
    system(&store, &who, &push_on("release", None, HEAD, &[]));
    system(
        &store,
        &who,
        &event(6, "fix", "release", None, true, "2026-09-29T10:00:00Z"),
    );
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    set(&store, &who, "pr-6", &[("worker.env.W", json!("2"))]);

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    system(
        &store,
        &who,
        &event(
            6,
            "fix",
            "release",
            Some(LATER),
            false,
            "2026-09-29T11:00:00Z",
        ),
    );
    let main = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(deployed(&main), [["web"]]);
    let release = system(
        &store,
        &who,
        &push_on("release", Some(HEAD), LATER, &["README.md"]),
    );
    assert_eq!(deployed(&release), [["worker"]]);
}

#[test]
fn a_kept_preview_edited_after_its_merge_still_reads_landed() {
    let (store, who) = shop();
    keep(&store, &who);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    store
        .write(&who, &merge_value("web.env.A", Some("3")))
        .unwrap();

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("3"))
    );
    assert_eq!(check(&store, &who), None);
    set(&store, &who, "pr-5", &[("web.env.A", json!("5"))]);
    assert_eq!(check(&store, &who), None);
}

#[test]
fn a_stale_merged_event_still_closes_the_preview() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    pull(&store, &who, "main", "2026-09-29T12:00:00Z");

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("2"))
    );
    assert!(!preview_listed(&store, &who));
}

#[test]
fn an_excluded_rows_edit_can_always_be_cleared() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    store
        .write(&who, &merge_value("web.env.A", Some("9")))
        .unwrap();
    exclude(&store, &who, "web.env.A", false);

    store.write(&who, &merge_value("web.env.A", None)).unwrap();
    exclude(&store, &who, "web.env.A", true);
    let a = row(&store, &who, "web.env.A");
    assert_eq!((a.lands, a.ride), (json!("2"), Ride::Rides));
}

#[test]
fn a_removal_takes_no_edit_and_lands_as_a_removal() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    store
        .write(&who, &merge_value("web.env.A", Some("9")))
        .unwrap();
    // The pull request then removes A: its Edit no longer applies.
    unset(&store, &who, "pr-5", "web.env.A");
    let a = row(&store, &who, "web.env.A");
    assert_eq!((a.lands, a.ride), (Value::Null, Ride::Rides));
    let refused = store
        .write(&who, &merge_value("web.env.A", Some("9")))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{refused:?}");

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(value(&store, &who, "production", "web.env.A"), None);
}

#[test]
fn a_config_file_reading_a_secret_the_merge_left_not_set_warns() {
    let (store, who) = shop();
    watching(&store, &who);
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "test-token" }))],
    );
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let config = || ployz_core::ConfigName::parse("sentry").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(9)).unwrap(),
                environment: EnvironmentRef::default(),
                name: config(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::PutConfigFile {
                environment: EnvironmentRef::default(),
                config: config(),
                file: ployz_core::ConfigFileName::parse("config.yml").unwrap(),
                content: "token: \"${{ web.TOKEN }}\"\n".into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
    save(&store, &who, "production");

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let id = &pushed.admitted[0].deployment.id;
    let claimed = store
        .claim(id, &ployz_store::RunnerId::parse("runner").unwrap())
        .unwrap();
    let warnings = json!(claimed.deployment.warnings);
    let warned = json!({ "config": "sentry", "file": "config.yml", "secret": "web.TOKEN" });
    assert!(warnings.as_array().unwrap().contains(&warned), "{warnings}");
}

#[test]
fn a_merge_reported_after_newer_facts_still_reads_merged_on_the_check() {
    let (store, who) = shop();
    keep(&store, &who);
    // A rides naming api, which stays here: production can't take the merge.
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
    system(
        &store,
        &who,
        &event(5, "search", "main", None, false, "2026-09-29T11:00:01Z"),
    );
    system(
        &store,
        &who,
        &event(
            5,
            "search",
            "main",
            Some(MERGE),
            false,
            "2026-09-29T11:00:00Z",
        ),
    );

    let view = store
        .read(
            &who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap();
    assert_eq!(
        view.pull_request.and_then(|facts| facts.merge_commit),
        Some(backend::sha(MERGE))
    );
    let stuck = view.check.unwrap();
    assert_eq!(
        (stuck.conclusion, stuck.title.as_str()),
        (OnMergeConclusion::Neutral, "Didn't land in production")
    );
}

#[test]
fn the_check_reads_a_config_file_row_as_a_changed_file() {
    let config = || ployz_core::ConfigName::parse("sentry").unwrap();
    let file = |environment: EnvironmentRef, content: &str| ployz_store::PutConfigFile {
        environment,
        config: config(),
        file: ployz_core::ConfigFileName::parse("config.yml").unwrap(),
        content: content.into(),
        mode: None,
        uid: None,
        gid: None,
    };
    let (store, who) = shop_with(|store, who| {
        store
            .write(
                who,
                &ployz_store::CreateConfig {
                    id: ployz_store::ConfigId::parse(uuid(9)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: config(),
                    mounts: vec![ployz_store::ConfigMountAt {
                        service: ServiceName::parse("web").unwrap(),
                        dir: "/etc/sentry".into(),
                    }],
                },
            )
            .unwrap();
        store
            .write(who, &file(EnvironmentRef::default(), "level: info\n"))
            .unwrap();
        save(store, who, "production");
    });
    store
        .write(&who, &file(at("pr-5"), "level: debug\n"))
        .unwrap();

    let check = check(&store, &who).unwrap();
    assert_eq!(check.summary, "- `configs.sentry · config.yml`: changed");
}

#[test]
fn a_merge_reported_after_a_retarget_lands_in_the_branch_it_merged_into() {
    let (store, who) = shop();
    // Newer facts point the pull request at release, which no Environment follows.
    system(
        &store,
        &who,
        &event(5, "search", "release", None, true, "2026-09-29T11:00:01Z"),
    );
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    // The older merge into main still lands in production, main's Environment.
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);

    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("2"))
    );
    let view = store
        .read(
            &who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap();
    assert_eq!(
        view.pull_request.map(|facts| facts.target_branch),
        Some(backend::git_branch("main"))
    );
}

#[test]
fn a_full_manual_deploy_takes_what_landed_from_the_next_push() {
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
    // Someone deploys production by hand: web ships with what landed.
    store
        .write_trusted(
            &who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: ployz_store::DeploymentId::parse(::uuid::Uuid::new_v4().to_string()).unwrap(),
                environment: at("production"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    // A later saved edit waits for a Deploy: auto-deploy is off.
    set(&store, &who, "production", &[("web.env.C", json!("2"))]);
    save(&store, &who, "production");

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(deployed(&pushed).is_empty(), "{pushed:?}");
}

#[test]
fn two_merges_before_their_pushes_each_deploy_with_their_own_commit() {
    let (store, who) = shop_with(|store, who| {
        for (n, name) in [(9, "api"), (10, "db")] {
            store
                .write(
                    who,
                    &ployz_store::CreateService {
                        id: ServiceLineageId::parse(uuid(n)).unwrap(),
                        environment: EnvironmentRef::default(),
                        name: ServiceName::parse(name).unwrap(),
                        image: Some(format!("{name}:1")),
                        template: None,
                    },
                )
                .unwrap();
        }
        set(
            store,
            who,
            "production",
            &[
                ("web.env.API", json!("${{ api.PLOYZ_PRIVATE_DOMAIN }}")),
                ("web.env.DB", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}")),
            ],
        );
        save(store, who, "production");
    });
    watching(&store, &who);
    system(
        &store,
        &who,
        &event(6, "fix", "main", None, true, "2026-09-29T10:00:00Z"),
    );
    set(&store, &who, "pr-5", &[("api.env.X", json!("2"))]);
    set(&store, &who, "pr-6", &[("db.env.Y", json!("2"))]);

    // Both merge before GitHub reports either push.
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    system(
        &store,
        &who,
        &event(6, "fix", "main", Some(LATER), false, "2026-09-29T11:00:00Z"),
    );
    let first = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(deployed_services(&first), ["api"]);
    let second = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert_eq!(deployed_services(&second), ["db"]);
}

#[test]
fn a_merge_that_renames_a_git_service_keeps_it_on_the_commit_it_last_built() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.autoDeploy", json!("false"))],
    );
    system(&store, &who, &push(None, HEAD, None));
    // web builds HEAD by hand.
    let built = ployz_store::DeploymentId::parse(::uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .write_trusted(
            &who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: built.clone(),
                environment: at("production"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let web = ServiceName::parse("web").unwrap();
    store
        .pin(
            &built,
            &std::collections::BTreeMap::from([(web.clone(), backend::sha(HEAD))]),
        )
        .unwrap();
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    store.claim(&built, &runner).unwrap();
    store
        .record(
            &built,
            &runner,
            ployz_store::RunEvidence::Build(ployz_store::BuildReport {
                service: web,
                status: ployz_store::BuildStatus::Built,
                message: None,
                log: String::new(),
            }),
        )
        .unwrap();

    set(&store, &who, "pr-5", &[("web.privateDns", json!("site"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let [admitted] = pushed.admitted.as_slice() else {
        panic!("one Deployment: {pushed:?}")
    };
    // web follows no push: the renamed Service ships the code it last built.
    let sources = store.sources(&admitted.deployment.id).unwrap();
    assert_eq!(
        sources
            .iter()
            .map(|source| (source.service.as_str(), source.commit.clone()))
            .collect::<Vec<_>>(),
        [("site", Some(backend::sha(HEAD)))]
    );
}

#[test]
fn a_merge_that_moves_a_git_service_to_another_repository_never_pins_the_old_build() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.autoDeploy", json!("false"))],
    );
    system(&store, &who, &push(None, HEAD, None));
    // web builds HEAD by hand.
    let built = ployz_store::DeploymentId::parse(::uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .write_trusted(
            &who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: built.clone(),
                environment: at("production"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let web = ServiceName::parse("web").unwrap();
    store
        .pin(
            &built,
            &std::collections::BTreeMap::from([(web.clone(), backend::sha(HEAD))]),
        )
        .unwrap();
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    store.claim(&built, &runner).unwrap();
    store
        .record(
            &built,
            &runner,
            ployz_store::RunEvidence::Build(ployz_store::BuildReport {
                service: web,
                status: ployz_store::BuildStatus::Built,
                message: None,
                log: String::new(),
            }),
        )
        .unwrap();

    // The preview moves web to another repository; trusted, as a Cloud edit is.
    let mut api = vouched();
    api.repositories.push(AuthorizedRepository {
        repository: backend::repo_name("acme/api"),
        repository_id: backend::repo_id(12),
        access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
        default_branch: backend::git_branch("main"),
        branches: Vec::new(),
    });
    store
        .write_trusted(
            &who,
            &Edit {
                environment: at("pr-5"),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("web.repository").unwrap(),
                    value: json!("acme/api"),
                }],
            },
            &api,
        )
        .unwrap();
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let [admitted] = pushed.admitted.as_slice() else {
        panic!("one Deployment: {pushed:?}")
    };
    // web's last build came from acme/web: on acme/api it resolves its branch's head.
    let sources = store.sources(&admitted.deployment.id).unwrap();
    assert_eq!(
        sources
            .iter()
            .map(|source| (source.service.as_str(), source.commit.clone()))
            .collect::<Vec<_>>(),
        [("web", None)]
    );
}

// Node removals.

fn node(name: &str) -> NodeName {
    NodeName::parse(name).unwrap()
}

fn image_service(store: &ConfigStore, who: &Actor, environment: &str, n: u8, name: &str) {
    store
        .write(
            who,
            &ployz_store::CreateService {
                id: ServiceLineageId::parse(uuid(n)).unwrap(),
                environment: at(environment),
                name: ServiceName::parse(name).unwrap(),
                image: Some(format!("{name}:1")),
                template: None,
            },
        )
        .unwrap();
}

fn volume(store: &ConfigStore, who: &Actor, n: u8, name: &str, on: &str) {
    store
        .write(
            who,
            &ployz_store::CreateVolume {
                shared_writes: false,
                storage_locked: false,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: ployz_store::VolumeId::parse(uuid(n)).unwrap(),
                environment: at("production"),
                name: ployz_store::VolumeName::parse(name).unwrap(),
                mounts: vec![ployz_store::Mount {
                    service: ServiceName::parse(on).unwrap(),
                    path: format!("/var/lib/{name}"),
                }],
            },
        )
        .unwrap();
}

fn config(store: &ConfigStore, who: &Actor, n: u8, name: &str, on: &[&str]) {
    store
        .write(
            who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(n)).unwrap(),
                environment: at("production"),
                name: ployz_core::ConfigName::parse(name).unwrap(),
                mounts: on
                    .iter()
                    .map(|service| ployz_store::ConfigMountAt {
                        service: ServiceName::parse(*service).unwrap(),
                        dir: format!("/etc/{name}"),
                    })
                    .collect(),
            },
        )
        .unwrap();
}

/// production's `db`, its Volume `data` and the Config `sentry` that `web` mounts,
/// beside `worker`, its Volume `cache` and an unmounted Config `extra`, all saved.
/// `web.env.DB_URL` references `db`.
fn production_nodes(store: &ConfigStore, who: &Actor) {
    image_service(store, who, "production", 20, "db");
    image_service(store, who, "production", 21, "worker");
    volume(store, who, 22, "data", "db");
    volume(store, who, 23, "cache", "worker");
    config(store, who, 24, "sentry", &["web"]);
    config(store, who, 25, "extra", &[]);
    store
        .write(
            who,
            &ployz_store::PutConfigFile {
                environment: at("production"),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
                file: ployz_core::ConfigFileName::parse("sentry.yml").unwrap(),
                content: "dsn: ${{ web.PLOYZ_PRIVATE_DOMAIN }}".into(),
                mode: Some(ployz_core::config::FileMode::parse("0640").unwrap()),
                uid: Some(1000),
                gid: None,
            },
        )
        .unwrap();
    set(
        store,
        who,
        "production",
        &[
            ("web.env.DB_URL", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}")),
            ("db.env.PASSWORD", json!({ "secret": "hunter2" })),
            ("db.env.USER", json!("app")),
            (
                "db.registryCredential",
                json!({ "username": "bot", "secret": "pull-token" }),
            ),
        ],
    );
    save(store, who, "production");
}

/// [`shop`] over [`production_nodes`]: pr-5 copies `web`, `sentry`, `db` and
/// `data`, and leaves `worker`, `cache` and `extra` in production.
fn shop_with_nodes() -> (ConfigStore, Actor) {
    shop_copying(
        production_nodes,
        Some(vec![
            node("db"),
            node("volumes.data"),
            node("configs.sentry"),
        ]),
    )
}

fn remove_service(store: &ConfigStore, who: &Actor, environment: &str, service: &str) {
    store
        .write(
            who,
            &ployz_store::RemoveService {
                environment: at(environment),
                service: ServiceName::parse(service).unwrap(),
            },
        )
        .unwrap();
}

/// pr-5 stops referencing `db` and removes it.
fn remove_db(store: &ConfigStore, who: &Actor) {
    set(
        store,
        who,
        "pr-5",
        &[("web.env.DB_URL", json!("db.internal"))],
    );
    remove_service(store, who, "pr-5", "db");
}

/// Whether `environment`'s latest Saved revision has `name`, by what Working
/// changes against it.
fn saved_has(store: &ConfigStore, who: &Actor, environment: &str, name: &str) -> bool {
    let working = value(store, who, environment, &format!("{name}.image")).is_some();
    let diff = store
        .read(
            who,
            &DiffQuery {
                environment: at(environment),
            },
        )
        .unwrap();
    let drafted = diff.draft_changes.iter().find(|change| change.name == name);
    match drafted.map(|change| change.lifecycle) {
        Some(ployz_core::config::ReviewLifecycleKind::Create) => false,
        Some(ployz_core::config::ReviewLifecycleKind::Delete) => true,
        _ => working,
    }
}

#[test]
fn production_nodes_the_pr_environment_never_copied_make_no_rows() {
    let (store, who) = shop_with_nodes();
    assert_eq!(labels(&store, &who), []);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    assert_eq!(
        labels(&store, &who),
        [("web.env.A".to_owned(), Ride::Rides)]
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    for name in ["db", "worker"] {
        assert!(saved_has(&store, &who, "production", name), "{name}");
    }
}

#[test]
fn a_removed_service_rides_and_the_merge_takes_it_out_of_production() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    let db = row(&store, &who, "db");
    assert_eq!(
        (db.lands, db.preview, db.ride, db.conflict, db.refused_by),
        (Value::Null, Value::Null, Ride::Rides, false, vec![])
    );
    assert_ne!(db.production, Value::Null);
    assert_eq!(row(&store, &who, "web.env.DB_URL").ride, Ride::Rides);

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(!saved_has(&store, &who, "production", "db"));
    assert_eq!(value(&store, &who, "production", "db.image"), None);
    assert_eq!(
        value(&store, &who, "production", "web.env.DB_URL"),
        Some(json!("db.internal"))
    );
    assert!(production_diff(&store, &who).published);
    // The rest of production stays.
    assert!(saved_has(&store, &who, "production", "worker"));
}

#[test]
fn an_excluded_removal_keeps_the_node_until_included_again() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    exclude(&store, &who, "db", false);
    assert_eq!(row(&store, &who, "db").ride, Ride::Excluded);
    exclude(&store, &who, "db", true);
    assert_eq!(row(&store, &who, "db").ride, Ride::Rides);
    exclude(&store, &who, "db", false);

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(saved_has(&store, &who, "production", "db"));
    assert_eq!(
        value(&store, &who, "production", "web.env.DB_URL"),
        Some(json!("db.internal"))
    );
}

#[test]
fn removing_a_node_production_changed_since_is_a_conflict() {
    let (store, who) = shop_with_nodes();
    set(
        &store,
        &who,
        "production",
        &[("db.env.USER", json!("admin"))],
    );
    save(&store, &who, "production");
    remove_db(&store, &who);
    let db = row(&store, &who, "db");
    assert_eq!((db.ride, db.conflict), (Ride::Rides, true));
}

#[test]
fn a_removal_production_still_references_stays_in_use_and_the_rest_lands() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    // production's worker references db since pr-5 started.
    set(
        &store,
        &who,
        "production",
        &[("worker.env.DB", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    save(&store, &who, "production");
    let db = row(&store, &who, "db");
    assert_eq!(
        (
            db.ride,
            db.refused_by
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        ),
        (Ride::Rides, vec!["worker.env.DB".to_owned()])
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(saved_has(&store, &who, "production", "db"));
    assert_eq!(
        value(&store, &who, "production", "web.env.DB_URL"),
        Some(json!("db.internal"))
    );
    assert!(production_diff(&store, &who).published);
}

#[test]
fn a_removal_is_refused_by_the_value_that_lands_not_the_previews() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    // Excluding the edit that stopped referencing db keeps production's reference.
    exclude(&store, &who, "web.env.DB_URL", false);
    let refused: Vec<String> = row(&store, &who, "db")
        .refused_by
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(refused, ["web.env.DB_URL"]);
    exclude(&store, &who, "web.env.DB_URL", true);
    assert_eq!(row(&store, &who, "db").refused_by, []);

    // An Edit landing the reference refuses it too.
    store
        .write(
            &who,
            &merge_value("web.env.DB_URL", Some("${{ db.PLOYZ_PRIVATE_DOMAIN }}")),
        )
        .unwrap();
    assert_eq!(row(&store, &who, "db").refused_by.len(), 1);
}

#[test]
fn a_removed_volume_is_kept_with_its_data() {
    let (store, who) = shop_with_nodes();
    store
        .write(
            &who,
            &ployz_store::RemoveVolume {
                environment: at("pr-5"),
                volume: ployz_store::VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    let data = row(&store, &who, "volumes.data");
    assert_eq!((data.ride, &data.lands), (Ride::Kept, &data.production));
    assert_ne!(data.production, Value::Null);
    // db keeps its mount on the Volume it keeps.
    assert_eq!(
        labels(&store, &who),
        [
            ("db.mounts.data".to_owned(), Ride::Kept),
            ("volumes.data".to_owned(), Ride::Kept)
        ]
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "db.mounts.data"),
        Some(json!("/var/lib/data"))
    );
    let kept = store.read(
        &who,
        &ployz_store::VolumeQuery {
            environment: at("production"),
            volume: ployz_store::VolumeName::parse("data").unwrap(),
        },
    );
    assert!(kept.is_ok(), "{kept:?}");
    assert!(production_diff(&store, &who).published);
}

#[test]
fn a_draft_on_a_removed_service_stays_in_working_while_saved_takes_the_removal() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    // Only whether USER is exported changes: its value, and so its cell, doesn't.
    set(
        &store,
        &who,
        "production",
        &[("db.env.USER.exported", json!("true"))],
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(!saved_has(&store, &who, "production", "db"));
    assert_eq!(
        value(&store, &who, "production", "db.env.USER.exported"),
        Some(json!(true))
    );
}

#[test]
fn undo_brings_a_removed_service_and_config_back_whole() {
    let (store, who) = shop_with_nodes();
    let snapshot = |store: &ConfigStore| {
        let settings = store
            .read(
                &who,
                &EnvironmentQuery {
                    environment: at("production"),
                    path: None,
                    all: true,
                },
            )
            .unwrap()
            .settings;
        let db = store
            .read(
                &who,
                &ployz_store::ServiceQuery {
                    environment: at("production"),
                    service: ServiceName::parse("db").unwrap(),
                },
            )
            .unwrap();
        let configs = store
            .read(
                &who,
                &ployz_store::ConfigsQuery {
                    environment: at("production"),
                },
            )
            .unwrap()
            .configs;
        json!({
            "settings": settings,
            "db": [db.service, db.lineage, db.values],
            "configs": configs,
        })
    };
    let before = snapshot(&store);
    remove_db(&store, &who);
    store
        .write(
            &who,
            &ployz_store::DeleteConfig {
                environment: at("pr-5"),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(row(&store, &who, "configs.sentry").ride, Ride::Rides);

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(!saved_has(&store, &who, "production", "db"));
    let landed = history(&store, &who)
        .into_iter()
        .find(|revision| revision.message.as_deref() == Some("Merged #5"))
        .unwrap();
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
    // IDs, sealed values, mounts, files, and the registry credential kept by ID.
    assert_eq!(snapshot(&store), before);
}

#[test]
fn each_destination_decides_its_own_removal() {
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
    // production deploys `release`; its Branches staging and qa deploy `main`.
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(3)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                repository: backend::repo_name("acme/web"),
                branch: Some(backend::git_branch("release")),
            },
            &vouched(),
        )
        .unwrap();
    image_service(&store, &who, "production", 20, "db");
    save(&store, &who, "production");
    for (n, name) in [(30, "staging"), (31, "qa")] {
        store
            .write(
                &who,
                &ployz_store::CreateBranch {
                    id: EnvironmentId::parse(uuid(n)).unwrap(),
                    from: at("production"),
                    name: EnvironmentName::parse(name).unwrap(),
                    copy: vec![node("web"), node("db")],
                    live: Vec::new(),
                    setup: Vec::new(),
                    keep: true,
                    fix: None,
                },
            )
            .unwrap();
        set(&store, &who, name, &[("web.branch", json!("main"))]);
        save(&store, &who, name);
    }
    // qa references db since.
    set(
        &store,
        &who,
        "qa",
        &[("web.env.DB_URL", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    save(&store, &who, "qa");
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy: Some(vec![node("db")]),
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    pull(&store, &who, "main", "2026-09-29T10:00:00Z");
    remove_service(&store, &who, "pr-5", "db");

    let destinations: Vec<_> = on_merge(&store, &who)
        .destinations
        .into_iter()
        .map(|destination| {
            let rows: Vec<_> = destination
                .rows
                .into_iter()
                .map(|row| (row.at.to_string(), row.ride, row.refused_by.len()))
                .collect();
            (destination.name.to_string(), rows)
        })
        .collect();
    assert_eq!(
        destinations,
        [
            (
                "staging".to_owned(),
                vec![("db".to_owned(), Ride::Rides, 0)]
            ),
            ("qa".to_owned(), vec![("db".to_owned(), Ride::Rides, 1)]),
        ]
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(!saved_has(&store, &who, "staging", "db"));
    assert!(saved_has(&store, &who, "qa", "db"));
    assert!(saved_has(&store, &who, "production", "db"));
}

#[test]
fn the_check_names_a_removal_apart_from_values_and_reads_neutral_while_it_is_in_use() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    store
        .write(
            &who,
            &ployz_store::RemoveVolume {
                environment: at("pr-5"),
                volume: ployz_store::VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    let removes = check(&store, &who).unwrap();
    assert_eq!(
        removes,
        OnMergeCheck {
            conclusion: OnMergeConclusion::Success,
            title: "2 config changes".into(),
            summary: "- `web · DB_URL`: ${{ db.PLOYZ_PRIVATE_DOMAIN }} → db.internal\n\
                      - Removes `db`\n\n\
                      - `volumes.data`: Kept"
                .into(),
            merged_into: None,
        }
    );

    // production's worker references db since pr-5 started: the removal waits.
    set(
        &store,
        &who,
        "production",
        &[("worker.env.DB", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    save(&store, &who, "production");
    let in_use = check(&store, &who).unwrap();
    assert_eq!(
        in_use,
        OnMergeCheck {
            conclusion: OnMergeConclusion::Neutral,
            title: "1 config change".into(),
            summary: "- `web · DB_URL`: ${{ db.PLOYZ_PRIVATE_DOMAIN }} → db.internal\n\n\
                      - `db`: In use · worker.env.DB\n\n\
                      - `volumes.data`: Kept"
                .into(),
            merged_into: None,
        }
    );
}

#[test]
fn the_check_says_a_removal_was_changed_there() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    set(&store, &who, "production", &[("db.env.POOL", json!("10"))]);
    save(&store, &who, "production");
    assert_eq!(
        check(&store, &who).unwrap().summary,
        "- `web · DB_URL`: ${{ db.PLOYZ_PRIVATE_DOMAIN }} → db.internal\n\n\
         ### Changed in production\n\n\
         - Removes `db` · Changed there\n\n\
         The merge sets these. Undo in Ployz History reverts it."
    );
}

fn runner() -> ployz_store::RunnerId {
    ployz_store::RunnerId::parse("runner").unwrap()
}

/// Run Deployment `id` to its end, touching `services`.
fn run(store: &ConfigStore, id: &ployz_store::DeploymentId, services: &[&str]) {
    run_seeing(store, id, services, true);
}

/// Run Deployment `id` to its end, touching `services`, with a snapshot of every
/// Server when `complete`.
fn run_seeing(
    store: &ConfigStore,
    id: &ployz_store::DeploymentId,
    services: &[&str],
    complete: bool,
) {
    let claimed = store.claim(id, &runner()).unwrap();
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
        "warnings": [], "would_remove": [], "preserved_volumes": [],
        "prune_refusal": (!complete).then_some("incomplete_snapshot")
    }))
    .unwrap();
    store
        .record(id, &runner(), ployz_store::RunEvidence::Prepared(preview))
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
            &runner(),
            ployz_store::RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

/// Deploy `services` of production by hand.
fn deploy_by_hand(
    store: &ConfigStore,
    who: &Actor,
    services: &[&str],
) -> ployz_store::DeploymentId {
    let id = ployz_store::DeploymentId::parse(::uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .write_trusted(
            who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: id.clone(),
                environment: at("production"),
                services: services
                    .iter()
                    .map(|service| ServiceName::parse(*service).unwrap())
                    .collect(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    id
}

/// production's `db` and `worker` run.
fn running(store: &ConfigStore, who: &Actor) {
    let id = deploy_by_hand(store, who, &["db", "worker"]);
    run(store, &id, &["db", "worker"]);
}

/// What the one Deployment `automated` admitted tells its runner: the Services it
/// selects, the ones it tears down, and the Volumes it targets.
fn teardown(store: &ConfigStore, who: &Actor, automated: &Automated) -> [Vec<String>; 3] {
    let [admitted] = automated.admitted.as_slice() else {
        panic!("one Deployment: {automated:?}")
    };
    told(store, who, &admitted.deployment.id)
}

fn told(store: &ConfigStore, who: &Actor, id: &ployz_store::DeploymentId) -> [Vec<String>; 3] {
    let claimed = store.claim(id, &runner()).unwrap();
    let volumes = store
        .read(who, &ployz_store::DeploymentQuery { id: id.clone() })
        .unwrap()
        .nodes
        .into_iter()
        .filter(|node| matches!(node.node, ployz_store::DeployedNode::Volume { .. }))
        .map(|node| node.node.name().to_owned())
        .collect();
    [
        claimed
            .intent
            .options
            .selected
            .iter()
            .map(|attempt| attempt.name.to_string())
            .collect(),
        claimed
            .intent
            .options
            .removals
            .iter()
            .map(ToString::to_string)
            .collect(),
        volumes,
    ]
}

/// [`shop_with_nodes`] where nothing references `db`.
fn shop_with_lone_db() -> (ConfigStore, Actor) {
    shop_copying(
        |store, who| {
            production_nodes(store, who);
            unset(store, who, "production", "web.env.DB_URL");
            save(store, who, "production");
        },
        Some(vec![
            node("db"),
            node("volumes.data"),
            node("configs.sentry"),
        ]),
    )
}

/// Undo what pull request #5's merge landed in production, and save it.
fn undo_merge(store: &ConfigStore, who: &Actor) {
    let landed = history(store, who)
        .into_iter()
        .find(|revision| revision.message.as_deref() == Some("Merged #5"))
        .unwrap();
    let query = HistoryPreviewQuery {
        environment: at("production"),
        revision: landed.revision,
        action: HistoryAction::Undo,
    };
    let preview = store.read(who, &query).unwrap();
    store
        .write(
            who,
            &StageHistory {
                environment: at("production"),
                revision: query.revision,
                action: query.action,
                version: preview.version,
                accept_overwrite: false,
            },
        )
        .unwrap();
    save(store, who, "production");
}

const NOTHING: Vec<String> = Vec::new();

#[test]
fn a_merge_that_removes_a_running_service_tears_it_down_with_its_push() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(landed.admitted.is_empty(), "{landed:?}");
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    // web for the DB_URL that landed, and db's teardown; never Volume data.
    assert_eq!(
        teardown(&store, &who, &pushed),
        [
            vec!["web".to_owned(), "db".to_owned()],
            vec!["db".to_owned()],
            NOTHING
        ]
    );
    // Once.
    let next = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert!(next.admitted.is_empty(), "{next:?}");
}

#[test]
fn a_merge_that_only_removes_a_service_tears_it_down_alone_after_its_push() {
    let (store, who) = shop_with_lone_db();
    running(&store, &who);
    watching(&store, &who);
    remove_service(&store, &who, "pr-5", "db");
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    // A teardown of db alone, never a full Deploy pruning whatever else runs.
    assert_eq!(
        teardown(&store, &who, &landed),
        [vec!["db".to_owned()], vec!["db".to_owned()], NOTHING]
    );
}

#[test]
fn a_removal_waits_for_ci_with_what_the_merge_changed() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
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
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(
        (pushed.waiting.len(), pushed.admitted.len()),
        (1, 0),
        "{pushed:?}"
    );

    let checked = passed(&store, &who, MERGE);
    assert_eq!(
        teardown(&store, &who, &checked),
        [
            vec!["web".to_owned(), "db".to_owned()],
            vec!["db".to_owned()],
            NOTHING
        ]
    );
}

#[test]
fn a_removed_service_that_never_ran_deploys_nothing() {
    let (store, who) = shop_with_lone_db();
    watching(&store, &who);
    remove_service(&store, &who, "pr-5", "db");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(landed.admitted.is_empty(), "{landed:?}");
    assert!(pushed.admitted.is_empty(), "{pushed:?}");
    assert!(pushed.skipped.is_empty(), "{pushed:?}");
}

#[test]
fn undo_then_deploy_brings_a_torn_down_service_back() {
    let (store, who) = shop_with_lone_db();
    running(&store, &who);
    watching(&store, &who);
    remove_service(&store, &who, "pr-5", "db");
    system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    run(&store, &landed.admitted[0].deployment.id, &["db"]);

    undo_merge(&store, &who);
    let id = deploy_by_hand(&store, &who, &["db"]);
    assert_eq!(
        told(&store, &who, &id),
        [vec!["db".to_owned()], NOTHING, vec!["data".to_owned()]]
    );
}

#[test]
fn undo_before_the_push_keeps_the_service_running() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);

    undo_merge(&store, &who);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    // web still deploys for the merge; db is saved again, so nothing tears it down.
    assert_eq!(
        teardown(&store, &who, &pushed),
        [vec!["web".to_owned()], NOTHING, NOTHING]
    );
}

#[test]
fn a_manual_deploy_of_other_services_leaves_the_teardown_to_the_push() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let web = deploy_by_hand(&store, &who, &["web"]);
    run(&store, &web, &["web"]);

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(
        teardown(&store, &who, &pushed),
        [vec!["db".to_owned()], vec!["db".to_owned()], NOTHING]
    );
}

#[test]
fn a_full_deploy_that_cant_see_every_server_leaves_the_teardown_to_the_push() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let full = deploy_by_hand(&store, &who, &[]);
    run_seeing(&store, &full, &["web", "worker"], false);

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(
        teardown(&store, &who, &pushed),
        [vec!["db".to_owned()], vec!["db".to_owned()], NOTHING]
    );
}

#[test]
fn a_full_deploy_over_a_queued_teardown_that_cant_see_every_server_is_not_applied() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(pushed.admitted.len(), 1, "{pushed:?}");
    let full = deploy_by_hand(&store, &who, &[]);
    run_seeing(&store, &full, &["web", "worker"], false);

    assert_ne!(
        status(&store, &who, &full),
        ployz_store::DeploymentStatus::Applied
    );
    retry(&store, &who, &full).unwrap();
}

#[test]
fn a_full_deploy_that_tore_the_service_down_leaves_the_push_nothing() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let full = deploy_by_hand(&store, &who, &[]);
    run(&store, &full, &["web", "worker", "db"]);

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");
}

#[test]
fn a_removal_supersedes_a_queued_deploy_that_would_run_the_service() {
    let (store, who) = shop_with_lone_db();
    watching(&store, &who);
    let queued = deploy_by_hand(&store, &who, &["db"]);
    remove_service(&store, &who, "pr-5", "db");
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(
        teardown(&store, &who, &pushed),
        [vec!["db".to_owned()], vec!["db".to_owned()], NOTHING]
    );
    assert!(store.claim(&queued, &runner()).is_err());
}

#[test]
fn a_removal_tears_down_what_a_deploy_claimed_before_its_push() {
    let (store, who) = shop_with_lone_db();
    watching(&store, &who);
    let queued = deploy_by_hand(&store, &who, &["db"]);
    remove_service(&store, &who, "pr-5", "db");
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    store.claim(&queued, &runner()).unwrap();

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(
        teardown(&store, &who, &pushed),
        [vec!["db".to_owned()], vec!["db".to_owned()], NOTHING]
    );
}

fn cancel(store: &ConfigStore, who: &Actor, id: &ployz_store::DeploymentId) {
    store
        .write(
            who,
            &ployz_store::Cancel {
                deployment: id.clone(),
            },
        )
        .unwrap();
}

fn retry(
    store: &ConfigStore,
    who: &Actor,
    source: &ployz_store::DeploymentId,
) -> Result<ployz_store::DeploymentId, ployz_core::RpcError> {
    let id = ployz_store::DeploymentId::parse(::uuid::Uuid::new_v4().to_string()).unwrap();
    store.write_trusted(
        who,
        &ployz_store::Admit::Retry(ployz_store::Retry {
            id: id.clone(),
            deployment: source.clone(),
        }),
        &Trusted::default(),
    )?;
    Ok(id)
}

#[test]
fn a_retry_takes_over_the_teardown_it_supersedes() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let worker = deploy_by_hand(&store, &who, &["worker"]);
    cancel(&store, &who, &worker);
    system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));

    let retried = retry(&store, &who, &worker).unwrap();
    let [_, removals, _] = told(&store, &who, &retried);
    assert_eq!(removals, vec!["db".to_owned()]);
}

fn status(
    store: &ConfigStore,
    who: &Actor,
    id: &ployz_store::DeploymentId,
) -> ployz_store::DeploymentStatus {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id.clone() })
        .unwrap()
        .deployment
        .status
}

#[test]
fn a_retry_from_before_the_merge_leaves_the_queued_teardown() {
    for target in ["db", "worker"] {
        let (store, who) = shop_with_nodes();
        running(&store, &who);
        watching(&store, &who);
        let earlier = deploy_by_hand(&store, &who, &[target]);
        cancel(&store, &who, &earlier);
        remove_db(&store, &who);
        merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
        let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
        let [queued] = pushed.admitted.as_slice() else {
            panic!("one Deployment: {pushed:?}")
        };

        let refused = retry(&store, &who, &earlier).unwrap_err();
        assert_eq!(
            refused.code,
            RpcErrorCode::Conflict,
            "{target}: {refused:?}"
        );
        assert_eq!(
            status(&store, &who, &queued.deployment.id),
            ployz_store::DeploymentStatus::Queued,
            "{target}"
        );
    }
}

#[test]
fn a_retry_after_undo_never_tears_down_what_saved_holds_again() {
    let (store, who) = shop_with_lone_db();
    running(&store, &who);
    watching(&store, &who);
    remove_service(&store, &who, "pr-5", "db");
    system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let teardown = landed.admitted[0].deployment.id.clone();
    cancel(&store, &who, &teardown);

    undo_merge(&store, &who);
    let refused = retry(&store, &who, &teardown).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
}

#[test]
fn a_merge_that_removes_a_mounted_config_takes_it_out_with_the_service() {
    let (store, who) = shop_with_nodes();
    let full = deploy_by_hand(&store, &who, &[]);
    run(&store, &full, &["web", "worker", "db"]);
    watching(&store, &who);
    store
        .write(
            &who,
            &ployz_store::DeleteConfig {
                environment: at("pr-5"),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
            },
        )
        .unwrap();
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let [admitted] = pushed.admitted.as_slice() else {
        panic!("one Deployment: {pushed:?}")
    };
    let nodes: Vec<String> = store
        .read(
            &who,
            &ployz_store::DeploymentQuery {
                id: admitted.deployment.id.clone(),
            },
        )
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| node.node.name().to_owned())
        .collect();
    assert_eq!(nodes, vec!["web".to_owned(), "sentry".to_owned()]);
}
