//! Core-local NATS authorization grants, stored in SQLite.
//!
//! One `nats_authorizations` row per grant holds the whole `NatsAuthorizedUser`,
//! keyed by its `authority_record_key`. `authorized-users.conf` is a rendered
//! projection of this table (see `adapters/nats_authorization`), so the grant set
//! is durable operator intent — mirrored to candidates and seeded on promotion
//! like the machine roster, never re-derived from partial truth.

use crate::core_store::{CoreStore, CoreStoreError, query_json_list, to_json};
use ployz_core::nats_config::{NatsAuthorizedUser, parse_authorized_users};
use rusqlite::{Connection, params};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct NatsAuthorizationStore {
    store: CoreStore,
}

impl NatsAuthorizationStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    /// Add or replace a grant by its `authority_record_key`. Operator grants are keyed
    /// by public key, so operator and Cloud both persist; every other principal is
    /// unique by role or machine id.
    pub async fn upsert(
        &self,
        user: &NatsAuthorizedUser,
    ) -> Result<(), NatsAuthorizationStoreError> {
        let user = user.clone();
        self.store
            .call(move |conn| put_authorization(conn, &user))
            .await
            .map_err(store_error)
    }

    /// Every current grant in insertion order (SQLite `rowid`, preserved across
    /// upserts). Insertion order — not key order — is what makes the render
    /// byte-stable: seeding from the Host Runner-written conf and re-rendering it must
    /// reproduce the same file, and a new machine grant appends, exactly as the
    /// prior disk-merge writer behaved. A reorder would make startup's no-op render
    /// write + reload needlessly.
    pub async fn list(&self) -> Result<Vec<NatsAuthorizedUser>, NatsAuthorizationStoreError> {
        self.store
            .call(|conn| {
                query_json_list(conn, "SELECT json FROM nats_authorizations ORDER BY rowid")
            })
            .await
            .map_err(store_error)
    }

    /// Import the Host Runner-written `authorized-users.conf` into the store exactly once,
    /// on first boot when the store is empty. Thereafter the store is authoritative
    /// and the conf is its rendered projection. A missing conf is a no-op (a fresh
    /// core that has not written one yet).
    pub async fn seed_from_conf_if_empty(
        &self,
        conf_path: &Path,
    ) -> Result<(), NatsAuthorizationStoreError> {
        let contents = match std::fs::read_to_string(conf_path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(NatsAuthorizationStoreError {
                    message: format!("read {}: {error}", conf_path.display()),
                });
            }
        };
        let grants =
            parse_authorized_users(&contents).map_err(|error| NatsAuthorizationStoreError {
                message: format!("parse {}: {error}", conf_path.display()),
            })?;
        // Import all grants in one transaction, re-checking emptiness inside it: a
        // partial import (a mid-loop error) would otherwise leave the table non-empty,
        // so the next start skips the import and renders a truncated conf that drops
        // principals. All-or-nothing keeps the table empty until the whole set lands.
        self.store
            .call(move |conn| seed_grants_if_empty(conn, &grants))
            .await
            .map_err(store_error)
    }
}

fn seed_grants_if_empty(
    conn: &mut Connection,
    grants: &[NatsAuthorizedUser],
) -> Result<(), rusqlite::Error> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM nats_authorizations", [], |row| {
        row.get(0)
    })?;
    if count != 0 {
        return Ok(());
    }
    let transaction = conn.transaction()?;
    for grant in grants {
        put_authorization(&transaction, grant)?;
    }
    transaction.commit()
}

fn put_authorization(conn: &Connection, user: &NatsAuthorizedUser) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO nats_authorizations (authority_key, json) VALUES (?1, ?2)
         ON CONFLICT(authority_key) DO UPDATE SET json = excluded.json",
        params![user.authority_record_key(), to_json(user)?],
    )?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("nats authorization store: {message}")]
pub struct NatsAuthorizationStoreError {
    message: String,
}

fn store_error(error: CoreStoreError) -> NatsAuthorizationStoreError {
    NatsAuthorizationStoreError {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::nats_config::MintedNatsUser;
    use ployz_core::security::NatsPrincipal;

    fn user_grant() -> NatsAuthorizedUser {
        NatsAuthorizedUser {
            principal: NatsPrincipal::Operator,
            nkey_public: MintedNatsUser::generate().expect("mint user").public,
        }
    }

    #[tokio::test]
    async fn upsert_lists_and_coexists_by_user_key() {
        let store = NatsAuthorizationStore::new(CoreStore::open_in_memory().await.expect("store"));
        assert!(store.list().await.expect("empty").is_empty());

        let operator = user_grant();
        let cloud = user_grant();
        store.upsert(&operator).await.expect("operator");
        store.upsert(&cloud).await.expect("cloud");
        // Upserting the same key again replaces, not duplicates.
        store.upsert(&operator).await.expect("operator again");

        let grants = store.list().await.expect("list");
        assert_eq!(grants.len(), 2);
    }
}
