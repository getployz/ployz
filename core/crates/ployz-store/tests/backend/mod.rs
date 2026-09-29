//! The database each behaviour suite runs on: SQLite, or Postgres when
//! `PLOYZ_STORE_TEST_POSTGRES` names a server as `postgres://USER:PASSWORD@HOST:PORT`
//! (each Store gets its own new database).
#![allow(dead_code, reason = "each suite uses what it needs")]

use ployz_store::ConfigStore;

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
        return ConfigStore::open("sqlite::memory:").unwrap();
    }
    ConfigStore::open(&fresh_url(&tempfile::tempdir().unwrap())).unwrap()
}
