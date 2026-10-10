#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Proposals through the Store's interface, with the rows they store read back from
//! the database itself: what an Include owns, what a local edit releases, what
//! Remove takes back, and when a draft's proposals end. On SQLite, and on Postgres
//! when `PLOYZ_STORE_TEST_POSTGRES` names a server (see `backend`).

use std::collections::BTreeMap;

use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, ConfigStore, CreateBranch, CreateProject, CreateService, Edit, EnvironmentId,
    EnvironmentName, EnvironmentRef, OrganizationId, ProjectId, ProjectName, ProposalId,
    RemoveProposal, ServiceLineageId, ServiceQuery, SettingPath, SyncChanges, SyncId, SyncQuery,
    SyncView, Synced, SyncedWhen,
};
use serde_json::{Value, json};

mod backend;

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

/// A Store on a database the test can read too.
struct Db {
    store: ConfigStore,
    url: String,
    who: Actor,
    _dir: tempfile::TempDir,
}

impl Db {
    /// `production` runs `api` with `X=0` and `Y=0`; Branch `dev` has its own `api`.
    fn new() -> Self {
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
        let db = Self {
            store,
            url,
            who,
            _dir: dir,
        };
        db.service("production", 3, "api");
        db.set("production", &[("api.env.X", "0"), ("api.env.Y", "0")]);
        db.branch(9, "production", "dev", &["api"]);
        db
    }

    fn service(&self, environment: &str, n: u8, name: &str) {
        self.store
            .write(
                &self.who,
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

    fn branch(&self, n: u8, from: &str, name: &str, copy: &[&str]) {
        self.store
            .write(
                &self.who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(n)).unwrap(),
                    from: at(from),
                    name: EnvironmentName::parse(name).unwrap(),
                    copy: copy
                        .iter()
                        .map(|node| ployz_store::NodeName::parse(node).unwrap())
                        .collect(),
                    live: Vec::new(),
                    setup: Vec::new(),
                    keep: false,
                    fix: None,
                },
            )
            .unwrap();
    }

    fn set(&self, environment: &str, changes: &[(&str, &str)]) {
        self.store
            .write(
                &self.who,
                &Edit {
                    environment: at(environment),
                    expect: None,
                    changes: changes
                        .iter()
                        .map(|(path, value)| ployz_store::Change::Set {
                            path: SettingPath::parse(path).unwrap(),
                            value: json!(value),
                        })
                        .collect(),
                },
            )
            .unwrap();
    }

    /// `service`'s variables in `environment`'s Working State.
    fn env(&self, environment: &str, service: &str) -> Value {
        self.store
            .read(
                &self.who,
                &ServiceQuery {
                    environment: at(environment),
                    service: ServiceName::parse(service).unwrap(),
                },
            )
            .unwrap()
            .values["env"]
            .clone()
    }

    fn offered(&self, from: &str, into: &str) -> SyncView {
        self.store
            .read(
                &self.who,
                &SyncQuery {
                    from: at(from),
                    into: Some(at(into)),
                    when: None,
                },
            )
            .unwrap()
    }

    /// Include `from` into `into` as Sync `id`: `picks` by name, or the rows ticked.
    fn include(
        &self,
        (from, into): (&str, &str),
        id: &SyncId,
        picks: Option<&[&str]>,
    ) -> Result<Synced, (RpcErrorCode, String)> {
        let view = self.offered(from, into);
        self.store
            .write(&self.who, &changes(&view, id, picks))
            .map_err(|error| (error.code, error.message))
    }

    fn remove(&self, into: &str, proposal: &ProposalId) -> Result<bool, (RpcErrorCode, String)> {
        self.store
            .write(
                &self.who,
                &RemoveProposal {
                    environment: at(into),
                    proposal: proposal.clone(),
                    version: None,
                },
            )
            .map(|removed| removed.removed)
            .map_err(|error| (error.code, error.message))
    }

    /// Every row `sql` selects, each column as text (`NULL` for none).
    fn rows(&self, sql: &str) -> Vec<Vec<String>> {
        match self.url.strip_prefix("sqlite:") {
            Some(path) => {
                let connection = rusqlite::Connection::open(path).unwrap();
                let mut statement = connection.prepare(sql).unwrap();
                let columns = statement.column_count();
                statement
                    .query_map([], |row| {
                        (0..columns)
                            .map(|index| {
                                row.get::<_, Option<String>>(index)
                                    .map(|cell| cell.unwrap_or_else(|| "NULL".into()))
                            })
                            .collect()
                    })
                    .unwrap()
                    .map(Result::unwrap)
                    .collect()
            }
            None => postgres::Client::connect(&self.url, postgres::NoTls)
                .unwrap()
                .query(sql, &[])
                .unwrap()
                .iter()
                .map(|row| {
                    (0..row.len())
                        .map(|index| {
                            row.get::<_, Option<String>>(index)
                                .unwrap_or_else(|| "NULL".into())
                        })
                        .collect()
                })
                .collect(),
        }
    }

    /// Set `changes`, each a JSON value, in `environment`'s Working State.
    fn put(&self, environment: &str, changes: &[(&str, Value)]) {
        self.store
            .write(
                &self.who,
                &Edit {
                    environment: at(environment),
                    expect: None,
                    changes: changes
                        .iter()
                        .map(|(path, value)| ployz_store::Change::Set {
                            path: SettingPath::parse(path).unwrap(),
                            value: value.clone(),
                        })
                        .collect(),
                },
            )
            .unwrap();
    }

    /// Save `environment`'s draft; its revision after.
    fn save(&self, environment: &str) -> u64 {
        let published = self
            .store
            .write(
                &self.who,
                &ployz_store::Publish {
                    environment: at(environment),
                    version: None,
                    message: None,
                    accept_volume_loss: Vec::new(),
                },
            )
            .unwrap();
        published.environment.revision.0
    }

    /// Discard `environment`'s draft back to Head: whole, or one `path`.
    fn discard(&self, environment: &str, path: Option<&str>) {
        self.store
            .write(
                &self.who,
                &ployz_store::Discard {
                    environment: at(environment),
                    target: ployz_store::DiscardTarget::Head,
                    path: path.map(|path| SettingPath::parse(path).unwrap()),
                    version: None,
                },
            )
            .unwrap();
    }

    fn undo(&self, synced: &Synced) -> Result<(), (RpcErrorCode, String)> {
        self.store
            .write(
                &self.who,
                &ployz_store::UndoSync {
                    sync: synced.sync.clone(),
                },
            )
            .map(|_| ())
            .map_err(|error| (error.code, error.message))
    }

    /// `environment`'s `diff` version and revision.
    fn diff(&self, environment: &str) -> (String, u64) {
        let diff = self
            .store
            .read(
                &self.who,
                &ployz_store::DiffQuery {
                    environment: at(environment),
                },
            )
            .unwrap();
        (diff.version, diff.environment.revision.0)
    }

    /// The services `environment`'s Working State runs.
    fn services(&self, environment: &str) -> Vec<String> {
        self.store
            .read(
                &self.who,
                &ployz_store::ServicesQuery {
                    environment: at(environment),
                },
            )
            .unwrap()
            .services
            .into_iter()
            .map(|listing| listing.service.name.to_string())
            .collect()
    }

    /// production's proposals: source name, first Sync, last Sync.
    fn proposals(&self) -> Vec<Vec<String>> {
        self.rows(&format!(
            "SELECT source_name, first_sync, last_sync FROM config_proposal \
             WHERE environment_id = '{}' ORDER BY source_name",
            uuid(2)
        ))
    }

    /// production's arrivals of `api`'s variables, by name: state, value, prior, was,
    /// Sync, whether a proposal owns it, and its source cell.
    fn arrivals(&self) -> BTreeMap<String, [String; 7]> {
        self.rows(&format!(
            "SELECT at, state, value, COALESCE(prior, 'NULL'), COALESCE(was, 'NULL'), \
             COALESCE(sync_id, 'NULL'), \
             CASE WHEN proposal_id IS NULL THEN 'unowned' ELSE 'owned' END, \
             COALESCE(source, 'NULL') \
             FROM config_sync_arrival WHERE environment_id = '{}' AND lineage = '{}'",
            uuid(2),
            uuid(3)
        ))
        .into_iter()
        .map(|row| {
            let mut row = row.into_iter();
            let at = row.next().unwrap();
            let rest: Vec<String> = row.collect();
            (at, rest.try_into().unwrap())
        })
        .collect()
    }

    /// What production and dev last shared, as `api`'s variables.
    fn base(&self) -> BTreeMap<String, String> {
        let rows = self.rows("SELECT base FROM config_sync_base");
        assert_eq!(rows.len(), 1, "one pair");
        let base: Value = serde_json::from_str(&rows[0][0]).unwrap();
        base["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|service| service["slug"] == "api")
            .unwrap()["variables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|variable| {
                (
                    variable["key"].as_str().unwrap().to_owned(),
                    variable["value"]["value"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// `production`'s draft as the diff lists its proposals: source, newer, rows owned.
    fn included(&self) -> Vec<(String, bool, usize)> {
        self.store
            .read(
                &self.who,
                &ployz_store::DiffQuery {
                    environment: at("production"),
                },
            )
            .unwrap()
            .included
            .into_iter()
            .map(|included| {
                let name = match included.source {
                    ployz_store::ProposalSource::Environment { name, .. }
                    | ployz_store::ProposalSource::PullRequest { name, .. } => name,
                };
                (name, included.newer, included.changes)
            })
            .collect()
    }
}

/// A stored arrival, its cells as the literal they hold: state, value, prior, was,
/// Sync, owner, source.
type Stored = [String; 7];

/// What a stored cell holds: its literal, or `-` for none.
fn literal(cell: &str) -> String {
    if cell == "NULL" {
        return "-".into();
    }
    let cell: Value = serde_json::from_str(cell).unwrap();
    let cell = cell.get("cell").unwrap_or(&cell);
    cell["value"]["value"].as_str().unwrap().to_owned()
}

/// production's arrivals of `api`'s variables, by key, their cells as literals and
/// their Sync as `S<n>`.
fn arrivals(db: &Db) -> BTreeMap<String, Stored> {
    db.arrivals()
        .into_iter()
        .map(|(at, [state, value, prior, was, sync, owner, source])| {
            let sync = (1..=9)
                .find(|n| sync_id(*n).as_str() == sync)
                .map_or_else(|| sync.clone(), |n| format!("S{n}"));
            (
                at.trim_start_matches("variables.").to_owned(),
                [
                    state,
                    literal(&value),
                    literal(&prior),
                    literal(&was),
                    sync,
                    owner,
                    literal(&source),
                ],
            )
        })
        .collect()
}

fn stored(cells: [&str; 7]) -> Stored {
    cells.map(str::to_owned)
}

fn map<const N: usize>(pairs: [(&str, &str); N]) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn sync_id(n: u8) -> SyncId {
    SyncId::parse(format!("00000000-0000-4000-8000-0000000002{n:02}")).unwrap()
}

fn changes(view: &SyncView, id: &SyncId, picks: Option<&[&str]>) -> SyncChanges {
    SyncChanges {
        from: at(view.from.name.as_str()),
        into: Some(at(view.into.name.as_str())),
        when: None,
        version: view.version.clone(),
        picks: picks.map(|picks| picks.iter().map(|label| (*label).into()).collect()),
        skip: Vec::new(),
        values: BTreeMap::new(),
        id: Some(id.clone()),
    }
}

fn proposal(synced: &Synced) -> ProposalId {
    let SyncedWhen::Now { proposal, .. } = &synced.when else {
        panic!("an Include stages now")
    };
    proposal.clone()
}

/// Runner B's worked trace, step by step, with the rows stored after each.
#[test]
fn the_worked_trace() {
    let db = Db::new();
    let s = |n| format!("S{n}");
    let (s1, s2) = (s(1), s(2));

    // 1. First transfer: dev sets X=1, Y=1 and production includes both.
    db.set("dev", &[("api.env.X", "1"), ("api.env.Y", "1")]);
    let view = db.offered("dev", "production");
    let first = changes(&view, &sync_id(1), None);
    let synced = db.store.write(&db.who, &first).unwrap();
    let p1 = proposal(&synced);
    assert_eq!(
        db.proposals(),
        [["dev", sync_id(1).as_str(), sync_id(1).as_str()]]
    );
    let owned_x = stored(["pending", "1", "0", "0", &s1, "owned", "1"]);
    let owned_y = stored(["pending", "1", "0", "0", &s1, "owned", "1"]);
    let mut expected = BTreeMap::from([("X".to_owned(), owned_x), ("Y".to_owned(), owned_y)]);
    assert_eq!(arrivals(&db), expected);
    assert_eq!(db.base(), map([("X", "1"), ("Y", "1")]));
    assert_eq!(db.env("production", "api"), json!({"X": "1", "Y": "1"}));
    // A retry of S1 returns its receipt and writes nothing.
    let again = db.store.write(&db.who, &first).unwrap();
    assert_eq!((&again.sync, proposal(&again)), (&synced.sync, p1.clone()));
    assert_eq!(arrivals(&db), expected);
    assert_eq!(db.included(), [("dev".to_owned(), false, 2)]);

    // 2. Destination override: production sets X itself, which releases X.
    db.set("production", &[("api.env.X", "local")]);
    expected.insert(
        "X".into(),
        stored(["pending", "1", "0", "0", &s1, "unowned", "-"]),
    );
    assert_eq!(arrivals(&db), expected);
    assert_eq!(db.included(), [("dev".to_owned(), false, 1)]);

    // 3. Source amendment: dev sets Y=2. production's draft says dev is newer;
    // nothing is written there.
    db.set("dev", &[("api.env.Y", "2")]);
    assert_eq!(arrivals(&db), expected);
    assert_eq!(db.included(), [("dev".to_owned(), true, 1)]);

    // 4. Second transfer: only Y is offered, ticked; X's override stays.
    let view = db.offered("dev", "production");
    let offered: Vec<(String, bool)> = view
        .rows
        .iter()
        .map(|row| (row.at.to_string(), row.ticked))
        .collect();
    assert_eq!(offered, [("api.env.Y".to_owned(), true)]);
    let refreshed = db
        .include(("dev", "production"), &sync_id(2), None)
        .unwrap();
    assert_eq!(proposal(&refreshed), p1);
    assert_eq!(
        db.proposals(),
        [["dev", sync_id(1).as_str(), sync_id(2).as_str()]]
    );
    // The upsert keeps the first `was` and `prior`.
    expected.insert(
        "Y".into(),
        stored(["pending", "2", "0", "0", &s2, "owned", "2"]),
    );
    assert_eq!(arrivals(&db), expected);
    assert_eq!(db.base(), map([("X", "1"), ("Y", "2")]));
    assert_eq!(db.env("production", "api"), json!({"X": "local", "Y": "2"}));
    assert_eq!(db.included(), [("dev".to_owned(), false, 1)]);

    // 5. Remove: Y goes back to 0, X keeps production's own value.
    assert_eq!(db.remove("production", &p1), Ok(true));
    assert_eq!(db.env("production", "api"), json!({"X": "local", "Y": "0"}));
    assert!(db.proposals().is_empty());
    expected.remove("Y");
    assert_eq!(arrivals(&db), expected);
    assert_eq!(db.base(), map([("X", "1"), ("Y", "0")]));
    assert!(db.included().is_empty());
    // A retried Remove finds nothing to remove.
    assert_eq!(db.remove("production", &p1), Ok(false));
    // Y is offered again; X is not, as dev hasn't changed it since they shared.
    let labels: Vec<String> = db
        .offered("dev", "production")
        .rows
        .iter()
        .map(|row| row.at.to_string())
        .collect();
    assert_eq!(labels, ["api.env.Y"]);
}

/// `label`'s row in `view`.
fn row<'view>(view: &'view SyncView, label: &str) -> &'view ployz_store::SyncRow {
    view.rows
        .iter()
        .find(|row| row.at.to_string() == label)
        .unwrap_or_else(|| panic!("{label} offered"))
}

/// Branch `qa` beside `dev`, with its own `api`.
fn with_qa(db: &Db) {
    db.branch(10, "production", "qa", &["api"]);
}

// Case 1.

#[test]
fn refresh_exposes_a_source_change_to_an_overridden_row_unticked() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1"), ("api.env.Y", "1")]);
    db.include(("dev", "production"), &sync_id(1), None)
        .unwrap();
    db.set("production", &[("api.env.X", "local")]);
    db.set("dev", &[("api.env.X", "2"), ("api.env.Y", "2")]);
    let view = db.offered("dev", "production");
    let x = row(&view, "api.env.X");
    assert_eq!(
        (x.change, x.ticked),
        (ployz_store::SyncChange::Conflict, false)
    );
    assert!(row(&view, "api.env.Y").ticked);
    // The ticked rows only: X keeps production's own value.
    db.include(("dev", "production"), &sync_id(2), None)
        .unwrap();
    assert_eq!(db.env("production", "api"), json!({"X": "local", "Y": "2"}));
}

// Case 2.

#[test]
fn a_second_proposal_cannot_take_an_owned_row() {
    let db = Db::new();
    with_qa(&db);
    db.set("dev", &[("api.env.X", "1")]);
    db.set("qa", &[("api.env.X", "2"), ("api.env.Y", "2")]);
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    let view = db.offered("qa", "production");
    let x = row(&view, "api.env.X");
    assert_eq!((x.held_by.as_deref(), x.ticked), (Some("dev"), false));
    let (code, _) = db
        .include(("qa", "production"), &sync_id(2), Some(&["api.env.X"]))
        .unwrap_err();
    assert_eq!(code, RpcErrorCode::Conflict);
    let b = proposal(&db.include(("qa", "production"), &sync_id(3), None).unwrap());
    assert_eq!(db.env("production", "api"), json!({"X": "1", "Y": "2"}));
    // Removing B touches nothing of A's; removing A restores production's 0.
    assert_eq!(db.remove("production", &b), Ok(true));
    assert_eq!(db.env("production", "api"), json!({"X": "1", "Y": "0"}));
    assert_eq!(db.remove("production", &a), Ok(true));
    assert_eq!(db.env("production", "api"), json!({"X": "0", "Y": "0"}));
}

// Case 3.

#[test]
fn an_equal_claim_is_disclosed_and_offered_again_after_remove() {
    let db = Db::new();
    with_qa(&db);
    db.set("dev", &[("api.env.X", "1")]);
    db.set("qa", &[("api.env.X", "1")]);
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    let view = db.offered("qa", "production");
    assert_eq!(row(&view, "api.env.X").held_by.as_deref(), Some("dev"));
    assert_eq!(db.remove("production", &a), Ok(true));
    let view = db.offered("qa", "production");
    let x = row(&view, "api.env.X");
    assert_eq!((x.held_by.as_deref(), x.ticked), (None, true));
}

// Case 4.

#[test]
fn a_refreshed_remove_restores_the_original_value() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1")]);
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    db.set("dev", &[("api.env.X", "2")]);
    db.include(("dev", "production"), &sync_id(2), None)
        .unwrap();
    assert_eq!(db.env("production", "api")["X"], "2");
    assert_eq!(db.remove("production", &a), Ok(true));
    assert_eq!(db.env("production", "api")["X"], "0");
}

// Case 5.

#[test]
fn an_edit_and_back_is_local_and_a_no_op_write_is_not() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1"), ("api.env.Y", "1")]);
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    db.set("production", &[("api.env.X", "2")]);
    db.set("production", &[("api.env.X", "1")]);
    db.set("production", &[("api.env.Y", "1")]);
    let owners: Vec<String> = arrivals(&db)
        .into_values()
        .map(|row| row[5].clone())
        .collect();
    assert_eq!(owners, ["unowned", "owned"]);
    assert_eq!(db.remove("production", &a), Ok(true));
    assert_eq!(db.env("production", "api"), json!({"X": "1", "Y": "0"}));
}

// Case 6, G7.

#[test]
fn remove_is_refused_while_the_draft_uses_a_service_it_introduced() {
    let db = Db::new();
    db.service("dev", 11, "cache");
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    assert_eq!(db.services("production"), ["api", "cache"]);
    db.set(
        "production",
        &[("api.env.CACHE", "${{ cache.PLOYZ_PRIVATE_DOMAIN }}")],
    );
    let (code, message) = db.remove("production", &a).unwrap_err();
    assert_eq!(code, RpcErrorCode::Conflict, "{message}");
    assert!(message.contains("cache"), "{message}");
    assert_eq!(db.services("production"), ["api", "cache"]);
    // Without the reference, Remove takes cache back out.
    db.discard("production", Some("api.env.CACHE"));
    assert_eq!(db.remove("production", &a), Ok(true));
    assert_eq!(db.services("production"), ["api"]);
}

#[test]
fn remove_is_refused_once_the_draft_edited_a_service_it_introduced() {
    let db = Db::new();
    db.service("dev", 11, "cache");
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    db.set("production", &[("cache.image", "cache:2")]);
    let (code, message) = db.remove("production", &a).unwrap_err();
    assert_eq!(code, RpcErrorCode::Conflict, "{message}");
    assert!(message.contains("cache"), "{message}");
    assert_eq!(db.services("production"), ["api", "cache"]);
}

// Case 8.

#[test]
fn a_secret_resolved_on_an_existing_service_survives_remove() {
    let db = Db::new();
    db.put("dev", &[("api.env.KEY", json!({"secret": "dev-key"}))]);
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    assert_eq!(db.env("production", "api")["KEY"], json!({"secret": false}));
    db.put(
        "production",
        &[("api.env.KEY", json!({"secret": "prod-key"}))],
    );
    assert_eq!(db.remove("production", &a), Ok(true));
    assert_eq!(db.env("production", "api")["KEY"], json!({"secret": true}));
    // No plaintext is stored with what arrived.
    let stored =
        db.rows("SELECT value, COALESCE(prior, ''), COALESCE(was, '') FROM config_sync_arrival");
    assert!(!format!("{stored:?}").contains("key\""), "{stored:?}");
}

#[test]
fn an_introduced_service_with_a_resolved_secret_refuses_remove() {
    let db = Db::new();
    db.service("dev", 11, "cache");
    db.put("dev", &[("cache.env.KEY", json!({"secret": "dev-key"}))]);
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    db.put(
        "production",
        &[("cache.env.KEY", json!({"secret": "prod-key"}))],
    );
    let (code, message) = db.remove("production", &a).unwrap_err();
    assert_eq!(code, RpcErrorCode::Conflict, "{message}");
    assert!(message.contains("cache"), "{message}");
    assert_eq!(db.services("production"), ["api", "cache"]);
}

// Case 10.

#[test]
fn a_deleted_source_stays_listed_and_removable() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1")]);
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    db.store
        .write(
            &db.who,
            &ployz_store::RemoveEnvironment {
                environment: at("dev"),
            },
        )
        .unwrap();
    assert_eq!(db.included(), [("dev".to_owned(), false, 1)]);
    assert_eq!(db.remove("production", &a), Ok(true));
    assert_eq!(db.env("production", "api")["X"], "0");
    assert!(db.proposals().is_empty());
}

// Case 11, amendment 1.

#[test]
fn cancelling_proposals_stay_listed_and_a_nothing_staged_save_consumes() {
    let db = Db::new();
    backend::deploy(&db.store, &db.who, "production", 1);
    with_qa(&db);
    db.set("dev", &[("api.env.X", "1")]);
    db.include(("dev", "production"), &sync_id(1), None)
        .unwrap();
    db.set("production", &[("api.env.X", "0")]);
    // Production's draft is empty again, yet dev is still included.
    db.set("qa", &[("api.env.Y", "1")]);
    let b = proposal(&db.include(("qa", "production"), &sync_id(2), None).unwrap());
    db.set("production", &[("api.env.Y", "0")]);
    assert_eq!(
        db.included(),
        [("dev".to_owned(), false, 0), ("qa".to_owned(), false, 0)]
    );
    let (_, before) = db.diff("production");
    let after = db.save("production");
    assert!(after > before, "consuming moves the revision");
    assert_eq!(db.diff("production").1, after);
    assert!(db.proposals().is_empty());
    assert_eq!(db.remove("production", &b), Ok(false));
}

// Case 12, G9.

#[test]
fn an_admitted_retry_neither_settles_nor_consumes_a_later_proposal() {
    let db = Db::new();
    let id = backend::admit(&db.store, &db.who, "production", 1);
    db.set("dev", &[("api.env.X", "1")]);
    db.include(("dev", "production"), &sync_id(1), None)
        .unwrap();
    backend::run(&db.store, &id);
    // The run Follows what it shipped into dev, so dev is newer; production's
    // proposal is untouched.
    assert_eq!(db.included(), [("dev".to_owned(), true, 1)]);
    assert_eq!(
        db.proposals(),
        [["dev", sync_id(1).as_str(), sync_id(1).as_str()]]
    );
    assert_eq!(
        arrivals(&db)["X"],
        stored(["pending", "1", "0", "0", "S1", "owned", "1"])
    );
}

// G10.

#[test]
fn stale_versions_are_refused_and_a_retry_replays() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1")]);
    let view = db.offered("dev", "production");
    db.set("production", &[("api.env.Y", "local")]);
    let (code, _) = db
        .store
        .write(&db.who, &changes(&view, &sync_id(1), None))
        .map_err(|error| (error.code, error.message))
        .unwrap_err();
    assert_eq!(code, RpcErrorCode::Conflict);
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    let (version, _) = db.diff("production");
    db.set("production", &[("api.env.Y", "local2")]);
    let stale = db.store.write(
        &db.who,
        &RemoveProposal {
            environment: at("production"),
            proposal: a.clone(),
            version: Some(version),
        },
    );
    assert_eq!(stale.unwrap_err().code, RpcErrorCode::Conflict);
    let (version, _) = db.diff("production");
    let removed = db.store.write(
        &db.who,
        &RemoveProposal {
            environment: at("production"),
            proposal: a.clone(),
            version: Some(version),
        },
    );
    assert!(removed.unwrap().removed);
    assert_eq!(db.remove("production", &a), Ok(false));
}

// Lifetime.

#[test]
fn a_save_ends_proposals_and_keeps_what_arrived() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1")]);
    let a = proposal(
        &db.include(("dev", "production"), &sync_id(1), None)
            .unwrap(),
    );
    db.save("production");
    assert!(db.proposals().is_empty());
    assert_eq!(
        arrivals(&db)["X"],
        stored(["pending", "1", "0", "0", "S1", "unowned", "-"])
    );
    assert_eq!(db.remove("production", &a), Ok(false));
    assert_eq!(db.env("production", "api")["X"], "1");
}

#[test]
fn a_proposal_over_a_saved_arrival_removes_back_to_what_was_saved() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1")]);
    db.include(("dev", "production"), &sync_id(1), None)
        .unwrap();
    db.save("production");
    db.set("dev", &[("api.env.X", "2")]);
    let b = proposal(
        &db.include(("dev", "production"), &sync_id(2), None)
            .unwrap(),
    );
    assert_eq!(db.remove("production", &b), Ok(true));
    assert_eq!(db.env("production", "api")["X"], "1");
    // The base is what production saved, so dev's 2 is an ordinary update again.
    let view = db.offered("dev", "production");
    let x = row(&view, "api.env.X");
    assert_eq!(
        (x.change, x.ticked),
        (ployz_store::SyncChange::Changed, true)
    );
}

#[test]
fn a_manual_deploy_ends_proposals() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1")]);
    db.include(("dev", "production"), &sync_id(1), None)
        .unwrap();
    backend::deploy(&db.store, &db.who, "production", 1);
    assert!(db.proposals().is_empty());
    assert_eq!(db.env("production", "api")["X"], "1");
}

#[test]
fn a_whole_discard_drops_membership_and_a_path_discard_rewinds_one_row() {
    let db = Db::new();
    backend::deploy(&db.store, &db.who, "production", 1);
    db.set("dev", &[("api.env.X", "1"), ("api.env.Y", "1")]);
    db.include(("dev", "production"), &sync_id(1), None)
        .unwrap();
    db.discard("production", Some("api.env.X"));
    assert_eq!(db.env("production", "api"), json!({"X": "0", "Y": "1"}));
    assert_eq!(db.included(), [("dev".to_owned(), false, 1)]);
    db.discard("production", None);
    assert_eq!(db.env("production", "api"), json!({"X": "0", "Y": "0"}));
    assert!(db.proposals().is_empty());
    assert!(db.included().is_empty());
}

// Undo.

#[test]
fn undo_of_an_unrefreshed_proposal_removes_it() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1")]);
    let synced = db
        .include(("dev", "production"), &sync_id(1), None)
        .unwrap();
    assert_eq!(db.undo(&synced), Ok(()));
    assert!(db.proposals().is_empty());
    assert_eq!(db.env("production", "api")["X"], "0");
}

#[test]
fn undo_of_a_refreshed_proposal_is_refused() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1")]);
    let first = db
        .include(("dev", "production"), &sync_id(1), None)
        .unwrap();
    db.set("dev", &[("api.env.Y", "1")]);
    db.include(("dev", "production"), &sync_id(2), None)
        .unwrap();
    let (code, _) = db.undo(&first).unwrap_err();
    assert_eq!(code, RpcErrorCode::Conflict);
    assert_eq!(db.env("production", "api"), json!({"X": "1", "Y": "1"}));
}

#[test]
fn a_saved_sync_still_undoes_as_before() {
    let db = Db::new();
    db.set("dev", &[("api.env.X", "1")]);
    let synced = db
        .include(("dev", "production"), &sync_id(1), None)
        .unwrap();
    db.save("production");
    assert_eq!(db.undo(&synced), Ok(()));
    assert_eq!(db.env("production", "api")["X"], "0");
}

#[test]
fn deleting_the_destination_deletes_its_proposals() {
    let db = Db::new();
    db.branch(10, "dev", "qa", &["api"]);
    db.set("qa", &[("api.env.X", "1")]);
    db.include(("qa", "dev"), &sync_id(1), None).unwrap();
    let dev = uuid(9);
    let count = |table: &str| {
        db.rows(&format!(
            "SELECT CAST(COUNT(*) AS TEXT) FROM {table} WHERE environment_id = '{dev}'"
        ))
    };
    assert_ne!(count("config_proposal"), [["0"]]);
    for environment in ["qa", "dev"] {
        db.store
            .write(
                &db.who,
                &ployz_store::RemoveEnvironment {
                    environment: at(environment),
                },
            )
            .unwrap();
    }
    assert_eq!(count("config_proposal"), [["0"]]);
    assert_eq!(count("config_sync_arrival"), [["0"]]);
}
