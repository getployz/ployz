use crate::core_store::{CoreStore, CoreStoreError, query_json, to_json};
use ployz_core::cert::{
    AutoLeaseState, ManagedCertBundle, ManagedLeaseIntent, ManagedLeaseRecord, PublicUrlMode,
};
use rusqlite::{Connection, params};

#[derive(Debug, Clone)]
pub struct LeaseIntentStore {
    store: CoreStore,
}

impl LeaseIntentStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub async fn set_mode(&self, mode: PublicUrlMode) -> Result<(), LeaseIntentStoreError> {
        self.store
            .call(move |conn| replace(conn, &ManagedLeaseIntent::empty(mode)))
            .await
            .map_err(store_error)
    }

    pub async fn set_mode_if_unconfigured(
        &self,
        mode: PublicUrlMode,
    ) -> Result<(), LeaseIntentStoreError> {
        self.store
            .call(move |conn| {
                let configured: Option<ManagedLeaseIntent> = query_json(
                    conn,
                    "SELECT json FROM managed_lease_intent WHERE id = ?1",
                    "1",
                )?;
                if configured.is_none() {
                    replace(conn, &ManagedLeaseIntent::empty(mode))?;
                }
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn store_lease(
        &self,
        record: ManagedLeaseRecord,
        bundle: Option<ManagedCertBundle>,
    ) -> Result<StoreLeaseOutcome, LeaseIntentStoreError> {
        if let Some(bundle) = &bundle
            && bundle.lease != record.name
        {
            return Err(LeaseIntentStoreError {
                message: format!(
                    "certificate bundle lease {} does not match record lease {}",
                    bundle.lease.as_str(),
                    record.name.as_str()
                ),
            });
        }
        self.store
            .call(move |conn| {
                let intent = load_intent(conn)?;
                let ManagedLeaseIntent::Auto { state } = intent else {
                    return Ok(StoreLeaseOutcome::Superseded);
                };
                let state = match (bundle, *state) {
                    (
                        Some(bundle),
                        AutoLeaseState::Unacquired
                        | AutoLeaseState::RecordOnly { .. }
                        | AutoLeaseState::Ready { .. },
                    ) => AutoLeaseState::Ready {
                        lease: record,
                        bundle,
                    },
                    (None, AutoLeaseState::Ready { lease, bundle })
                        if lease.name == record.name
                            && bundle.lease == record.name
                            && bundle.issued_at.unix_seconds()
                                <= record.issued_at.unix_seconds()
                            && record.issued_at.unix_seconds()
                                < bundle.expires_at.unix_seconds() =>
                    {
                        AutoLeaseState::Ready {
                            lease: record,
                            bundle,
                        }
                    }
                    (
                        None,
                        AutoLeaseState::Unacquired
                        | AutoLeaseState::RecordOnly { .. }
                        | AutoLeaseState::Ready { .. },
                    ) => AutoLeaseState::RecordOnly { lease: record },
                };
                let intent = ManagedLeaseIntent::Auto {
                    state: Box::new(state),
                };
                replace(conn, &intent)?;
                Ok(StoreLeaseOutcome::Stored)
            })
            .await
            .map_err(store_error)
    }

    pub async fn restore_lease_record(
        &self,
        record: ManagedLeaseRecord,
    ) -> Result<(), LeaseIntentStoreError> {
        self.store
            .call(move |conn| {
                let intent = ManagedLeaseIntent::Auto {
                    state: Box::new(AutoLeaseState::RecordOnly { lease: record }),
                };
                replace(conn, &intent)
            })
            .await
            .map_err(store_error)
    }

    pub async fn load(&self) -> Result<ManagedLeaseIntent, LeaseIntentStoreError> {
        self.store.call(load_intent).await.map_err(store_error)
    }

    pub async fn load_if_configured(
        &self,
    ) -> Result<Option<ManagedLeaseIntent>, LeaseIntentStoreError> {
        self.store
            .call(|conn| {
                query_json(
                    conn,
                    "SELECT json FROM managed_lease_intent WHERE id = ?1",
                    "1",
                )
            })
            .await
            .map_err(store_error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreLeaseOutcome {
    Stored,
    Superseded,
}

fn load_intent(conn: &mut Connection) -> Result<ManagedLeaseIntent, rusqlite::Error> {
    Ok(query_json(
        conn,
        "SELECT json FROM managed_lease_intent WHERE id = ?1",
        "1",
    )?
    .unwrap_or_else(|| ManagedLeaseIntent::empty(PublicUrlMode::Auto)))
}

fn replace(conn: &Connection, intent: &ManagedLeaseIntent) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO managed_lease_intent (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        params![to_json(intent)?],
    )?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("managed lease intent store: {message}")]
pub struct LeaseIntentStoreError {
    message: String,
}

fn store_error(error: CoreStoreError) -> LeaseIntentStoreError {
    LeaseIntentStoreError {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::cert::{LeaseBearerToken, LeaseExpiresAt, LeaseIssuedAt, ManagedLeaseName};

    #[tokio::test]
    async fn absent_row_loads_auto_without_lease() {
        let store = LeaseIntentStore::new(CoreStore::open_in_memory().await.expect("core store"));

        let intent = store.load().await.expect("load default intent");

        assert_eq!(intent, ManagedLeaseIntent::empty(PublicUrlMode::Auto));
        assert!(
            store
                .load_if_configured()
                .await
                .expect("load configured intent")
                .is_none()
        );
    }

    #[tokio::test]
    async fn non_auto_mode_clears_managed_lease_evidence() {
        let store = LeaseIntentStore::new(CoreStore::open_in_memory().await.expect("core store"));
        let name = ManagedLeaseName::try_new("cluster-one").expect("lease name");
        let issued_at = LeaseIssuedAt::try_new(1_700_000_000).expect("issued at");
        let expires_at =
            LeaseExpiresAt::try_new(issued_at.unix_seconds() + 60).expect("expires at");
        let record = ManagedLeaseRecord::try_new(
            name.clone(),
            LeaseBearerToken::try_new("lease-token").expect("token"),
            issued_at,
            expires_at,
        )
        .expect("record");
        let bundle = ManagedCertBundle::try_new(
            name.clone(),
            name.wildcard_and_apex(),
            "certificate".to_owned(),
            "private-key".to_owned(),
            issued_at,
            expires_at,
        )
        .expect("bundle");
        store
            .store_lease(record.clone(), Some(bundle.clone()))
            .await
            .expect("store lease");

        store.set_mode(PublicUrlMode::None).await.expect("set mode");
        let intent = store.load().await.expect("load intent");

        assert_eq!(intent, ManagedLeaseIntent::empty(PublicUrlMode::None));

        assert_eq!(
            store
                .store_lease(record, Some(bundle))
                .await
                .expect("superseded lease result"),
            StoreLeaseOutcome::Superseded
        );
        assert_eq!(
            store.load().await.expect("load cleared intent"),
            ManagedLeaseIntent::empty(PublicUrlMode::None)
        );
    }

    #[tokio::test]
    async fn pending_bundle_stores_record_only() {
        let store = LeaseIntentStore::new(CoreStore::open_in_memory().await.expect("core store"));
        let record = ManagedLeaseRecord::try_new(
            ManagedLeaseName::try_new("cluster-one").expect("lease name"),
            LeaseBearerToken::try_new("lease-token").expect("token"),
            LeaseIssuedAt::try_new(1_700_000_000).expect("issued at"),
            LeaseExpiresAt::try_new(1_700_000_060).expect("expires at"),
        )
        .expect("record");

        store
            .store_lease(record.clone(), None)
            .await
            .expect("store lease");

        assert_eq!(
            store.load().await.expect("load intent"),
            ManagedLeaseIntent::Auto {
                state: Box::new(AutoLeaseState::RecordOnly { lease: record })
            }
        );
    }

    #[tokio::test]
    async fn pending_renewal_preserves_ready_bundle() {
        let store = LeaseIntentStore::new(CoreStore::open_in_memory().await.expect("core store"));
        let name = ManagedLeaseName::try_new("cluster-one").expect("lease name");
        let issued_at = LeaseIssuedAt::try_new(1_700_000_000).expect("issued at");
        let expires_at = LeaseExpiresAt::try_new(1_700_000_060).expect("expires at");
        let record = ManagedLeaseRecord::try_new(
            name.clone(),
            LeaseBearerToken::try_new("lease-token").expect("token"),
            issued_at,
            expires_at,
        )
        .expect("record");
        let bundle = ManagedCertBundle::try_new(
            name.clone(),
            name.wildcard_and_apex(),
            "certificate".to_owned(),
            "private-key".to_owned(),
            issued_at,
            expires_at,
        )
        .expect("bundle");
        store
            .store_lease(record, Some(bundle.clone()))
            .await
            .expect("store ready lease");
        let renewed = ManagedLeaseRecord::try_new(
            name,
            LeaseBearerToken::try_new("lease-token").expect("token"),
            LeaseIssuedAt::try_new(1_700_000_030).expect("renewed issued at"),
            LeaseExpiresAt::try_new(1_700_000_090).expect("renewed expires at"),
        )
        .expect("renewed record");

        store
            .store_lease(renewed.clone(), None)
            .await
            .expect("store pending renewal");

        assert_eq!(
            store.load().await.expect("load intent"),
            ManagedLeaseIntent::Auto {
                state: Box::new(AutoLeaseState::Ready {
                    lease: renewed,
                    bundle,
                })
            }
        );
    }

    #[tokio::test]
    async fn pending_renewal_drops_bundle_expired_at_renewal_issue_time() {
        let store = LeaseIntentStore::new(CoreStore::open_in_memory().await.expect("core store"));
        let name = ManagedLeaseName::try_new("cluster-one").expect("lease name");
        let issued_at = LeaseIssuedAt::try_new(1_700_000_000).expect("issued at");
        let expires_at = LeaseExpiresAt::try_new(1_700_000_060).expect("expires at");
        let record = ManagedLeaseRecord::try_new(
            name.clone(),
            LeaseBearerToken::try_new("lease-token").expect("token"),
            issued_at,
            expires_at,
        )
        .expect("record");
        let bundle = ManagedCertBundle::try_new(
            name.clone(),
            name.wildcard_and_apex(),
            "certificate".to_owned(),
            "private-key".to_owned(),
            issued_at,
            expires_at,
        )
        .expect("bundle");
        store
            .store_lease(record, Some(bundle))
            .await
            .expect("store ready lease");
        let renewed = ManagedLeaseRecord::try_new(
            name,
            LeaseBearerToken::try_new("lease-token").expect("renewed token"),
            LeaseIssuedAt::try_new(1_700_000_060).expect("renewed issued at"),
            LeaseExpiresAt::try_new(1_700_000_120).expect("renewed expires at"),
        )
        .expect("renewed record");

        store
            .store_lease(renewed.clone(), None)
            .await
            .expect("store pending renewal");

        assert_eq!(
            store.load().await.expect("load intent"),
            ManagedLeaseIntent::Auto {
                state: Box::new(AutoLeaseState::RecordOnly { lease: renewed })
            }
        );
    }

    #[tokio::test]
    async fn bundle_for_different_lease_is_rejected_without_storing_invalid_ready_state() {
        let store = LeaseIntentStore::new(CoreStore::open_in_memory().await.expect("core store"));
        let record_name = ManagedLeaseName::try_new("cluster-one").expect("record lease name");
        let bundle_name = ManagedLeaseName::try_new("cluster-two").expect("bundle lease name");
        let issued_at = LeaseIssuedAt::try_new(1_700_000_000).expect("issued at");
        let expires_at = LeaseExpiresAt::try_new(1_700_000_060).expect("expires at");
        let record = ManagedLeaseRecord::try_new(
            record_name,
            LeaseBearerToken::try_new("lease-token").expect("token"),
            issued_at,
            expires_at,
        )
        .expect("record");
        let bundle = ManagedCertBundle::try_new(
            bundle_name.clone(),
            bundle_name.wildcard_and_apex(),
            "certificate".to_owned(),
            "private-key".to_owned(),
            issued_at,
            expires_at,
        )
        .expect("bundle");

        let result = store.store_lease(record, Some(bundle)).await;

        assert!(result.is_err());
        assert_eq!(
            store.load().await.expect("load intent"),
            ManagedLeaseIntent::Auto {
                state: Box::new(AutoLeaseState::Unacquired)
            }
        );
    }
}
