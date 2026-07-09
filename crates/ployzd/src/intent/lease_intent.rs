use crate::core_store::{CoreStore, CoreStoreError, query_json, to_json};
use ployz_core::cert::{ManagedCertBundle, ManagedLeaseIntent, ManagedLeaseRecord, PublicUrlMode};
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
            .call(move |conn| {
                let mut intent = load_intent(conn)?;
                intent.mode = mode;
                if !matches!(mode, PublicUrlMode::Auto) {
                    intent.lease = None;
                    intent.bundle = None;
                }
                replace(conn, &intent)
            })
            .await
            .map_err(store_error)
    }

    pub async fn store_lease(
        &self,
        record: ManagedLeaseRecord,
        bundle: ManagedCertBundle,
    ) -> Result<StoreLeaseOutcome, LeaseIntentStoreError> {
        self.store
            .call(move |conn| {
                let mut intent = load_intent(conn)?;
                if !matches!(intent.mode, PublicUrlMode::Auto) {
                    return Ok(StoreLeaseOutcome::Superseded);
                }
                intent.lease = Some(record);
                intent.bundle = Some(bundle);
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
                let mut intent = load_intent(conn)?;
                intent.lease = Some(record);
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
    use ployz_core::cert::{
        DEFAULT_MANAGED_LEASE_TTL_SECONDS, LeaseBearerToken, LeaseExpiresAt, LeaseIssuedAt,
        ManagedLeaseName,
    };

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
            LeaseExpiresAt::try_new(issued_at.unix_seconds() + DEFAULT_MANAGED_LEASE_TTL_SECONDS)
                .expect("expires at");
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
            .store_lease(record.clone(), bundle.clone())
            .await
            .expect("store lease");

        store.set_mode(PublicUrlMode::None).await.expect("set mode");
        let intent = store.load().await.expect("load intent");

        assert_eq!(intent, ManagedLeaseIntent::empty(PublicUrlMode::None));

        assert_eq!(
            store
                .store_lease(record, bundle)
                .await
                .expect("superseded lease result"),
            StoreLeaseOutcome::Superseded
        );
        assert_eq!(
            store.load().await.expect("load cleared intent"),
            ManagedLeaseIntent::empty(PublicUrlMode::None)
        );
    }
}
