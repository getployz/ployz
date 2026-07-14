//! Core-local ingress intent and private projection evidence stores.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::net::{Ipv4Addr, Ipv6Addr};

use ployz_core::cert::{LeaseBearerToken, ManagedLeaseAcquisitionId, ManagedLeaseRecord};
pub use ployz_core::ingress::IngressConfiguration;
use ployz_core::ingress::{
    ActiveCertificateMetadata, CertificateOwner, IngressEndpointProjectionIdentity,
    IngressEndpointSet, PloyzDnsTargetIntent,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::core_store::{
    CoreStore, CoreStoreError, from_json, query_json, query_json_list, to_json,
};
use crate::ingress_endpoint::ProjectionEvidenceRecord;

const SINGLETON_ID: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressConfigurationWrite {
    Stored,
    Unchanged,
}

#[derive(Debug, Clone)]
pub struct IngressIntentStore {
    store: CoreStore,
}

impl IngressIntentStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub async fn load(&self) -> Result<Option<IngressConfiguration>, IngressIntentStoreError> {
        self.store
            .call(|conn| load_configuration(conn))
            .await
            .map_err(IngressIntentStoreError::Store)
    }

    pub async fn replace(
        &self,
        configuration: IngressConfiguration,
    ) -> Result<IngressConfigurationWrite, IngressIntentStoreError> {
        let outcome = self
            .store
            .call(move |conn| replace_configuration(conn, &configuration))
            .await
            .map_err(IngressIntentStoreError::Store)?;
        match outcome {
            ReplaceConfigurationOutcome::Written(write) => Ok(write),
            ReplaceConfigurationOutcome::Invalid { message } => {
                Err(IngressIntentStoreError::InvalidConfiguration { message })
            }
        }
    }

    pub async fn validate_replace(
        &self,
        configuration: &IngressConfiguration,
    ) -> Result<(), IngressIntentStoreError> {
        let configuration = configuration.clone();
        let rejection = self
            .store
            .call(move |conn| replace_rejection(conn, &configuration))
            .await
            .map_err(IngressIntentStoreError::Store)?;
        match rejection {
            Some(message) => Err(IngressIntentStoreError::InvalidConfiguration { message }),
            None => Ok(()),
        }
    }
}

enum ReplaceConfigurationOutcome {
    Written(IngressConfigurationWrite),
    Invalid { message: String },
}

fn replace_configuration(
    conn: &mut Connection,
    configuration: &IngressConfiguration,
) -> Result<ReplaceConfigurationOutcome, rusqlite::Error> {
    if let Some(message) = replace_rejection(conn, configuration)? {
        return Ok(ReplaceConfigurationOutcome::Invalid { message });
    }
    let transaction = conn.transaction()?;
    let current = load_configuration(&transaction)?;
    if current.as_ref() == Some(configuration) {
        return Ok(ReplaceConfigurationOutcome::Written(
            IngressConfigurationWrite::Unchanged,
        ));
    }
    transaction.execute(
        "INSERT INTO automatic_hostname_intent (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        [to_json(&configuration.automatic_hostnames)?],
    )?;
    transaction.execute(
        "INSERT INTO ployz_dns_target_intent (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        [to_json(&configuration.ployz_dns_target)?],
    )?;
    transaction.commit()?;
    Ok(ReplaceConfigurationOutcome::Written(
        IngressConfigurationWrite::Stored,
    ))
}

fn replace_rejection(
    conn: &Connection,
    configuration: &IngressConfiguration,
) -> Result<Option<String>, rusqlite::Error> {
    if let Some(message) = invalid_configuration(configuration) {
        return Ok(Some(message));
    }
    let current = load_configuration(conn)?;
    if current
        .as_ref()
        .is_some_and(|current| current.automatic_hostnames != configuration.automatic_hostnames)
        && has_automatic_route_bindings(conn)?
    {
        return Ok(Some("automatic route bindings exist".to_owned()));
    }
    Ok(None)
}

fn invalid_configuration(configuration: &IngressConfiguration) -> Option<String> {
    configuration
        .validate()
        .err()
        .map(|error| error.to_string())
}

fn load_configuration(conn: &Connection) -> Result<Option<IngressConfiguration>, rusqlite::Error> {
    let automatic_hostnames = query_json(
        conn,
        "SELECT json FROM automatic_hostname_intent WHERE id = ?1",
        SINGLETON_ID,
    )?;
    let ployz_dns_target = query_json(
        conn,
        "SELECT json FROM ployz_dns_target_intent WHERE id = ?1",
        SINGLETON_ID,
    )?;
    match (automatic_hostnames, ployz_dns_target) {
        (None, None) => Ok(None),
        (Some(automatic_hostnames), Some(ployz_dns_target)) => Ok(Some(IngressConfiguration {
            automatic_hostnames,
            ployz_dns_target,
        })),
        (Some(_), None) | (None, Some(_)) => Err(rusqlite::Error::InvalidQuery),
    }
}

fn has_automatic_route_bindings(conn: &Connection) -> Result<bool, rusqlite::Error> {
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM route_bindings
            WHERE json_extract(json, '$.origin') = 'automatic'
        )",
        [],
        |row| row.get(0),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum IngressIntentStoreError {
    #[error("ingress intent store: {0}")]
    Store(CoreStoreError),
    #[error("invalid ingress configuration: {message}")]
    InvalidConfiguration { message: String },
}

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
#[serde(deny_unknown_fields)]
pub struct ManagedDnsEndpointSet {
    ipv4: BTreeSet<Ipv4Addr>,
    ipv6: BTreeSet<Ipv6Addr>,
}

impl ManagedDnsEndpointSet {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            ipv4: BTreeSet::new(),
            ipv6: BTreeSet::new(),
        }
    }

    #[must_use]
    pub const fn ipv4(&self) -> &BTreeSet<Ipv4Addr> {
        &self.ipv4
    }

    #[must_use]
    pub const fn ipv6(&self) -> &BTreeSet<Ipv6Addr> {
        &self.ipv6
    }
}

impl From<&IngressEndpointSet> for ManagedDnsEndpointSet {
    fn from(value: &IngressEndpointSet) -> Self {
        Self {
            ipv4: value.ipv4().clone(),
            ipv6: value.ipv6().clone(),
        }
    }
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

    pub async fn load_publishable_gateway_ids(
        &self,
    ) -> Result<Vec<ployz_core::ids::MachineId>, CoreStoreError> {
        self.store
            .call(|conn| {
                let record: Option<ProjectionEvidenceRecord> = query_json(
                    conn,
                    "SELECT json FROM ingress_endpoint_projection WHERE id = ?1",
                    SINGLETON_ID,
                )?;
                Ok(record
                    .map(|record| record.publishable_gateway_ids)
                    .unwrap_or_default())
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
            last_applied_endpoints: ManagedDnsEndpointSet::empty(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressProjectionWrite {
    Stored,
    Unchanged,
    Conflict {
        current: Option<IngressEndpointProjectionIdentity>,
    },
}

#[derive(Debug, Clone)]
pub struct IngressProjectionStore {
    store: CoreStore,
}

impl IngressProjectionStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub(crate) async fn load(&self) -> Result<Option<ProjectionEvidenceRecord>, CoreStoreError> {
        self.store
            .call(|conn| {
                query_json(
                    conn,
                    "SELECT json FROM ingress_endpoint_projection WHERE id = ?1",
                    SINGLETON_ID,
                )
            })
            .await
    }

    pub(crate) async fn compare_and_replace(
        &self,
        expected: Option<IngressEndpointProjectionIdentity>,
        next: ProjectionEvidenceRecord,
    ) -> Result<IngressProjectionWrite, CoreStoreError> {
        self.store
            .call(move |conn| compare_and_replace_projection(conn, expected, &next))
            .await
    }
}

fn compare_and_replace_projection(
    conn: &mut Connection,
    expected: Option<IngressEndpointProjectionIdentity>,
    next: &ProjectionEvidenceRecord,
) -> Result<IngressProjectionWrite, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let current: Option<ProjectionEvidenceRecord> = transaction
        .query_row(
            "SELECT json FROM ingress_endpoint_projection WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|json| from_json(&json))
        .transpose()?;
    let current_identity = current.as_ref().map(|record| record.projection.identity());
    if current_identity != expected {
        return Ok(IngressProjectionWrite::Conflict {
            current: current_identity,
        });
    }
    if current.as_ref() == Some(next) {
        return Ok(IngressProjectionWrite::Unchanged);
    }
    transaction.execute(
        "INSERT INTO ingress_endpoint_projection (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        [to_json(next)?],
    )?;
    transaction.commit()?;
    Ok(IngressProjectionWrite::Stored)
}

#[derive(Debug, Clone)]
pub struct ActiveCertificateMetadataStore {
    store: CoreStore,
}

impl ActiveCertificateMetadataStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub async fn active_for_owner(
        &self,
        owner: &CertificateOwner,
    ) -> Result<Option<ActiveCertificateMetadata>, CoreStoreError> {
        let owner_key = certificate_owner_key(owner);
        self.store
            .call(move |conn| {
                query_json::<ActiveCertificateMetadata>(
                    conn,
                    "SELECT json FROM active_certificate_metadata WHERE owner_key = ?1",
                    &owner_key,
                )
            })
            .await
    }

    pub async fn active_certificates(
        &self,
    ) -> Result<Vec<ActiveCertificateMetadata>, CoreStoreError> {
        self.store
            .call(|conn| {
                query_json_list(
                    conn,
                    "SELECT json FROM active_certificate_metadata ORDER BY owner_key",
                )
            })
            .await
    }

    pub async fn replace(
        &self,
        certificate: ActiveCertificateMetadata,
    ) -> Result<(), CoreStoreError> {
        let owner_key = certificate_owner_key(&certificate.owner);
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO active_certificate_metadata (owner_key, json) VALUES (?1, ?2)
                     ON CONFLICT(owner_key) DO UPDATE SET json = excluded.json",
                    params![owner_key, to_json(&certificate)?],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn remove(&self, owner: &CertificateOwner) -> Result<(), CoreStoreError> {
        let owner_key = certificate_owner_key(owner);
        self.store
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM active_certificate_metadata WHERE owner_key = ?1",
                    [owner_key],
                )?;
                Ok(())
            })
            .await
    }
}

fn certificate_owner_key(owner: &CertificateOwner) -> String {
    match owner {
        CertificateOwner::PloyzAutomaticNamespace => "ployz-automatic-namespace".to_owned(),
        CertificateOwner::RouteBinding { route_binding_id } => {
            format!("route-binding:{}", route_binding_id.as_str())
        }
    }
}

pub(crate) fn upsert_active_certificate_metadata(
    transaction: &rusqlite::Transaction<'_>,
    certificate: &ActiveCertificateMetadata,
) -> Result<(), rusqlite::Error> {
    let owner_key = certificate_owner_key(&certificate.owner);
    transaction.execute(
        "INSERT INTO active_certificate_metadata (owner_key, json) VALUES (?1, ?2)
         ON CONFLICT(owner_key) DO UPDATE SET json = excluded.json",
        params![owner_key, to_json(certificate)?],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::cert::{CertBundleRef, CertValidAt, CertValidityWindow};
    use ployz_core::ids::RouteBindingId;
    use ployz_core::ingress::{
        AutomaticHostnameConfiguration, IngressEndpointProjection, IngressEndpointProjectionState,
    };
    use ployz_core::state::ControlPlaneEpoch;
    use ployz_test_support::ids::{cert_id, route_hostname};

    fn configuration(
        automatic_hostnames: AutomaticHostnameConfiguration,
        ployz_dns_target: PloyzDnsTargetIntent,
    ) -> IngressConfiguration {
        IngressConfiguration {
            automatic_hostnames,
            ployz_dns_target,
        }
    }

    fn projection(revision: u64) -> IngressEndpointProjection {
        IngressEndpointProjection {
            control_plane_epoch: ControlPlaneEpoch::initial(),
            revision,
            state: IngressEndpointProjectionState::Pending,
        }
    }

    fn projection_record(revision: u64) -> ProjectionEvidenceRecord {
        ProjectionEvidenceRecord {
            projection: projection(revision),
            candidate_outcomes: Vec::new(),
            publishable_gateway_ids: Vec::new(),
        }
    }

    fn active_certificate_metadata() -> ActiveCertificateMetadata {
        ActiveCertificateMetadata {
            owner: CertificateOwner::RouteBinding {
                route_binding_id: RouteBindingId::try_new("route_1").expect("route binding id"),
            },
            active: ployz_core::cert::ActiveCertState {
                cert_id: cert_id("cert_app_example_com"),
                hostname: route_hostname("app.example.com"),
                bundle_ref: CertBundleRef::try_new(format!(
                    "sha256:{}:/var/lib/ployz/certificates/cert_app_example_com.bundle",
                    "a".repeat(64)
                ))
                .expect("bundle ref"),
                validity: CertValidityWindow::try_new(
                    CertValidAt::try_new(1).expect("not before"),
                    CertValidAt::try_new(2).expect("not after"),
                )
                .expect("validity"),
            },
        }
    }

    #[test]
    fn certificate_owner_keys_keep_namespace_and_route_owners_distinct() {
        let route = CertificateOwner::RouteBinding {
            route_binding_id: RouteBindingId::try_new("route_1").expect("route binding id"),
        };

        assert_ne!(
            certificate_owner_key(&CertificateOwner::PloyzAutomaticNamespace),
            certificate_owner_key(&route)
        );
    }

    #[tokio::test]
    async fn active_certificate_metadata_round_trips_with_its_owner() {
        let store =
            ActiveCertificateMetadataStore::new(CoreStore::open_in_memory().await.expect("store"));
        let metadata = active_certificate_metadata();

        store
            .replace(metadata.clone())
            .await
            .expect("store metadata");

        assert_eq!(
            store
                .active_for_owner(&metadata.owner)
                .await
                .expect("load metadata"),
            Some(metadata)
        );
    }

    #[tokio::test]
    async fn configuration_write_rejects_ployz_hostnames_without_target() {
        let store = IngressIntentStore::new(CoreStore::open_in_memory().await.expect("store"));

        let error = store
            .replace(configuration(
                AutomaticHostnameConfiguration::Ployz,
                PloyzDnsTargetIntent::Disabled,
            ))
            .await
            .expect_err("invalid configuration");

        assert!(matches!(
            error,
            IngressIntentStoreError::InvalidConfiguration { .. }
        ));
    }

    #[tokio::test]
    async fn configuration_write_commits_both_decisions_atomically() {
        let store = IngressIntentStore::new(CoreStore::open_in_memory().await.expect("store"));
        let expected = configuration(
            AutomaticHostnameConfiguration::Ployz,
            PloyzDnsTargetIntent::Enabled,
        );

        store.replace(expected.clone()).await.expect("store config");

        assert_eq!(store.load().await.expect("load config"), Some(expected));
    }

    #[tokio::test]
    async fn configuration_write_preserves_namespace_used_by_automatic_bindings() {
        let core = CoreStore::open_in_memory().await.expect("store");
        let store = IngressIntentStore::new(core.clone());
        store
            .replace(configuration(
                AutomaticHostnameConfiguration::Ployz,
                PloyzDnsTargetIntent::Enabled,
            ))
            .await
            .expect("store config");
        core.call(|conn| {
            conn.execute(
                "INSERT INTO route_bindings (hostname, route_binding_id, json)
                 VALUES ('api.example.com', 'route_1', '{\"origin\":\"automatic\"}')",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("store binding");

        let error = store
            .replace(configuration(
                AutomaticHostnameConfiguration::Disabled,
                PloyzDnsTargetIntent::Enabled,
            ))
            .await
            .expect_err("namespace remains in use");

        assert!(matches!(
            error,
            IngressIntentStoreError::InvalidConfiguration { .. }
        ));
    }

    #[tokio::test]
    async fn configuration_preflight_rejects_namespace_used_by_automatic_bindings() {
        let core = CoreStore::open_in_memory().await.expect("store");
        let store = IngressIntentStore::new(core.clone());
        store
            .replace(configuration(
                AutomaticHostnameConfiguration::Ployz,
                PloyzDnsTargetIntent::Enabled,
            ))
            .await
            .expect("store config");
        core.call(|conn| {
            conn.execute(
                "INSERT INTO route_bindings (hostname, route_binding_id, json)
                 VALUES ('api.example.com', 'route_1', '{\"origin\":\"automatic\"}')",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("store binding");

        let error = store
            .validate_replace(&configuration(
                AutomaticHostnameConfiguration::Disabled,
                PloyzDnsTargetIntent::Enabled,
            ))
            .await
            .expect_err("namespace remains in use");

        assert!(matches!(
            error,
            IngressIntentStoreError::InvalidConfiguration { .. }
        ));
        assert!(matches!(
            store.load().await.expect("load config"),
            Some(IngressConfiguration {
                automatic_hostnames: AutomaticHostnameConfiguration::Ployz,
                ployz_dns_target: PloyzDnsTargetIntent::Enabled,
            })
        ));
    }

    #[tokio::test]
    async fn projection_compare_and_replace_rejects_stale_writer() {
        let store = IngressProjectionStore::new(CoreStore::open_in_memory().await.expect("store"));
        store
            .compare_and_replace(None, projection_record(1))
            .await
            .expect("initial write");

        let outcome = store
            .compare_and_replace(None, projection_record(2))
            .await
            .expect("compare");

        assert_eq!(
            outcome,
            IngressProjectionWrite::Conflict {
                current: Some(projection(1).identity())
            }
        );
    }

    #[tokio::test]
    async fn acquired_target_starts_with_an_empty_checkpoint() {
        let core = CoreStore::open_in_memory().await.expect("store");
        IngressIntentStore::new(core.clone())
            .replace(configuration(
                AutomaticHostnameConfiguration::Disabled,
                PloyzDnsTargetIntent::Enabled,
            ))
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
                last_applied_endpoints: ManagedDnsEndpointSet::empty(),
            })
        );
    }
}
