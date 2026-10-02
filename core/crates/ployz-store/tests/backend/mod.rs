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

/// A new, empty Store.
pub fn open() -> ConfigStore {
    if std::env::var_os("PLOYZ_STORE_TEST_POSTGRES").is_none() {
        return ConfigStore::open("sqlite::memory:", key()).unwrap();
    }
    ConfigStore::open(&fresh_url(&tempfile::tempdir().unwrap()), key()).unwrap()
}

/// The key every test Store seals with.
pub fn key() -> SealingKey {
    SealingKey::new(b"test-encryption-secret").unwrap()
}

/// GitHub's ID for repository `n`.
pub fn repo_id(n: u64) -> ployz_store::RepositoryId {
    ployz_store::RepositoryId::parse(n).unwrap()
}

/// A repository as `owner/name`.
pub fn repo_name(name: &str) -> ployz_store::RepositoryName {
    ployz_store::RepositoryName::parse(name).unwrap()
}

/// A Git branch.
pub fn git_branch(name: &str) -> ployz_store::BranchName {
    ployz_store::BranchName::parse(name).unwrap()
}

/// A full Git commit.
pub fn sha(commit: &str) -> ployz_store::CommitSha {
    ployz_store::CommitSha::parse(commit).unwrap()
}

/// Pull request number `n`.
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
    use serde_json::json;
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
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id, &runner).unwrap();
    let names: Vec<String> = claimed
        .intent
        .target
        .iter()
        .map(|service| service.name.to_string())
        .collect();
    let operation = |index: usize| {
        json!({"type": "remove_container", "machine_id": "a".repeat(32),
               "container_id": format!("{index:x}").repeat(64)})
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
        .record(&id, &runner, ployz_store::RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: ployz_core::DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(json!({
            "type": "success",
            "completed": (0..names.len()).map(operation).collect::<Vec<_>>()
        }))
        .unwrap();
    store
        .record(
            &id,
            &runner,
            ployz_store::RunEvidence::Executed {
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
    claimed.input
}
