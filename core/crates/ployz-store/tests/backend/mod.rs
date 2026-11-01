//! The database each behaviour suite runs on: SQLite, or Postgres when
//! `PLOYZ_STORE_TEST_POSTGRES` names a server as `postgres://USER:PASSWORD@HOST:PORT`
//! (each Store gets its own new database).
#![allow(dead_code, reason = "each suite uses what it needs")]

use ployz_store::{ConfigStore, SealingKey};

/// A new, empty Store database, shared by every handle opened on the URL.
pub fn fresh_url(dir: &tempfile::TempDir) -> String {
    let Ok(server) = std::env::var("PLOYZ_STORE_TEST_POSTGRES") else {
        return format!("sqlite:{}", dir.path().join("store.db").display());
    };
    let name = format!("store_{}", uuid::Uuid::new_v4().simple());
    postgres::Client::connect(&server, postgres::NoTls)
        .unwrap()
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .unwrap();
    format!("{server}/{name}")
}

/// Run `sql` on the database at `url`, beside the Store.
pub fn execute(url: &str, sql: &str) {
    match url.strip_prefix("sqlite:") {
        Some(path) => {
            rusqlite::Connection::open(path)
                .unwrap()
                .execute(sql, [])
                .unwrap();
        }
        None => {
            postgres::Client::connect(url, postgres::NoTls)
                .unwrap()
                .execute(sql, &[])
                .unwrap();
        }
    }
}

/// 48 bytes of ciphertext the browser could have sealed: `n` repeated.
pub fn ciphertext(n: u8) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode([n; 48])
}

/// A variable value the browser sealed for Service `service` of Environment
/// `environment`, as variable `key`, holding [`ciphertext`] `n`.
pub fn client_sealed(environment: &str, service: &str, key: &str, n: u8) -> serde_json::Value {
    serde_json::json!({
        "sealedFor": environment,
        "value": { "sealed": ciphertext(n), "scope": { "service": service }, "key": key },
    })
}

/// [`client_sealed`] as a secret setting: what an edit sets `service.env.key` to.
pub fn sealed_secret(environment: &str, service: &str, key: &str, n: u8) -> serde_json::Value {
    serde_json::json!({ "secret": client_sealed(environment, service, key, n) })
}

/// A new, empty Store.
pub fn open() -> ConfigStore {
    let Ok(server) = std::env::var("PLOYZ_STORE_TEST_POSTGRES") else {
        return ConfigStore::open("sqlite::memory:", key()).unwrap();
    };
    let template = migrated_template(&server);
    let name = format!("store_{}", uuid::Uuid::new_v4().simple());
    postgres::Client::connect(&server, postgres::NoTls)
        .unwrap()
        .batch_execute(&format!("CREATE DATABASE {name} TEMPLATE {template}"))
        .unwrap();
    ConfigStore::open(&format!("{server}/{name}"), key()).unwrap()
}

/// A Store database this test binary migrated, which [`open`] copies: copying takes a
/// fraction of migrating each one. Each build makes its own, so none goes stale.
fn migrated_template(server: &str) -> String {
    use std::hash::{Hash, Hasher};
    let binary = std::env::current_exe().unwrap();
    let built = std::fs::metadata(&binary).unwrap();
    let mut hasher = std::hash::DefaultHasher::new();
    (&binary, built.len(), built.modified().unwrap()).hash(&mut hasher);
    let template = format!("store_template_{:016x}", hasher.finish());
    let mut admin = postgres::Client::connect(server, postgres::NoTls).unwrap();
    // Held until this connection closes, so tests running at once make it only once.
    admin
        .batch_execute("SELECT pg_advisory_lock(1226)")
        .unwrap();
    let made = admin
        .query_opt("SELECT 1 FROM pg_database WHERE datname = $1", &[&template])
        .unwrap()
        .is_some();
    if !made {
        // Migrated under another name, so a test killed midway leaves no half-made template.
        let building = format!("{template}_building");
        admin
            .batch_execute(&format!("DROP DATABASE IF EXISTS {building}"))
            .unwrap();
        admin
            .batch_execute(&format!("CREATE DATABASE {building}"))
            .unwrap();
        drop(ConfigStore::open(&format!("{server}/{building}"), key()).unwrap());
        admin
            .batch_execute(&format!("ALTER DATABASE {building} RENAME TO {template}"))
            .unwrap();
    }
    template
}

/// The key every test Store seals with.
pub fn key() -> SealingKey {
    SealingKey::new(b"test-encryption-secret").unwrap()
}

pub fn repo_id(n: u64) -> ployz_store::RepositoryId {
    ployz_store::RepositoryId::parse(n).unwrap()
}

/// A repository as `owner/name`.
pub fn repo_name(name: &str) -> ployz_store::RepositoryName {
    ployz_store::RepositoryName::parse(name).unwrap()
}

pub fn git_branch(name: &str) -> ployz_store::BranchName {
    ployz_store::BranchName::parse(name).unwrap()
}

/// Cloud's evidence for the repositories `acme/web` (11) and `acme/docs` (12), each on
/// `main` with a `dev` branch.
pub fn acme_repositories() -> ployz_store::Trusted {
    let repository = |name: &str, n| ployz_store::AuthorizedRepository {
        repository: repo_name(name),
        repository_id: repo_id(n),
        access: ployz_core::config::ServiceGitAccess::Public,
        default_branch: git_branch("main"),
        branches: vec![git_branch("dev")],
    };
    ployz_store::Trusted {
        repositories: vec![repository("acme/web", 11), repository("acme/docs", 12)],
        ..ployz_store::Trusted::default()
    }
}

/// The GitHub user `ada`, an org member in [`members`].
pub fn ada() -> ployz_store::GithubUserId {
    ployz_store::GithubUserId::parse(7).unwrap()
}

/// Cloud's evidence that `ada` is the org's one member.
pub fn members() -> ployz_store::Trusted {
    ployz_store::Trusted {
        members: Some(vec![ada()]),
        ..ployz_store::Trusted::default()
    }
}

/// A full Git commit.
pub fn sha(commit: &str) -> ployz_store::CommitSha {
    ployz_store::CommitSha::parse(commit).unwrap()
}

pub fn pr_number(n: u64) -> ployz_store::PullRequestNumber {
    ployz_store::PullRequestNumber::parse(n).unwrap()
}

/// Deploy `environment` in full as Deployment `n` and record every Service applied;
/// what the runner was handed.
pub fn deploy(
    store: &ConfigStore,
    who: &ployz_store::Actor,
    environment: &str,
    n: u8,
) -> serde_json::Value {
    let id = admit(store, who, environment, n);
    run(store, &id)
}

/// Admit Deployment `n` of `environment`, publishing its Working State; [`run`] runs it.
pub fn admit(
    store: &ConfigStore,
    who: &ployz_store::Actor,
    environment: &str,
    n: u8,
) -> ployz_store::DeploymentId {
    let id = ployz_store::DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}"))
        .unwrap();
    store
        .write_trusted(
            who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: id.clone(),
                environment: ployz_store::EnvironmentRef {
                    project: None,
                    environment: Some(ployz_store::EnvironmentName::parse(environment).unwrap()),
                },
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    id
}

/// Run admitted Deployment `id` to success; what the runner was handed.
pub fn run(store: &ConfigStore, id: &ployz_store::DeploymentId) -> serde_json::Value {
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let claimed = store.claim(id, &runner).unwrap();
    finish(store, id, claimed)
}

/// Run Deployment `id`, which the runner claimed as `claimed`, to success;
/// what the runner was handed.
pub fn finish(
    store: &ConfigStore,
    id: &ployz_store::DeploymentId,
    claimed: ployz_store::Claimed,
) -> serde_json::Value {
    finish_confirming(store, id, claimed, &[])
}

/// Deploy `environment` as `n`, the runner confirming the Services of each of
/// `batches` mid-run, in order, as it does when they finish at different times.
pub fn deploy_in_batches(
    store: &ConfigStore,
    who: &ployz_store::Actor,
    environment: &str,
    n: u8,
    batches: &[&[&str]],
) -> serde_json::Value {
    let id = admit(store, who, environment, n);
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id, &runner).unwrap();
    finish_confirming(store, &id, claimed, batches)
}

/// [`finish`], confirming the Services of each of `batches` before the outcome.
pub fn finish_confirming(
    store: &ConfigStore,
    id: &ployz_store::DeploymentId,
    claimed: ployz_store::Claimed,
    batches: &[&[&str]],
) -> serde_json::Value {
    use serde_json::json;
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let mut names: Vec<String> = claimed
        .intent
        .target
        .iter()
        .map(|service| service.name.to_string())
        .collect();
    // A Service a batch confirms, removals among them, has its own operation, so it
    // waits for its batch.
    for name in batches.iter().flat_map(|batch| batch.iter()) {
        if !names.iter().any(|known| known == name) {
            names.push((*name).to_owned());
        }
    }
    let operation = |index: usize| {
        json!({"type": "remove_container", "machine_id": "a".repeat(32),
               "container_id": format!("{index:064x}")})
    };
    let preview: ployz_core::DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace,
        "operations": names.iter().enumerate().map(|(index, name)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": name,
            "operation": operation(index), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(id, &runner, ployz_store::RunEvidence::Prepared(preview))
        .unwrap();
    for batch in batches {
        let services = batch
            .iter()
            .map(|name| ployz_core::ServiceName::parse(*name).unwrap())
            .collect();
        store
            .record(id, &runner, ployz_store::RunEvidence::Confirmed(services))
            .unwrap();
    }
    let outcome: ployz_core::DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(json!({
            "type": "success",
            "completed": (0..names.len()).map(operation).collect::<Vec<_>>()
        }))
        .unwrap();
    store
        .record(
            id,
            &runner,
            ployz_store::RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
    claimed.input
}

/// The tables of the database at `url` with a row whose text holds `marker`.
pub fn tables_holding(url: &str, marker: &str) -> Vec<String> {
    let mut tables = Vec::new();
    match url.strip_prefix("sqlite:") {
        Some(path) => {
            let connection = rusqlite::Connection::open(path).unwrap();
            let names: Vec<String> = connection
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            for name in names {
                let mut statement = connection
                    .prepare(&format!("SELECT * FROM \"{name}\""))
                    .unwrap();
                let width = statement.column_count();
                let mut rows = statement.query([]).unwrap();
                while let Some(row) = rows.next().unwrap() {
                    let held = (0..width).any(|i| match row.get_ref(i).unwrap() {
                        rusqlite::types::ValueRef::Text(text)
                        | rusqlite::types::ValueRef::Blob(text) => {
                            String::from_utf8_lossy(text).contains(marker)
                        }
                        rusqlite::types::ValueRef::Null
                        | rusqlite::types::ValueRef::Integer(_)
                        | rusqlite::types::ValueRef::Real(_) => false,
                    });
                    if held {
                        tables.push(name.clone());
                        break;
                    }
                }
            }
        }
        None => {
            let mut client = postgres::Client::connect(url, postgres::NoTls).unwrap();
            let names: Vec<String> = client
                .query(
                    "SELECT table_name::text FROM information_schema.tables \
                     WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
                    &[],
                )
                .unwrap()
                .iter()
                .map(|row| row.get(0))
                .collect();
            for name in names {
                let held: bool = client
                    .query_one(
                        &format!("SELECT EXISTS (SELECT 1 FROM \"{name}\" t WHERE t::text LIKE '%' || $1 || '%')"),
                        &[&marker],
                    )
                    .unwrap()
                    .get(0);
                if held {
                    tables.push(name);
                }
            }
        }
    }
    tables.sort();
    tables
}
