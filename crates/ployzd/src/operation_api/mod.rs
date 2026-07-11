//! Operator-facing operation service handlers.

pub mod admission;
mod connectivity_proof;
mod core_replace;
mod error_map;
mod first_machine;
mod machine_join;
mod network_query;
mod queries;
pub mod service;
mod submit;

pub use core_replace::core_replace_report;
pub use first_machine::init_first_machine_activate;
pub use machine_join::{machine_join_redeem, machine_join_report};
pub use network_query::NetworkQueryService;
pub use queries::{
    LogsQueryService, MachineQueryService, RuntimeSnapshotQueryService, ServiceQueryService,
    VolumeQueryService, credential_list, ops_list, ops_status, ops_status_missing, ops_watch,
};
pub use submit::{
    core_replace, credential_add, credential_remove, deploy_reserve, deploy_submit, machine_add,
    machine_drain, machine_resume, machine_update, namespace_remove, network_repair,
    owned_operation, service_restart, volume_remove,
};

use crate::adapters::nats_authorization::MachineCredentialMint;
use crate::core_store::CoreStore;
use crate::fact_cache::FactCache;
use crate::intent::lease_intent::LeaseIntentStore;
use crate::intent::machine_roster::MachineRosterStore;
use crate::intent::service::{NatsIntentReader, publish_pending_machine_joins};
use crate::operation_api::admission::OperationControllers;
use crate::operations::credential_grant::CredentialGrantOperation;
use crate::operations::deploy::driver::DeployOperationDriver;
use crate::operations::machine_lifecycle::MachineLifecycleOperation;
use crate::operations::machine_update::MachineUpdateOperation;
use crate::operations::namespace_remove::NamespaceRemoveOperation;
use crate::operations::network_repair::NetworkRepairOperation;
use crate::operations::service_restart::ServiceRestartOperation;
use crate::operations::volume_remove::VolumeRemoveOperation;
use crate::roles::machine::client::{NatsMachineFactsReader, NatsMachineLogsTailer};
use ployz_core::dataplane::MachineEndpointSupernet;
use ployz_core::ids::MachineId;
use std::sync::Arc;

/// The operation drivers, bundled so a new kind adds a field here instead of
/// another positional parameter threaded through `execute_operations`.
pub struct OperationWorkers {
    pub credential_grant: CredentialGrantOperation,
    pub deploy: DeployOperationDriver,
    pub service_restart: ServiceRestartOperation,
    pub namespace_remove: NamespaceRemoveOperation,
    pub network_repair: NetworkRepairOperation,
    pub volume_remove: VolumeRemoveOperation,
    pub machine_update: MachineUpdateOperation,
    pub machine_lifecycle: MachineLifecycleOperation,
    pub machine_mint: MachineCredentialMint,
}

#[derive(Clone)]
pub struct OperationApiHandlers {
    controllers: OperationControllers,
    deploy_driver: Arc<DeployOperationDriver>,
    service_restart: Arc<ServiceRestartOperation>,
    namespace_remove: Arc<NamespaceRemoveOperation>,
    network_repair: Arc<NetworkRepairOperation>,
    volume_remove: Arc<VolumeRemoveOperation>,
    machine_update: Arc<MachineUpdateOperation>,
    machine_lifecycle: Arc<MachineLifecycleOperation>,
    machine_mint: Arc<MachineCredentialMint>,
    credential_grant: Arc<CredentialGrantOperation>,
    dataplane_endpoint_supernet: MachineEndpointSupernet,
    core_store: CoreStore,
    local_machine_id: MachineId,
    intent_change_client: async_nats::Client,
    machine_roster: MachineRosterStore,
    lease_intent: LeaseIntentStore,
    intent_reader: NatsIntentReader,
    machine_query: Arc<MachineQueryService>,
    service_query: Arc<ServiceQueryService>,
    network_query: Arc<NetworkQueryService>,
    volume_query: Arc<VolumeQueryService>,
    runtime_snapshot_query: Arc<RuntimeSnapshotQueryService>,
    logs_query: Arc<LogsQueryService>,
}

impl OperationApiHandlers {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn execute_operations(
        controllers: OperationControllers,
        workers: OperationWorkers,
        dataplane_endpoint_supernet: MachineEndpointSupernet,
        core_store: CoreStore,
        local_machine_id: MachineId,
        intent_change_client: async_nats::Client,
        machine_roster: MachineRosterStore,
        facts: FactCache,
        facts_reader: NatsMachineFactsReader,
        intent_reader: NatsIntentReader,
        logs_tailer: NatsMachineLogsTailer,
    ) -> Self {
        let OperationWorkers {
            credential_grant,
            deploy: deploy_driver,
            service_restart,
            namespace_remove,
            network_repair,
            volume_remove,
            machine_update,
            machine_lifecycle,
            machine_mint,
        } = workers;
        let machine_query =
            MachineQueryService::new(intent_reader.clone(), facts.clone(), facts_reader.clone());
        let service_query = ServiceQueryService::new(intent_reader.clone(), facts_reader.clone());
        let network_query =
            NetworkQueryService::new(intent_reader.clone(), intent_change_client.clone());
        let volume_query = VolumeQueryService::new(intent_reader.clone());
        let runtime_snapshot_query = RuntimeSnapshotQueryService::new(
            intent_reader.clone(),
            facts.clone(),
            facts_reader.clone(),
        );
        let logs_query = LogsQueryService::new(intent_reader.clone(), facts_reader, logs_tailer);
        let lease_intent = LeaseIntentStore::new(core_store.clone());
        Self {
            controllers,
            deploy_driver: Arc::new(deploy_driver),
            service_restart: Arc::new(service_restart),
            namespace_remove: Arc::new(namespace_remove),
            network_repair: Arc::new(network_repair),
            volume_remove: Arc::new(volume_remove),
            machine_update: Arc::new(machine_update),
            machine_lifecycle: Arc::new(machine_lifecycle),
            machine_mint: Arc::new(machine_mint),
            credential_grant: Arc::new(credential_grant),
            dataplane_endpoint_supernet,
            core_store,
            local_machine_id,
            intent_change_client,
            machine_roster,
            lease_intent,
            intent_reader,
            machine_query: Arc::new(machine_query),
            service_query: Arc::new(service_query),
            network_query: Arc::new(network_query),
            volume_query: Arc::new(volume_query),
            runtime_snapshot_query: Arc::new(runtime_snapshot_query),
            logs_query: Arc::new(logs_query),
        }
    }

    #[must_use]
    pub const fn controllers(&self) -> &OperationControllers {
        &self.controllers
    }

    pub(crate) fn machine_query(&self) -> &MachineQueryService {
        &self.machine_query
    }

    pub(crate) fn service_query(&self) -> &ServiceQueryService {
        &self.service_query
    }

    pub(crate) fn network_query(&self) -> &NetworkQueryService {
        &self.network_query
    }

    pub(crate) fn intent_reader(&self) -> &NatsIntentReader {
        &self.intent_reader
    }

    pub(crate) fn volume_query(&self) -> &VolumeQueryService {
        &self.volume_query
    }

    pub(crate) fn runtime_snapshot_query(&self) -> &RuntimeSnapshotQueryService {
        &self.runtime_snapshot_query
    }

    pub(crate) fn logs_query(&self) -> &LogsQueryService {
        &self.logs_query
    }

    pub(crate) fn machine_update(&self) -> &MachineUpdateOperation {
        &self.machine_update
    }

    pub(crate) fn credential_grant(&self) -> &CredentialGrantOperation {
        &self.credential_grant
    }

    pub(crate) fn service_restart(&self) -> &ServiceRestartOperation {
        &self.service_restart
    }

    pub(crate) fn namespace_remove(&self) -> &NamespaceRemoveOperation {
        &self.namespace_remove
    }

    pub(crate) fn network_repair(&self) -> &NetworkRepairOperation {
        &self.network_repair
    }

    pub(crate) fn volume_remove(&self) -> &VolumeRemoveOperation {
        &self.volume_remove
    }

    pub(crate) fn machine_lifecycle(&self) -> &MachineLifecycleOperation {
        &self.machine_lifecycle
    }

    pub(crate) fn local_machine_id(&self) -> &MachineId {
        &self.local_machine_id
    }

    pub(crate) fn lease_intent(&self) -> &LeaseIntentStore {
        &self.lease_intent
    }

    pub(crate) fn dataplane_endpoint_supernet(&self) -> &MachineEndpointSupernet {
        &self.dataplane_endpoint_supernet
    }

    pub(crate) async fn publish_pending_machine_joins(&self) {
        let _ = publish_pending_machine_joins(
            &self.intent_change_client,
            self.controllers.repository(),
            &self.core_store,
        )
        .await;
    }
}
