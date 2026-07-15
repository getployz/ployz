use std::fmt::Write;

use ployz_core::cert::{
    LeaseBearerToken, ManagedLeaseAcquisitionId, ManagedLeaseAddressSet, ManagedLeaseRecord,
};
use ployz_core::ingress::{IngressEndpointProjectionIdentity, PloyzDnsTargetIntent};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::control::store::{CoreStore, CoreStoreError, query_json, to_json};

const SINGLETON_ID: &str = "1";

pub use ployz_core::cert::ManagedLeaseAddressSet as ManagedDnsEndpointSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PloyzDnsTargetAllocation {
    Unacquired {
        acquisition_id: ManagedLeaseAcquisitionId,
        token: LeaseBearerToken,
    },
    Allocated {
        lease: ManagedLeaseRecord,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedDnsCheckpoint {
    Applied {
        last_applied_identity: Option<IngressEndpointProjectionIdentity>,
        last_applied_endpoints: ManagedDnsEndpointSet,
    },
    Withdrawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PloyzDnsTargetWrite {
    Stored,
    Superseded,
}

#[derive(Debug, Clone)]
pub struct PloyzDnsTargetStore {
    store: CoreStore,
}

impl PloyzDnsTargetStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub async fn load_allocation(
        &self,
    ) -> Result<Option<PloyzDnsTargetAllocation>, CoreStoreError> {
        self.store
            .call(|conn| {
                query_json(
                    conn,
                    "SELECT json FROM ployz_dns_target_allocation WHERE id = ?1",
                    SINGLETON_ID,
                )
            })
            .await
    }

    pub async fn load_state(
        &self,
    ) -> Result<
        (
            Option<PloyzDnsTargetAllocation>,
            Option<ManagedDnsCheckpoint>,
        ),
        CoreStoreError,
    > {
        self.store
            .call(|conn| {
                Ok((
                    load_target_allocation(conn)?,
                    query_json(
                        conn,
                        "SELECT json FROM managed_dns_checkpoint WHERE id = ?1",
                        SINGLETON_ID,
                    )?,
                ))
            })
            .await
    }

    pub async fn load_checkpoint(&self) -> Result<Option<ManagedDnsCheckpoint>, CoreStoreError> {
        self.store
            .call(|conn| {
                query_json(
                    conn,
                    "SELECT json FROM managed_dns_checkpoint WHERE id = ?1",
                    SINGLETON_ID,
                )
            })
            .await
    }

    pub async fn ensure_acquisition(
        &self,
    ) -> Result<Option<PloyzDnsTargetAllocation>, CoreStoreError> {
        self.store.call(ensure_acquisition).await
    }

    pub async fn store_acquired(
        &self,
        acquisition_id: ManagedLeaseAcquisitionId,
        lease: ManagedLeaseRecord,
    ) -> Result<PloyzDnsTargetWrite, CoreStoreError> {
        self.store
            .call(move |conn| store_acquired(conn, &acquisition_id, lease))
            .await
    }

    pub async fn store_successful_reconcile(
        &self,
        lease: ManagedLeaseRecord,
        checkpoint: ManagedDnsCheckpoint,
    ) -> Result<PloyzDnsTargetWrite, CoreStoreError> {
        self.store
            .call(move |conn| store_successful_reconcile(conn, lease, &checkpoint))
            .await
    }

    pub async fn store_successful_withdraw(
        &self,
        lease: ManagedLeaseRecord,
    ) -> Result<PloyzDnsTargetWrite, CoreStoreError> {
        self.store
            .call(move |conn| store_successful_withdraw(conn, lease))
            .await
    }
}

fn ensure_acquisition(
    conn: &mut Connection,
) -> Result<Option<PloyzDnsTargetAllocation>, rusqlite::Error> {
    let transaction = conn.transaction()?;
    if load_target_intent(&transaction)? != Some(PloyzDnsTargetIntent::Enabled) {
        return Ok(None);
    }
    if let Some(allocation) = load_target_allocation(&transaction)? {
        return Ok(Some(allocation));
    }
    let allocation = PloyzDnsTargetAllocation::Unacquired {
        acquisition_id: ManagedLeaseAcquisitionId::try_new(random_hex::<16>()?)
            .map_err(to_sql_error)?,
        token: LeaseBearerToken::try_new(random_hex::<32>()?).map_err(to_sql_error)?,
    };
    transaction.execute(
        "INSERT INTO ployz_dns_target_allocation (id, json) VALUES (1, ?1)",
        [to_json(&allocation)?],
    )?;
    transaction.commit()?;
    Ok(Some(allocation))
}

fn store_acquired(
    conn: &mut Connection,
    acquisition_id: &ManagedLeaseAcquisitionId,
    lease: ManagedLeaseRecord,
) -> Result<PloyzDnsTargetWrite, rusqlite::Error> {
    let transaction = conn.transaction()?;
    if load_target_intent(&transaction)? != Some(PloyzDnsTargetIntent::Enabled) {
        return Ok(PloyzDnsTargetWrite::Superseded);
    }
    let Some(PloyzDnsTargetAllocation::Unacquired {
        acquisition_id: current,
        token,
    }) = load_target_allocation(&transaction)?
    else {
        return Ok(PloyzDnsTargetWrite::Superseded);
    };
    if current != *acquisition_id || token != lease.token {
        return Ok(PloyzDnsTargetWrite::Superseded);
    }
    let allocation = PloyzDnsTargetAllocation::Allocated { lease };
    transaction.execute(
        "UPDATE ployz_dns_target_allocation SET json = ?1 WHERE id = 1",
        [to_json(&allocation)?],
    )?;
    transaction.execute(
        "INSERT INTO managed_dns_checkpoint (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        [to_json(&ManagedDnsCheckpoint::Applied {
            last_applied_identity: None,
            last_applied_endpoints: ManagedLeaseAddressSet::new(Vec::new(), Vec::new()),
        })?],
    )?;
    transaction.commit()?;
    Ok(PloyzDnsTargetWrite::Stored)
}

fn store_successful_reconcile(
    conn: &mut Connection,
    lease: ManagedLeaseRecord,
    checkpoint: &ManagedDnsCheckpoint,
) -> Result<PloyzDnsTargetWrite, rusqlite::Error> {
    let transaction = conn.transaction()?;
    if load_target_intent(&transaction)? != Some(PloyzDnsTargetIntent::Enabled) {
        return Ok(PloyzDnsTargetWrite::Superseded);
    }
    if matches!(checkpoint, ManagedDnsCheckpoint::Withdrawn) {
        return Ok(PloyzDnsTargetWrite::Superseded);
    }
    let Some(PloyzDnsTargetAllocation::Allocated { lease: current }) =
        load_target_allocation(&transaction)?
    else {
        return Ok(PloyzDnsTargetWrite::Superseded);
    };
    if current.name != lease.name || current.token != lease.token {
        return Ok(PloyzDnsTargetWrite::Superseded);
    }
    transaction.execute(
        "UPDATE ployz_dns_target_allocation SET json = ?1 WHERE id = 1",
        [to_json(&PloyzDnsTargetAllocation::Allocated { lease })?],
    )?;
    transaction.execute(
        "INSERT INTO managed_dns_checkpoint (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        [to_json(checkpoint)?],
    )?;
    transaction.commit()?;
    Ok(PloyzDnsTargetWrite::Stored)
}

fn store_successful_withdraw(
    conn: &mut Connection,
    lease: ManagedLeaseRecord,
) -> Result<PloyzDnsTargetWrite, rusqlite::Error> {
    let transaction = conn.transaction()?;
    if load_target_intent(&transaction)? != Some(PloyzDnsTargetIntent::Disabled) {
        return Ok(PloyzDnsTargetWrite::Superseded);
    }
    let Some(PloyzDnsTargetAllocation::Allocated { lease: current }) =
        load_target_allocation(&transaction)?
    else {
        return Ok(PloyzDnsTargetWrite::Superseded);
    };
    if current.name != lease.name || current.token != lease.token {
        return Ok(PloyzDnsTargetWrite::Superseded);
    }
    transaction.execute(
        "UPDATE ployz_dns_target_allocation SET json = ?1 WHERE id = 1",
        [to_json(&PloyzDnsTargetAllocation::Allocated { lease })?],
    )?;
    transaction.execute(
        "INSERT INTO managed_dns_checkpoint (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        [to_json(&ManagedDnsCheckpoint::Withdrawn)?],
    )?;
    transaction.commit()?;
    Ok(PloyzDnsTargetWrite::Stored)
}

fn load_target_intent(conn: &Connection) -> Result<Option<PloyzDnsTargetIntent>, rusqlite::Error> {
    query_json(
        conn,
        "SELECT json FROM ployz_dns_target_intent WHERE id = ?1",
        SINGLETON_ID,
    )
}

fn load_target_allocation(
    conn: &Connection,
) -> Result<Option<PloyzDnsTargetAllocation>, rusqlite::Error> {
    query_json(
        conn,
        "SELECT json FROM ployz_dns_target_allocation WHERE id = ?1",
        SINGLETON_ID,
    )
}

fn random_hex<const N: usize>() -> Result<String, rusqlite::Error> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(to_sql_error)?;
    let mut value = String::with_capacity(N * 2);
    for byte in bytes {
        write!(value, "{byte:02x}").map_err(to_sql_error)?;
    }
    Ok(value)
}

fn to_sql_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ingress::{AutomaticHostnameConfiguration, IngressConfiguration};

    use crate::control::intent::ingress_intent::IngressIntentStore;

    #[tokio::test]
    async fn acquired_target_starts_with_an_empty_checkpoint() {
        let core = CoreStore::open_in_memory().await.expect("store");
        IngressIntentStore::new(core.clone())
            .replace(
                IngressConfiguration::try_new(
                    AutomaticHostnameConfiguration::Disabled,
                    PloyzDnsTargetIntent::Enabled,
                )
                .expect("valid ingress configuration"),
            )
            .await
            .expect("enable target");
        let store = PloyzDnsTargetStore::new(core);
        let Some(PloyzDnsTargetAllocation::Unacquired {
            acquisition_id,
            token,
        }) = store.ensure_acquisition().await.expect("acquisition")
        else {
            panic!("unacquired target");
        };
        let lease = ManagedLeaseRecord::try_new(
            ployz_core::cert::ManagedLeaseName::try_new("cluster-one").expect("name"),
            token,
            ployz_core::cert::LeaseIssuedAt::try_new(1_000).expect("issued"),
            ployz_core::cert::LeaseExpiresAt::try_new(2_000).expect("expires"),
        )
        .expect("lease");

        store
            .store_acquired(acquisition_id, lease)
            .await
            .expect("store acquired");

        assert_eq!(
            store.load_checkpoint().await.expect("checkpoint"),
            Some(ManagedDnsCheckpoint::Applied {
                last_applied_identity: None,
                last_applied_endpoints: ManagedLeaseAddressSet::new(Vec::new(), Vec::new()),
            })
        );
    }
}
