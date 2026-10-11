#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Pull requests in a Destination's draft, through the Store's interface, on SQLite
//! and on Postgres (see `backend`): a Merge-menu Sync offers its rows without
//! touching the draft, Include brings them in, and Save and Deploy wait until every
//! pull request the draft includes merged into a branch it deploys. A merge only
//! marks the pull request merged; Remove unblocks the rest of the draft.
//!
//! After each command the suite checks that no offer owns an arrival
//! ([`Db::probe`]): an offer is not in the draft.

use ployz_core::RpcErrorCode;
use ployz_core::config::ServiceGitAccess;
use ployz_store::{
    Actor, Admit, AuthorizedRepository, BranchQuery, Change, Command, ConditionalSyncState,
    ConfigStore, CreateBranch, CreateGitService, CreateProject, Deploy, DeploymentId, DiffQuery,
    DiffView, Discard, DiscardTarget, Edit, EnvironmentId, EnvironmentName, EnvironmentRef,
    IncludeProposal, Included, OrganizationId, ProjectId, ProjectName, ProposalId,
    ProposalIncluded, Publish, PullRequest, PullRequestQuery, Readiness, RemoveProposal, RowId,
    SecretRow, ServiceLineageId, SetPrPlan, SettingPath, SyncChanges, SyncQuery, SyncView,
    SyncedWhen, SystemEvent, Trusted, UndoSync, When, Written,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod backend;

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

/// Items as their text, to compare with literals.
fn names<T: ToString>(items: &[T]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

/// A Store on a database this suite can also read directly.
struct Db {
    store: ConfigStore,
    who: Actor,
    url: String,
    _dir: tempfile::TempDir,
}

impl Db {
    /// Each row of `sql`, which selects one text column.
    fn rows(&self, sql: &str) -> Vec<String> {
        match self.url.strip_prefix("sqlite:") {
            Some(path) => {
                let connection = rusqlite::Connection::open(path).unwrap();
                let mut statement = connection.prepare(sql).unwrap();
                statement
                    .query_map([], |row| row.get::<_, String>(0))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect()
            }
            None => postgres::Client::connect(&self.url, postgres::NoTls)
                .unwrap()
                .query(sql, &[])
                .unwrap()
                .iter()
                .map(|row| row.get::<_, String>(0))
                .collect(),
        }
    }

    /// No offer owns an arrival: an offer is not in its draft.
    fn probe(&self) {
        let owned = self.rows(
            "SELECT a.lineage FROM config_sync_arrival a \
             JOIN config_proposal p ON p.id = a.proposal_id WHERE p.offered IS NOT NULL",
        );
        assert!(owned.is_empty(), "an offer owns arrivals: {owned:?}");
    }

    /// Production's Working State and revision, Saved revisions, Deployments,
    /// proposals and arrivals, as text.
    fn production(&self) -> Vec<String> {
        let production = uuid(2);
        let mut state = self.rows(&format!(
            "SELECT CAST(working_revision AS TEXT) || ' ' || working FROM config_environment \
             WHERE id = '{production}'"
        ));
        for table in ["config_saved", "config_deployment"] {
            state.extend(self.rows(&format!(
                "SELECT CAST(COUNT(*) AS TEXT) FROM {table} WHERE environment_id = '{production}'"
            )));
        }
        let mut proposals = self.rows(&format!(
            "SELECT id || ' ' || CAST(source_revision AS TEXT) || ' ' || \
             COALESCE(offered, '-') || ' ' || first_sync || ' ' || last_sync \
             FROM config_proposal WHERE environment_id = '{production}'"
        ));
        let mut arrivals = self.rows(&format!(
            "SELECT lineage || ' ' || at || ' ' || COALESCE(proposal_id, '-') || ' ' || \
             COALESCE(sync_id, '-') || ' ' || value \
             FROM config_sync_arrival WHERE environment_id = '{production}'"
        ));
        proposals.sort();
        arrivals.sort();
        state.extend(proposals);
        state.extend(arrivals);
        state
    }
}

/// Project `shop`: `production` runs Git Services `web` and `api` from `acme/web`'s
/// `main`, published; pull requests get PR Environments of it, and PR #5 is open.
fn shop() -> Db {
    shop_from("production")
}

/// [`shop`], its PR Environments Branches of `start_from`: a kept Branch of
/// production with its own `web`, unless it is production.
fn shop_from(start_from: &str) -> Db {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
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
            branches: vec![backend::git_branch("dev")],
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
    let db = Db {
        store,
        who,
        url,
        _dir: dir,
    };
    publish(&db, "production", None).unwrap();
    if start_from != "production" {
        branch(&db, start_from);
    }
    db.store
        .write(
            &db.who,
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
    let opened = observe(&db, facts(true, None, "main", 1));
    assert_eq!(opened.admitted.len(), 1);
    db
}

/// A kept Branch `name` of production with its own `web`.
fn branch(db: &Db, name: &str) {
    db.store
        .write(
            &db.who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(20)).unwrap(),
                from: at("production"),
                name: EnvironmentName::parse(name).unwrap(),
                copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                live: Vec::new(),
                setup: Vec::new(),
                keep: true,
                fix: None,
            },
        )
        .unwrap();
}

fn publish(
    db: &Db,
    environment: &str,
    version: Option<String>,
) -> Result<(), ployz_core::RpcError> {
    db.store
        .write(
            &db.who,
            &Command::Publish(Publish {
                environment: at(environment),
                version,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .map(drop)
}

/// Admit a full Deploy of production as Deployment `n`.
fn deploy(db: &Db, n: u8, version: Option<String>) -> Result<DeploymentId, ployz_core::RpcError> {
    let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000002{n:02}")).unwrap();
    db.store
        .write_trusted(
            &db.who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: at("production"),
                services: Vec::new(),
                version,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .map(|_| id)
}

fn set(db: &Db, environment: &str, changes: &[(&str, Value)]) {
    db.store
        .write(
            &db.who,
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
    db.probe();
}

/// PR #5's facts at second `second`: merged into `target` when `merge` names its commit.
fn facts(open: bool, merge: Option<&str>, target: &str, second: u8) -> PullRequest {
    PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: backend::git_branch("search"),
        head: commit(1),
        target_branch: backend::git_branch(target),
        commits: 1,
        open,
        merge_commit: merge.map(backend::sha),
        updated: ployz_store::GithubTimestamp::parse(format!("2026-09-29T10:00:{second:02}Z"))
            .unwrap(),
    }
}

/// PR #5 merged into `main` at second 2.
fn merged() -> PullRequest {
    facts(false, Some(MERGE), "main", 2)
}

fn observe(db: &Db, facts: PullRequest) -> ployz_store::Automated {
    let Written::Automated(automated) = db
        .store
        .system(
            &db.who.organization,
            &SystemEvent::PullRequest(facts),
            &Trusted::default(),
        )
        .unwrap()
    else {
        panic!("a system event writes Automated")
    };
    db.probe();
    automated
}

/// What a Sync from `pr-5` carries into `into`, omitted its only Destination.
fn offered(db: &Db, into: Option<&str>) -> SyncView {
    read_sync(db, into, None)
}

/// What a Sync from `pr-5` into `into` carries now.
fn offered_now(db: &Db, into: &str) -> SyncView {
    read_sync(db, Some(into), Some(When::Now { close_after: false }))
}

fn read_sync(db: &Db, into: Option<&str>, when: Option<When>) -> SyncView {
    db.store
        .read(
            &db.who,
            &SyncQuery {
                from: at("pr-5"),
                into: into.map(at),
                when,
            },
        )
        .unwrap()
}

/// Sync every row `review` ticks, as reviewed, at the merge if `review` is.
fn sync(review: &SyncView) -> SyncChanges {
    SyncChanges {
        from: at(review.from.name.as_str()),
        into: Some(at(review.into.name.as_str())),
        when: Some(match review.at_merge {
            Some(_) => When::AtMerge,
            None => When::Now { close_after: false },
        }),
        version: review.version.clone(),
        picks: None,
        skip: Vec::new(),
        values: BTreeMap::new(),
        id: None,
    }
}

/// Write `sync`; what it did.
fn synced(db: &Db, sync: &SyncChanges) -> ployz_store::Synced {
    let Written::Synced(synced) = db
        .store
        .write(&db.who, &Command::Sync(sync.clone()))
        .unwrap()
    else {
        panic!("a Sync writes Synced")
    };
    db.probe();
    synced
}

/// Offer pr-5's ticked rows to production; the offer.
fn offer(db: &Db) -> ProposalId {
    let review = offered(db, None);
    assert_eq!(review.at_merge, Some(backend::pr_number(5)));
    proposal_of(&synced(db, &sync(&review)))
}

fn proposal_of(synced: &ployz_store::Synced) -> ProposalId {
    match &synced.when {
        SyncedWhen::AtMerge { conditional_sync } => {
            ProposalId::parse(conditional_sync.id.as_str()).unwrap()
        }
        SyncedWhen::Now { proposal, .. } => proposal.clone(),
    }
}

/// The row of `web`'s variable `key`.
fn row(key: &str) -> RowId {
    format!("{}:variables.{key}", uuid(3)).parse().unwrap()
}

fn diff(db: &Db) -> DiffView {
    db.store
        .read(
            &db.who,
            &DiffQuery {
                environment: at("production"),
            },
        )
        .unwrap()
}

/// What production's draft includes or is offered.
fn included(db: &Db) -> Vec<Included> {
    diff(db).included
}

/// Include `proposal` in production's draft, giving `values` for its secrets.
fn include(
    db: &Db,
    proposal: &ProposalId,
    values: &[(&str, &str)],
) -> Result<ProposalIncluded, ployz_core::RpcError> {
    let request = IncludeProposal {
        environment: at("production"),
        proposal: proposal.clone(),
        version: diff(db).version,
        values: values
            .iter()
            .map(|(key, value)| (row(key).into(), (*value).to_owned()))
            .collect(),
    };
    let written = db.store.write(&db.who, &request);
    db.probe();
    written
}

fn remove(db: &Db, proposal: &ProposalId, version: Option<String>) -> ployz_store::Removed {
    let removed = db
        .store
        .write(
            &db.who,
            &RemoveProposal {
                environment: at("production"),
                proposal: proposal.clone(),
                version,
            },
        )
        .unwrap();
    db.probe();
    removed
}

fn env(db: &Db, environment: &str) -> Value {
    db.store
        .read(
            &db.who,
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

fn check(db: &Db) -> (bool, String) {
    let view = db
        .store
        .read(
            &db.who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap();
    (view.passing, view.reason)
}

/// What Deployment `id` resolves `web`'s `key` to at claim.
fn resolved(db: &Db, id: &DeploymentId, key: &str) -> Value {
    let input = backend::run(&db.store, id);
    input["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|snapshot| snapshot["serviceId"] == uuid(3))
        .unwrap()["resolvedEnv"][key]
        .clone()
}

/// A push of commit `n` to `main`.
fn push(db: &Db, n: u8) -> ployz_store::Automated {
    let base = db
        .store
        .branch_head(
            &db.who.organization,
            backend::repo_id(11),
            &backend::git_branch("main"),
        )
        .unwrap();
    let Written::Automated(pushed) = db
        .store
        .system(
            &db.who.organization,
            &SystemEvent::BranchHead(ployz_store::BranchHead {
                repository_id: backend::repo_id(11),
                branch: backend::git_branch("main"),
                base,
                head: Some(commit(n)),
                changed: None,
            }),
            &Trusted::default(),
        )
        .unwrap()
    else {
        panic!("a system event writes Automated")
    };
    db.probe();
    pushed
}

/// pr-5 sets MODE and secret TOKEN, offered to production with production's value of it.
fn offer_mode_and_token(db: &Db) -> ProposalId {
    set(
        db,
        "pr-5",
        &[
            ("web.env.MODE", json!("fast")),
            ("web.env.TOKEN", json!({ "secret": "pr-secret" })),
        ],
    );
    let review = offered(db, None);
    let mut request = sync(&review);
    request
        .values
        .insert(row("TOKEN").into(), "prod-secret".into());
    proposal_of(&synced(db, &request))
}

#[test]
fn a_merge_menu_sync_offers_its_rows_without_touching_the_draft() {
    let db = shop();
    set(
        &db,
        "pr-5",
        &[
            ("web.env.MODE", json!("fast")),
            ("web.env.TOKEN", json!({ "secret": "pr-secret" })),
        ],
    );
    let review = offered(&db, None);
    assert_eq!(
        (review.into.name.as_str(), review.at_merge),
        ("production", Some(backend::pr_number(5)))
    );
    let secret = |label: &str| {
        let row = review.rows.iter().find(|row| row.at.to_string() == label);
        row.unwrap().secret.clone()
    };
    assert_eq!(secret("web.env.MODE"), None);
    assert_eq!(secret("web.env.TOKEN"), Some(SecretRow {}));
    let (before, version) = (db.production(), diff(&db).version);
    let mut request = sync(&review);
    request
        .values
        .insert(row("TOKEN").into(), "prod-secret".into());
    let synced = synced(&db, &request);
    let SyncedWhen::AtMerge { conditional_sync } = &synced.when else {
        panic!("a Merge-menu Sync offers")
    };
    assert_eq!(conditional_sync.state, ConditionalSyncState::Standing);
    assert_eq!(
        names(&conditional_sync.rows),
        ["web.env.MODE", "web.env.TOKEN"]
    );
    // Only the offer is new: the draft, its version and its History are as they were.
    let after = db.production();
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(after[..3], before[..3]);
    assert_eq!(diff(&db).version, version);
    assert!(env(&db, "production").get("MODE").is_none());
    let offers = included(&db);
    assert_eq!(offers.len(), 1);
    assert_eq!(
        (offers[0].offered, offers[0].readiness, offers[0].changes),
        (true, Some(Readiness::Open), 2)
    );
    assert_eq!(offers[0].sync, synced.sync);
    assert_eq!(check(&db), (true, "2 changes go live with this PR".into()));
}

#[test]
fn syncing_again_replaces_the_offer() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let first = synced(&db, &sync(&offered(&db, None)));
    set(&db, "pr-5", &[("web.env.MODE", json!("slow"))]);
    assert_eq!(
        check(&db),
        (false, "Changed since synced · sync again".into())
    );
    let second = synced(&db, &sync(&offered(&db, None)));
    let offers = included(&db);
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].proposal, proposal_of(&second));
    // The first Sync's offer is gone: there is nothing of it to undo.
    assert_eq!(
        db.store
            .write(&db.who, &UndoSync { sync: first.sync })
            .unwrap_err()
            .code,
        RpcErrorCode::NotFound
    );
    include(&db, &proposal_of(&second), &[]).unwrap();
    assert_eq!(env(&db, "production")["MODE"], json!("slow"));
}

#[test]
fn undoing_the_sync_forgets_the_offer() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let synced = synced(&db, &sync(&offered(&db, None)));
    let before = diff(&db).version;
    let undo = UndoSync { sync: synced.sync };
    let undone = db.store.write(&db.who, &undo).unwrap();
    assert_eq!(undone.into.name.as_str(), "production");
    assert!(included(&db).is_empty());
    assert_eq!(diff(&db).version, before);
    assert_eq!(
        db.store.write(&db.who, &undo).unwrap_err().code,
        RpcErrorCode::NotFound
    );
    assert_eq!(check(&db), (false, "1 change to sync in Ployz".into()));
}

#[test]
fn removing_an_offer_changes_nothing_in_the_draft() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    let version = diff(&db).version;
    assert!(remove(&db, &proposal, None).removed);
    assert!(included(&db).is_empty());
    assert_eq!(diff(&db).version, version);
    assert!(!remove(&db, &proposal, None).removed);
}

/// Include lands what the Sync stored, with the value given for its secret, and
/// the draft then waits for the merge to save it.
#[test]
fn including_an_offer_lands_what_the_sync_stored() {
    let db = shop();
    let proposal = offer_mode_and_token(&db);
    let version = diff(&db).version;
    let included_now = include(&db, &proposal, &[]).unwrap();
    assert!(included_now.included);
    assert_eq!(names(&included_now.staged), ["web"]);
    assert!(included_now.kept.is_empty());
    assert_eq!(env(&db, "production")["MODE"], json!("fast"));
    // The draft changed, and it now includes PR #5.
    let review = diff(&db);
    assert_ne!(review.version, version);
    assert!(review.version.contains(":g"), "{}", review.version);
    let listed = &review.included;
    assert_eq!(
        (listed.len(), listed[0].offered, listed[0].readiness),
        (1, false, Some(Readiness::Open))
    );
    // Merged into main, which production deploys: the draft saves, and deploys with
    // production's value of TOKEN; the pull request's never travels.
    observe(&db, merged());
    assert_eq!(included(&db)[0].readiness, Some(Readiness::Ready));
    publish(&db, "production", Some(diff(&db).version)).unwrap();
    db.probe();
    assert!(included(&db).is_empty());
    let id = deploy(&db, 1, None).unwrap();
    assert_eq!(resolved(&db, &id, "TOKEN"), json!("prod-secret"));
    assert_eq!(resolved_mode(&db), json!("fast"));
}

fn resolved_mode(db: &Db) -> Value {
    env(db, "production")["MODE"].clone()
}

#[test]
fn an_offer_is_included_after_its_pr_environment_is_gone() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("pr"))]);
    let proposal = offer(&db);
    let closed = observe(&db, merged());
    assert_eq!(closed.removed.len(), 1, "{closed:?}");
    let offers = included(&db);
    assert_eq!(
        (offers.len(), offers[0].offered, offers[0].readiness),
        (1, true, Some(Readiness::Ready))
    );
    let included_now = include(&db, &proposal, &[]).unwrap();
    assert_eq!(names(&included_now.staged), ["web"]);
    assert_eq!(env(&db, "production")["MODE"], json!("pr"));
    publish(&db, "production", Some(diff(&db).version)).unwrap();
    assert_eq!(env(&db, "production")["MODE"], json!("pr"));
}

#[test]
fn a_retried_include_answers_that_it_included_nothing() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    let first = include(&db, &proposal, &[]).unwrap();
    let state = db.production();
    let again = include(&db, &proposal, &[]).unwrap();
    assert_eq!((first.included, again.included), (true, false));
    assert_eq!(again.sync, first.sync);
    assert_eq!(db.production(), state);
}

/// production changed MODE itself after the Sync was reviewed: the row stays as
/// production has it, and with nothing else to move the Include is refused.
#[test]
fn an_include_where_nothing_still_moves_is_refused_and_keeps_the_offer() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("pr"))]);
    let proposal = offer(&db);
    set(&db, "production", &[("web.env.MODE", json!("own"))]);
    let refused = include(&db, &proposal, &[]).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert!(refused.message.contains("Remove it"), "{}", refused.message);
    assert_eq!(env(&db, "production")["MODE"], json!("own"));
    let offers = included(&db);
    assert_eq!((offers.len(), offers[0].offered), (1, true));
}

#[test]
fn an_include_is_refused_with_a_stale_version() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    let stale = diff(&db).version;
    set(&db, "production", &[("web.env.OTHER", json!("1"))]);
    let refused = db
        .store
        .write(
            &db.who,
            &IncludeProposal {
                environment: at("production"),
                proposal,
                version: stale,
                values: BTreeMap::new(),
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert!(included(&db)[0].offered);
}

#[test]
fn including_a_secret_the_draft_lacks_needs_its_value() {
    let db = shop();
    set(
        &db,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "pr-secret" }))],
    );
    let proposal = offer(&db);
    let refused = include(&db, &proposal, &[]).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(refused.details["needs_value"], json!(["web.env.TOKEN"]));
    assert!(included(&db)[0].offered);
    include(&db, &proposal, &[("TOKEN", "given")]).unwrap();
    observe(&db, merged());
    publish(&db, "production", Some(diff(&db).version)).unwrap();
    let id = deploy(&db, 1, None).unwrap();
    assert_eq!(resolved(&db, &id, "TOKEN"), json!("given"));
}

/// Once PR #5 is in the draft, a Merge-menu Sync refreshes it there now.
#[test]
fn a_merge_menu_sync_refreshes_an_included_pr_now() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    set(&db, "pr-5", &[("web.env.MODE", json!("slow"))]);
    let review = offered(&db, None);
    assert_eq!(review.at_merge, None);
    let synced = synced(&db, &sync(&review));
    assert!(matches!(synced.when, SyncedWhen::Now { .. }));
    assert_eq!(proposal_of(&synced), proposal);
    assert_eq!(env(&db, "production")["MODE"], json!("slow"));
    let listed = included(&db);
    assert_eq!((listed.len(), listed[0].offered), (1, false));
}

/// A Sync now from pr-5 into production, which #5 hasn't merged into, is offered
/// as a Merge-menu Sync is, replacing the offer: the draft, its version and its
/// History are as they were. It can't close pr-5.
#[test]
fn a_sync_now_into_a_destination_is_offered_while_the_pr_is_unmerged() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let offered_first = offer(&db);
    set(&db, "pr-5", &[("web.env.MODE", json!("slow"))]);
    let (before, version) = (db.production(), diff(&db).version);
    let review = offered_now(&db, "production");
    assert_eq!(review.at_merge, Some(backend::pr_number(5)));
    let mut request = sync(&review);
    request.when = Some(When::Now { close_after: false });
    let synced = synced(&db, &request);
    let SyncedWhen::AtMerge { conditional_sync } = &synced.when else {
        panic!("a Sync now of an unmerged PR into its Destination offers")
    };
    assert_eq!(names(&conditional_sync.rows), ["web.env.MODE"]);
    let after = db.production();
    assert_eq!(after.len(), before.len());
    assert_eq!(after[..3], before[..3]);
    assert_eq!(after[4..], before[4..]);
    assert_eq!(diff(&db).version, version);
    assert!(env(&db, "production").get("MODE").is_none());
    let listed = included(&db);
    assert_eq!((listed.len(), listed[0].offered), (1, true));
    assert_ne!(listed[0].proposal, offered_first);
    assert_eq!(listed[0].proposal, proposal_of(&synced));

    request.when = Some(When::Now { close_after: true });
    let refused = db
        .store
        .write(&db.who, &Command::Sync(request))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        refused.message,
        "#5 isn't merged: its Sync is queued, so sync without close_after"
    );
    assert_eq!(db.production(), after);
}

/// Once #5 merged into what production deploys, a Sync now from pr-5, kept open by
/// its plan, lands in the draft directly, which Save then takes.
#[test]
fn a_sync_now_of_a_merged_pr_lands_in_the_draft() {
    let db = shop();
    db.store
        .write(
            &db.who,
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
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let offered_first = offer(&db);
    observe(&db, merged());
    let review = offered_now(&db, "production");
    assert_eq!(review.at_merge, None);
    let synced = synced(&db, &sync(&review));
    assert!(matches!(synced.when, SyncedWhen::Now { .. }));
    assert_eq!(env(&db, "production")["MODE"], json!("fast"));
    let listed = included(&db);
    assert_eq!(
        (listed.len(), listed[0].offered, listed[0].readiness),
        (1, false, Some(Readiness::Ready))
    );
    assert_ne!(listed[0].proposal, offered_first);
    publish(&db, "production", Some(diff(&db).version)).unwrap();
    assert!(included(&db).is_empty());
}

/// production stages its own OTHER beside PR #5's MODE: while #5 is unmerged, Save
/// and Deploy refuse the whole draft, naming it, and write nothing.
#[test]
fn save_and_deploy_refuse_the_whole_draft_while_the_pr_is_unmerged() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    set(&db, "production", &[("web.env.OTHER", json!("1"))]);
    let version = diff(&db).version;
    let before = db.production();
    let refused = publish(&db, "production", Some(version.clone())).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "#5 isn't merged: Remove it to save the rest."
    );
    assert_eq!(refused.details["waits_on"], json!(5));
    assert_eq!(refused.details["proposal"], json!(proposal));
    let refused = deploy(&db, 1, Some(version.clone())).unwrap_err();
    assert_eq!(refused.details["waits_on"], json!(5));
    assert_eq!(db.production(), before);
    assert_eq!(diff(&db).version, version);
}

/// Remove takes PR #5's MODE out and leaves production's own OTHER, which then saves.
#[test]
fn removing_the_pr_unblocks_the_rest_of_the_draft() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    set(&db, "production", &[("web.env.OTHER", json!("1"))]);
    // A stale version is refused; none is not: Remove is what unblocks.
    let stale = diff(&db).version;
    set(&db, "production", &[("web.env.MORE", json!("2"))]);
    let refused = db
        .store
        .write(
            &db.who,
            &RemoveProposal {
                environment: at("production"),
                proposal: proposal.clone(),
                version: Some(stale),
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert!(remove(&db, &proposal, None).removed);
    let review = diff(&db);
    assert!(!review.version.contains(":g"), "{}", review.version);
    assert!(review.included.is_empty());
    let production = env(&db, "production");
    assert!(production.get("MODE").is_none());
    assert_eq!(production["OTHER"], json!("1"));
    let saved = db.production()[1].clone();
    publish(&db, "production", Some(review.version)).unwrap();
    assert_ne!(db.production()[1], saved);
}

/// Remove reviewed before PR #5 merged still takes it out: only the content the
/// review showed must be current, not whether the pull request merged.
#[test]
fn a_merge_since_the_review_does_not_refuse_its_remove() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    let reviewed = diff(&db).version;
    observe(&db, merged());
    assert_ne!(diff(&db).version, reviewed);
    assert!(remove(&db, &proposal, Some(reviewed)).removed);
    assert!(included(&db).is_empty());
    assert!(env(&db, "production").get("MODE").is_none());
}

/// Removing production consumes no draft: it needs no version while its draft
/// includes the unmerged PR #5, and a stale one is still refused.
#[test]
fn an_environment_including_a_pr_is_removed_without_a_version() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    assert_eq!(included(&db)[0].readiness, Some(Readiness::Open));
    let stale = diff(&db).version;
    set(&db, "production", &[("web.env.OTHER", json!("1"))]);
    // With no Server left a removal applies at once, after the same version check.
    let removal = |environment: &str, n: u8, version: Option<String>| {
        db.store.write_trusted(
            &db.who,
            &Admit::Remove(ployz_store::Removal {
                id: DeploymentId::parse(uuid(n)).unwrap(),
                environment: at(environment),
                version,
                accept_volume_loss: Vec::new(),
                close: false,
            }),
            &Trusted {
                servers: Some(0),
                ..Trusted::default()
            },
        )
    };
    // production's Branch pr-5 must be off the Servers first.
    removal("pr-5", 30, None).unwrap();
    let refused = removal("production", 31, Some(stale)).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert!(
        refused
            .message
            .starts_with("The Environment changed after this review"),
        "{}",
        refused.message
    );
    let removed = removal("production", 31, None).unwrap();
    assert_eq!(removed.status, ployz_store::DeploymentStatus::Applied);
}

#[test]
fn a_merge_after_review_makes_that_review_stale() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    let reviewed = diff(&db).version;
    observe(&db, merged());
    let refused = publish(&db, "production", Some(reviewed.clone())).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert!(
        refused
            .message
            .starts_with("The Environment changed after this review"),
        "{}",
        refused.message
    );
    // A version naming the review with a gate of its own is still a stale one.
    let current = diff(&db).version;
    assert_ne!(current, reviewed);
    let (base, _) = current.rsplit_once(":g").unwrap();
    assert_eq!(
        publish(&db, "production", Some(format!("{base}:g00000000")))
            .unwrap_err()
            .code,
        RpcErrorCode::Conflict
    );
    publish(&db, "production", Some(current)).unwrap();
    assert!(included(&db).is_empty());
    assert_eq!(env(&db, "production")["MODE"], json!("fast"));
}

#[test]
fn no_version_is_refused_only_while_a_pr_is_included() {
    let db = shop();
    set(&db, "production", &[("web.env.OWN", json!("1"))]);
    publish(&db, "production", None).unwrap();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    // An offer isn't in the draft: nothing to review.
    set(&db, "production", &[("web.env.OWN", json!("2"))]);
    publish(&db, "production", None).unwrap();
    include(&db, &proposal, &[]).unwrap();
    observe(&db, merged());
    // Even merged, whether it did must have been reviewed.
    for refused in [
        publish(&db, "production", None).unwrap_err(),
        deploy(&db, 1, None).unwrap_err(),
    ] {
        assert_eq!(refused.code, RpcErrorCode::Conflict);
        assert!(
            refused
                .message
                .starts_with("This draft includes a pull request"),
            "{}",
            refused.message
        );
    }
    deploy(&db, 1, Some(diff(&db).version)).unwrap();
    assert!(included(&db).is_empty());
}

#[test]
fn readiness_follows_whether_and_where_the_pr_merged() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    let readiness = |db: &Db| included(db)[0].readiness;
    let merged_into = |db: &Db| {
        included(db)[0]
            .merged_into
            .as_ref()
            .map(ToString::to_string)
    };
    assert_eq!(readiness(&db), Some(Readiness::Open));
    assert_eq!(merged_into(&db), None);
    observe(&db, facts(false, None, "main", 2));
    assert_eq!(readiness(&db), Some(Readiness::Closed));
    let refused = publish(&db, "production", Some(diff(&db).version)).unwrap_err();
    assert_eq!(
        refused.message,
        "#5 closed unmerged: Remove it to save the rest."
    );
    observe(&db, facts(true, None, "main", 3));
    assert_eq!(readiness(&db), Some(Readiness::Open));
    // Retargeted to dev, which production doesn't deploy, and merged there.
    observe(&db, facts(false, Some(MERGE), "dev", 4));
    assert_eq!(readiness(&db), Some(Readiness::Elsewhere));
    assert_eq!(merged_into(&db).as_deref(), Some("dev"));
    let refused = publish(&db, "production", Some(diff(&db).version)).unwrap_err();
    assert_eq!(
        refused.message,
        "#5 merged elsewhere: Remove it to save the rest."
    );
    assert!(remove(&db, &proposal, Some(diff(&db).version)).removed);
    publish(&db, "production", Some(diff(&db).version)).unwrap();
}

#[test]
fn merged_stays_for_good_whatever_order_the_facts_come_in() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    observe(&db, merged());
    let version = diff(&db).version;
    assert_eq!(included(&db)[0].readiness, Some(Readiness::Ready));
    // An older open event, a duplicate of the merge, an open one as recent as it,
    // and a merge into another branch: merged into main it stays.
    for facts in [
        facts(true, None, "main", 1),
        merged(),
        facts(true, None, "main", 2),
        facts(false, Some(MERGE), "dev", 3),
        facts(false, None, "main", 4),
    ] {
        observe(&db, facts);
        assert_eq!(included(&db)[0].readiness, Some(Readiness::Ready));
        assert_eq!(diff(&db).version, version);
    }
    publish(&db, "production", Some(version)).unwrap();
}

/// A merge marks the pull request merged: production's draft, History, Deployments,
/// proposals and arrivals stay as they were.
#[test]
fn a_merge_writes_nothing_else() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    let before = db.production();
    let pending = db
        .store
        .pending_syncs(
            &db.who.organization,
            backend::repo_id(11),
            &backend::git_branch("main"),
        )
        .unwrap();
    assert_eq!(pending.standing, [backend::pr_number(5)]);
    let closed = observe(&db, merged());
    assert!(closed.admitted.is_empty(), "{closed:?}");
    assert_eq!(db.production(), before);
    let pending = db
        .store
        .pending_syncs(
            &db.who.organization,
            backend::repo_id(11),
            &backend::git_branch("main"),
        )
        .unwrap();
    assert!(pending.standing.is_empty());
}

/// A push to main deploys production's Saved State; the draft keeps PR #5.
#[test]
fn a_push_deploys_saved_state_and_keeps_the_draft() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    let before = db.production();
    let pushed = push(&db, 4);
    db.probe();
    assert_eq!(pushed.admitted.len(), 1);
    let after = db.production();
    // One more Deployment; nothing else of production changed.
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1], before[1]);
    assert_ne!(after[2], before[2]);
    assert_eq!(after[3..], before[3..]);
    assert_eq!(
        resolved(&db, &pushed.admitted[0].deployment.id, "MODE"),
        Value::Null
    );
    assert_eq!(included(&db)[0].proposal, proposal);
}

#[test]
fn discarding_the_whole_draft_keeps_offers() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    set(&db, "production", &[("web.env.OWN", json!("1"))]);
    let discard = |db: &Db| {
        db.store
            .write(
                &db.who,
                &Discard {
                    environment: at("production"),
                    target: DiscardTarget::Saved,
                    path: None,
                    version: Some(diff(db).version),
                },
            )
            .unwrap();
        db.probe();
    };
    discard(&db);
    assert!(env(&db, "production").get("OWN").is_none());
    let listed = included(&db);
    assert_eq!((listed.len(), listed[0].offered), (1, true));
    // Included, a whole Discard ends it with the rest of the draft.
    include(&db, &proposal, &[]).unwrap();
    discard(&db);
    assert!(env(&db, "production").get("MODE").is_none());
    assert!(included(&db).is_empty());
}

#[test]
fn a_pr_environment_syncs_into_an_environment_its_merge_doesnt_reach_now() {
    let db = shop();
    // staging is made from production, which deploys main: it isn't a Destination.
    branch(&db, "staging");
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let review = offered(&db, Some("staging"));
    assert_eq!(review.at_merge, None);
    let synced = synced(&db, &sync(&review));
    let SyncedWhen::Now { staged, .. } = synced.when else {
        panic!("a Sync into staging stages now")
    };
    assert_eq!(names(&staged), ["web"]);
    assert_eq!(env(&db, "staging")["MODE"], json!("fast"));
    // It is still waiting to go to production.
    assert_eq!(check(&db), (false, "1 change to sync in Ployz".into()));
}

#[test]
fn a_follow_into_the_pr_environment_leaves_its_offer_standing() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    offer(&db);
    // production deploys a change of its own, which follows into pr-5.
    set(&db, "production", &[("web.env.SHARED", json!("1"))]);
    publish(&db, "production", None).unwrap();
    let pushed = push(&db, 4);
    backend::run(&db.store, &pushed.admitted[0].deployment.id);
    db.probe();
    assert_eq!(env(&db, "pr-5")["SHARED"], json!("1"));
    assert_eq!(check(&db), (true, "1 change goes live with this PR".into()));
    // The author's own edit makes it stale.
    set(&db, "pr-5", &[("web.env.MODE", json!("slow"))]);
    assert_eq!(
        check(&db),
        (false, "Changed since synced · sync again".into())
    );
}

/// GitHub never reports an open pull request with a merge commit: the Store refuses
/// those facts and the pull request stays unmerged.
#[test]
fn an_open_pull_request_with_a_merge_commit_is_refused() {
    let db = shop();
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    let refused = db
        .store
        .system(
            &db.who.organization,
            &SystemEvent::PullRequest(facts(true, Some(MERGE), "main", 2)),
            &Trusted::default(),
        )
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(included(&db)[0].readiness, Some(Readiness::Open));
}

/// A Config pr-5 adds, its file and its mount, offered and included like any row.
#[test]
fn including_an_offer_lands_a_config_its_file_and_its_mount() {
    let db = shop();
    let name = ployz_core::ConfigName::parse("sentry").unwrap();
    let file = ployz_core::ConfigFileName::parse("a.yml").unwrap();
    db.store
        .write(
            &db.who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(10)).unwrap(),
                environment: at("pr-5"),
                name: name.clone(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ployz_core::ServiceName::parse("web").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    db.store
        .write(
            &db.who,
            &ployz_store::PutConfigFile {
                environment: at("pr-5"),
                config: name.clone(),
                file: file.clone(),
                content: "dsn: one\n".into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
    db.probe();
    let proposal = offer(&db);
    let config = |db: &Db| {
        db.store.read(
            &db.who,
            &ployz_store::ConfigItemQuery {
                environment: at("production"),
                config: name.clone().into(),
            },
        )
    };
    assert!(config(&db).is_err(), "an offer reached the draft");
    include(&db, &proposal, &[]).unwrap();
    let included = config(&db).unwrap();
    assert_eq!(included.contents[&file], "dsn: one\n");
    assert_eq!(
        included.config.mounts,
        [ployz_store::ConfigMountAt {
            service: ployz_core::ServiceName::parse("web").unwrap(),
            dir: "/etc/sentry".into(),
        }]
    );
}

/// pr-5 is a Branch of staging, and production includes its offer: staging deploys
/// a change that follows into pr-5. production's draft lists PR #5 newer, and its
/// version stays, so a review of it is still current.
#[test]
fn a_follow_into_the_pr_environment_marks_its_included_proposal_newer() {
    let db = shop_from("staging");
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let proposal = offer(&db);
    include(&db, &proposal, &[]).unwrap();
    let review = diff(&db);
    assert!(!review.included[0].newer);
    set(&db, "staging", &[("web.env.SHARED", json!("1"))]);
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000250").unwrap();
    db.store
        .write_trusted(
            &db.who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: at("staging"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    backend::run(&db.store, &id);
    db.probe();
    assert_eq!(env(&db, "pr-5")["SHARED"], json!("1"));
    let after = diff(&db);
    assert_eq!(after.version, review.version);
    assert_eq!(
        (after.included[0].proposal.clone(), after.included[0].newer),
        (proposal, true)
    );
}

#[test]
fn a_pr_environment_syncs_into_its_parent_now_and_into_its_destination_at_the_merge() {
    // pr-5 is a Branch of staging, which the merge doesn't reach.
    let db = shop_from("staging");
    set(&db, "pr-5", &[("web.env.MODE", json!("fast"))]);
    let to_parent = db
        .store
        .read(
            &db.who,
            &BranchQuery {
                environment: at("pr-5"),
            },
        )
        .unwrap()
        .to_parent;
    assert_eq!(to_parent, 1);
    let into_parent = offered(&db, Some("staging"));
    assert_eq!(into_parent.at_merge, None);
    let synced = synced(&db, &sync(&into_parent));
    assert!(matches!(synced.when, SyncedWhen::Now { .. }));
    assert_eq!(env(&db, "staging")["MODE"], json!("fast"));
    // Unnamed, the Sync is into production, at the merge.
    let review = offered(&db, None);
    assert_eq!(
        (review.into.name.as_str(), review.at_merge),
        ("production", Some(backend::pr_number(5)))
    );
}
