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
