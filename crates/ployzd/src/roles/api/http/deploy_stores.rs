//! Driver-facing storage seams for the deploy driver.
//!
//! Each trait mirrors the concrete Corrosion store it delegates to, carrying
//! the store's typed errors through unchanged so failure classes survive to
//! the driver.

use async_trait::async_trait;
use ployz_core::corrosion::{
    CorrosionDeployTransition, CorrosionNamespaceName, CorrosionServiceName, CorrosionTimestamp,
    OperationDocument,
};
use ployz_core::ids::{ContainerId, OperationRowId, ServiceRowId};

use super::operation_evidence::PreparedRedeployIntent;
use super::operation_finalizer::{
    PreparedPromotionStore, PromotionFinalizerStoreError, PromotionRequestDisposition,
    PromotionRowsObservation,
};
use super::operation_store::{
    ConditionalOperationWrite, HeartbeatWrite, ObservedOperation, OperationStore,
    OperationStoreError,
};
use super::promotion_store::{
    ContainerRowCleanup, CorrosionPreparedPromotionStore, DeployAdmission, ObservedContainer,
};

pub(super) trait DeployStore: PreparedPromotionStore + RedeployStore {}

impl<T> DeployStore for T where T: PreparedPromotionStore + RedeployStore {}

/// Admission triage and exact redeploy row writes, lifted onto the driver seam.
#[async_trait]
pub(super) trait RedeployStore: Send + Sync {
    async fn resolve_deploy_admission(
        &self,
        namespace_name: &CorrosionNamespaceName,
        service_name: &CorrosionServiceName,
    ) -> Result<DeployAdmission, PromotionFinalizerStoreError>;

    async fn converge_redeploy_rows(
        &self,
        prepared: &PreparedRedeployIntent,
    ) -> Result<(PromotionRequestDisposition, PromotionRowsObservation), PromotionFinalizerStoreError>;

    async fn service_containers(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<ObservedContainer>, PromotionFinalizerStoreError>;

    async fn delete_exact_container_rows(
        &self,
        rows: &[ObservedContainer],
    ) -> Result<Vec<(ContainerId, ContainerRowCleanup)>, PromotionFinalizerStoreError>;
}

#[async_trait]
impl RedeployStore for CorrosionPreparedPromotionStore {
    async fn resolve_deploy_admission(
        &self,
        namespace_name: &CorrosionNamespaceName,
        service_name: &CorrosionServiceName,
    ) -> Result<DeployAdmission, PromotionFinalizerStoreError> {
        CorrosionPreparedPromotionStore::resolve_deploy_admission(
            self,
            namespace_name,
            service_name,
        )
        .await
    }

    async fn converge_redeploy_rows(
        &self,
        prepared: &PreparedRedeployIntent,
    ) -> Result<(PromotionRequestDisposition, PromotionRowsObservation), PromotionFinalizerStoreError>
    {
        CorrosionPreparedPromotionStore::converge_redeploy_rows(self, prepared).await
    }

    async fn service_containers(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<ObservedContainer>, PromotionFinalizerStoreError> {
        CorrosionPreparedPromotionStore::service_containers(self, service_id).await
    }

    async fn delete_exact_container_rows(
        &self,
        rows: &[ObservedContainer],
    ) -> Result<Vec<(ContainerId, ContainerRowCleanup)>, PromotionFinalizerStoreError> {
        CorrosionPreparedPromotionStore::delete_exact_container_rows(self, rows).await
    }
}

#[async_trait]
pub(super) trait DeployOperationRows: Send + Sync {
    async fn insert_created(
        &self,
        operation_id: &OperationRowId,
        operation: &OperationDocument,
    ) -> Result<(), OperationStoreError>;

    async fn operation(
        &self,
        operation_id: &OperationRowId,
    ) -> Result<Option<ObservedOperation>, OperationStoreError>;

    async fn transition_deploy(
        &self,
        observed: ObservedOperation,
        transition: CorrosionDeployTransition,
    ) -> Result<ConditionalOperationWrite, OperationStoreError>;

    async fn replace_terminal(
        &self,
        observed: &ObservedOperation,
        terminal: &OperationDocument,
    ) -> Result<ConditionalOperationWrite, OperationStoreError>;

    async fn refresh_heartbeat(
        &self,
        observed: &ObservedOperation,
        now: CorrosionTimestamp,
    ) -> Result<HeartbeatWrite, OperationStoreError>;

    async fn deploy_claim_candidates(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, OperationStoreError>;

    async fn deploy_takeover_candidates(
        &self,
        operation_id: &OperationRowId,
        service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, OperationStoreError>;
}

#[async_trait]
impl DeployOperationRows for OperationStore {
    async fn insert_created(
        &self,
        operation_id: &OperationRowId,
        operation: &OperationDocument,
    ) -> Result<(), OperationStoreError> {
        OperationStore::insert_created(self, operation_id, operation).await
    }

    async fn operation(
        &self,
        operation_id: &OperationRowId,
    ) -> Result<Option<ObservedOperation>, OperationStoreError> {
        OperationStore::operation(self, operation_id).await
    }

    async fn transition_deploy(
        &self,
        observed: ObservedOperation,
        transition: CorrosionDeployTransition,
    ) -> Result<ConditionalOperationWrite, OperationStoreError> {
        OperationStore::transition_deploy(self, observed, transition).await
    }

    async fn replace_terminal(
        &self,
        observed: &ObservedOperation,
        terminal: &OperationDocument,
    ) -> Result<ConditionalOperationWrite, OperationStoreError> {
        OperationStore::replace_terminal(self, observed, terminal).await
    }

    async fn refresh_heartbeat(
        &self,
        observed: &ObservedOperation,
        now: CorrosionTimestamp,
    ) -> Result<HeartbeatWrite, OperationStoreError> {
        OperationStore::refresh_heartbeat(self, observed, now).await
    }

    async fn deploy_claim_candidates(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, OperationStoreError> {
        OperationStore::deploy_claim_candidates(self, service_id).await
    }

    async fn deploy_takeover_candidates(
        &self,
        operation_id: &OperationRowId,
        service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, OperationStoreError> {
        OperationStore::deploy_takeover_candidates(self, operation_id, service_id).await
    }
}
