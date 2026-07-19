//! Core-local NATS authorization grants, stored in SQLite.
//!
//! One `nats_authorizations` row per grant holds the whole `NatsAuthorizationGrant`,
//! keyed by its `authority_record_key`. `authorized-users.conf` is a rendered
//! projection of this table (see `adapters/nats_authorization`), so the grant set
//! is durable operator intent — mirrored to candidates and seeded on promotion
//! like the machine roster, never re-derived from partial truth.

use crate::control::store::{CoreStore, CoreStoreError, from_json, query_json_list, to_json};
use ployz_core::nats_config::{
    BuildExecutorIdentity, CredentialGrant, CredentialRole, NatsAuthorizationGrant,
    NatsUserPublicKey,
};
use ployz_nats::permissions::parse_authorized_users;
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
        user: &NatsAuthorizationGrant,
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
    pub async fn list(&self) -> Result<Vec<NatsAuthorizationGrant>, NatsAuthorizationStoreError> {
        self.store
            .call(|conn| {
                query_json_list(conn, "SELECT json FROM nats_authorizations ORDER BY rowid")
            })
            .await
            .map_err(store_error)
    }

    pub async fn list_credentials(
        &self,
    ) -> Result<Vec<CredentialGrant>, NatsAuthorizationStoreError> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter_map(|grant| match grant {
                NatsAuthorizationGrant::Credential(credential) => Some(credential),
                NatsAuthorizationGrant::Internal { .. } => None,
            })
            .collect())
    }

    /// Atomically admits one credential mutation against durable authority intent.
    pub async fn upsert_credential_at(
        &self,
        credential: &CredentialGrant,
        now_unix_seconds: u64,
    ) -> Result<CredentialUpsertStoreOutcome, NatsAuthorizationStoreError> {
        let credential = credential.clone();
        self.store
            .call(move |conn| upsert_credential_at(conn, &credential, now_unix_seconds))
            .await
            .map_err(store_error)
    }

    pub async fn remove_credential(
        &self,
        public_key: &NatsUserPublicKey,
    ) -> Result<CredentialRemoveStoreOutcome, NatsAuthorizationStoreError> {
        let authority_key = credential_authority_key(public_key);
        self.store
            .call(move |conn| remove_credential(conn, &authority_key))
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
    grants: &[NatsAuthorizationGrant],
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

fn put_authorization(
    conn: &Connection,
    user: &NatsAuthorizationGrant,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO nats_authorizations (authority_key, json) VALUES (?1, ?2)
         ON CONFLICT(authority_key) DO UPDATE SET json = excluded.json",
        params![user.authority_record_key(), to_json(user)?],
    )?;
    Ok(())
}

fn upsert_credential_at(
    conn: &mut Connection,
    requested: &CredentialGrant,
    now_unix_seconds: u64,
) -> Result<CredentialUpsertStoreOutcome, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let credentials = query_json_list::<NatsAuthorizationGrant>(
        &transaction,
        "SELECT json FROM nats_authorizations ORDER BY rowid",
    )?
    .into_iter()
    .filter_map(|grant| match grant {
        NatsAuthorizationGrant::Credential(credential) => Some(credential),
        NatsAuthorizationGrant::Internal { .. } => None,
    })
    .collect::<Vec<_>>();

    let existing = credentials
        .iter()
        .find(|credential| credential.public_key == requested.public_key);
    if let Some(existing) = existing {
        if !existing.role.has_same_authority_as(&requested.role) {
            return Ok(CredentialUpsertStoreOutcome::Rejected(
                CredentialUpsertStoreRejection::AuthorityMismatch {
                    existing: existing.role.clone(),
                    requested: requested.role.clone(),
                },
            ));
        }
        if existing == requested {
            return Ok(CredentialUpsertStoreOutcome::Unchanged);
        }
    }

    if requested.role.is_active_at(now_unix_seconds) {
        if let Some(identity) = requested.role.build_executor_identity() {
            if let Some(conflict) = credentials.iter().find(|credential| {
                credential.public_key != requested.public_key
                    && credential.role.is_active_at(now_unix_seconds)
                    && credential.role.build_executor_identity().as_ref() == Some(&identity)
            }) {
                return Ok(CredentialUpsertStoreOutcome::Rejected(
                    CredentialUpsertStoreRejection::ActiveBuildExecutorIdentity {
                        identity,
                        existing_public_key: conflict.public_key.clone(),
                    },
                ));
            }
        }
    }

    let outcome = if existing.is_some() {
        CredentialUpsertStoreOutcome::Updated
    } else {
        CredentialUpsertStoreOutcome::Added
    };
    put_authorization(
        &transaction,
        &NatsAuthorizationGrant::Credential(requested.clone()),
    )?;
    transaction.commit()?;
    Ok(outcome)
}

fn credential_authority_key(public_key: &NatsUserPublicKey) -> String {
    format!("user.{}", public_key.as_str())
}

fn remove_credential(
    conn: &mut Connection,
    authority_key: &str,
) -> Result<CredentialRemoveStoreOutcome, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let mut statement =
        transaction.prepare("SELECT json FROM nats_authorizations WHERE authority_key = ?1")?;
    let mut rows = statement.query([authority_key])?;
    let Some(row) = rows.next()? else {
        return Ok(CredentialRemoveStoreOutcome::Absent);
    };
    let json: String = row.get(0)?;
    let grant: NatsAuthorizationGrant = from_json(&json)?;
    let NatsAuthorizationGrant::Credential(credential) = grant else {
        return Ok(CredentialRemoveStoreOutcome::Absent);
    };
    drop(rows);
    drop(statement);

    let operator_count = query_json_list::<NatsAuthorizationGrant>(
        &transaction,
        "SELECT json FROM nats_authorizations",
    )?
    .into_iter()
    .filter(|grant| {
        matches!(
            grant,
            NatsAuthorizationGrant::Credential(CredentialGrant {
                role: CredentialRole::Operator,
                ..
            })
        )
    })
    .count();
    let removes_final_operator = match credential.role {
        CredentialRole::Operator => operator_count == 1,
        CredentialRole::BuildExecutor { .. } => false,
    };
    if removes_final_operator {
        return Ok(CredentialRemoveStoreOutcome::RejectedLastOperator);
    }
    transaction.execute(
        "DELETE FROM nats_authorizations WHERE authority_key = ?1",
        [authority_key],
    )?;
    transaction.commit()?;
    Ok(CredentialRemoveStoreOutcome::Removed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRemoveStoreOutcome {
    Removed,
    Absent,
    RejectedLastOperator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialUpsertStoreOutcome {
    Added,
    Updated,
    Unchanged,
    Rejected(CredentialUpsertStoreRejection),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialUpsertStoreRejection {
    #[error("credential authority cannot change from {existing:?} to {requested:?}")]
    AuthorityMismatch {
        existing: CredentialRole,
        requested: CredentialRole,
    },
    #[error(
        "active Build Executor identity {identity:?} is already granted to {existing_public_key:?}"
    )]
    ActiveBuildExecutorIdentity {
        identity: BuildExecutorIdentity,
        existing_public_key: NatsUserPublicKey,
    },
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
    use ployz_core::nats_config::{
        BuildExecutorCredentialExpiresAt, CredentialName, CredentialRole, MintedNatsUser,
        NatsInternalAuthority,
    };
    use ployz_core::ids::{BuildExecutorId, BuildPoolId};

    fn credential(name: &str) -> CredentialGrant {
        CredentialGrant {
            public_key: MintedNatsUser::generate().expect("mint user").public,
            name: CredentialName::try_new(name).expect("credential name"),
            role: CredentialRole::Operator,
        }
    }

    #[tokio::test]
    async fn upsert_lists_and_coexists_by_user_key() {
        let store = NatsAuthorizationStore::new(CoreStore::open_in_memory().await.expect("store"));
        assert!(store.list().await.expect("empty").is_empty());

        let operator = NatsAuthorizationGrant::Credential(credential("Founder"));
        let cloud = NatsAuthorizationGrant::Credential(credential("Ployz Cloud"));
        store.upsert(&operator).await.expect("operator");
        store.upsert(&cloud).await.expect("cloud");
        // Upserting the same key again replaces, not duplicates.
        store.upsert(&operator).await.expect("operator again");

        let grants = store.list().await.expect("list");
        assert_eq!(grants.len(), 2);
    }

    #[tokio::test]
    async fn list_credentials_excludes_internal_authorities() {
        let store = NatsAuthorizationStore::new(CoreStore::open_in_memory().await.expect("store"));
        let operator = credential("Founder");
        let internal = NatsAuthorizationGrant::Internal {
            authority: NatsInternalAuthority::Controller,
            public_key: MintedNatsUser::generate().expect("mint user").public,
        };
        store
            .upsert(&NatsAuthorizationGrant::Credential(operator.clone()))
            .await
            .expect("operator");
        store.upsert(&internal).await.expect("internal");

        assert_eq!(
            store.list_credentials().await.expect("credentials"),
            vec![operator]
        );
    }

    #[tokio::test]
    async fn concurrent_removes_preserve_one_operator() {
        let store = NatsAuthorizationStore::new(CoreStore::open_in_memory().await.expect("store"));
        let first = credential("First");
        let second = credential("Second");
        store
            .upsert(&NatsAuthorizationGrant::Credential(first.clone()))
            .await
            .expect("first");
        store
            .upsert(&NatsAuthorizationGrant::Credential(second.clone()))
            .await
            .expect("second");

        let first_remove = store.remove_credential(&first.public_key);
        let second_remove = store.remove_credential(&second.public_key);
        let (first_outcome, second_outcome) = tokio::join!(first_remove, second_remove);
        let outcomes = [
            first_outcome.expect("first remove"),
            second_outcome.expect("second remove"),
        ];

        assert!(matches!(
            outcomes,
            [
                CredentialRemoveStoreOutcome::Removed,
                CredentialRemoveStoreOutcome::RejectedLastOperator
            ] | [
                CredentialRemoveStoreOutcome::RejectedLastOperator,
                CredentialRemoveStoreOutcome::Removed
            ]
        ));
        assert_eq!(
            store.list_credentials().await.expect("credentials").len(),
            1
        );
    }

    #[tokio::test]
    async fn credential_upsert_renews_only_same_authority() {
        let store = NatsAuthorizationStore::new(CoreStore::open_in_memory().await.expect("store"));
        let public_key = MintedNatsUser::generate().expect("mint user").public;
        let original = build_executor_credential(public_key.clone(), "pool-a", "builder-a", 20);
        assert_eq!(
            store
                .upsert_credential_at(&original, 10)
                .await
                .expect("add"),
            CredentialUpsertStoreOutcome::Added
        );

        let renewed = build_executor_credential(public_key.clone(), "pool-a", "builder-a", 30);
        assert_eq!(
            store
                .upsert_credential_at(&renewed, 10)
                .await
                .expect("renew"),
            CredentialUpsertStoreOutcome::Updated
        );
        let reassigned = build_executor_credential(public_key, "pool-b", "builder-a", 40);
        assert!(matches!(
            store
                .upsert_credential_at(&reassigned, 10)
                .await
                .expect("reject"),
            CredentialUpsertStoreOutcome::Rejected(
                CredentialUpsertStoreRejection::AuthorityMismatch { .. }
            )
        ));
        assert_eq!(store.list_credentials().await.expect("list"), vec![renewed]);
    }

    #[tokio::test]
    async fn credential_upsert_rejects_second_active_key_but_allows_expired_history() {
        let store = NatsAuthorizationStore::new(CoreStore::open_in_memory().await.expect("store"));
        let old = build_executor_credential(
            MintedNatsUser::generate().expect("old key").public,
            "pool-a",
            "builder-a",
            20,
        );
        store.upsert_credential_at(&old, 10).await.expect("old add");
        let replacement = build_executor_credential(
            MintedNatsUser::generate().expect("replacement key").public,
            "pool-a",
            "builder-a",
            40,
        );

        assert!(matches!(
            store
                .upsert_credential_at(&replacement, 19)
                .await
                .expect("reject active duplicate"),
            CredentialUpsertStoreOutcome::Rejected(
                CredentialUpsertStoreRejection::ActiveBuildExecutorIdentity { .. }
            )
        ));
        assert_eq!(
            store
                .upsert_credential_at(&replacement, 20)
                .await
                .expect("replace expired"),
            CredentialUpsertStoreOutcome::Added
        );
        assert_eq!(store.list_credentials().await.expect("history").len(), 2);
    }

    #[tokio::test]
    async fn concurrent_credential_upserts_admit_only_one_active_identity() {
        let store = NatsAuthorizationStore::new(CoreStore::open_in_memory().await.expect("store"));
        let first = build_executor_credential(
            MintedNatsUser::generate().expect("first key").public,
            "pool-a",
            "builder-a",
            30,
        );
        let second = build_executor_credential(
            MintedNatsUser::generate().expect("second key").public,
            "pool-a",
            "builder-a",
            30,
        );

        let outcomes = tokio::join!(
            store.upsert_credential_at(&first, 10),
            store.upsert_credential_at(&second, 10),
        );
        let admitted = [outcomes.0.expect("first"), outcomes.1.expect("second")]
            .into_iter()
            .filter(|outcome| matches!(outcome, CredentialUpsertStoreOutcome::Added))
            .count();

        assert_eq!(admitted, 1);
        assert_eq!(store.list_credentials().await.expect("list").len(), 1);
    }

    fn build_executor_credential(
        public_key: NatsUserPublicKey,
        pool: &str,
        executor: &str,
        expires_at: u64,
    ) -> CredentialGrant {
        CredentialGrant {
            public_key,
            name: CredentialName::try_new("External builder").expect("name"),
            role: CredentialRole::BuildExecutor {
                pool_id: BuildPoolId::try_new(pool).expect("pool id"),
                executor_id: BuildExecutorId::try_new(executor).expect("executor id"),
                expires_at: BuildExecutorCredentialExpiresAt::try_new(expires_at).expect("expiry"),
            },
        }
    }
}
