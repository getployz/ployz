//! User-facing operation service handlers.

pub mod admission;
mod error_map;
mod first_machine;
mod machine_join;
mod queries;
pub mod service;
mod submit;

pub use first_machine::init_first_machine_activate;
pub use machine_join::{machine_join_redeem, machine_join_report};
pub use queries::{
    LogsQueryService, MachineQueryService, RuntimeSnapshotQueryService, ServiceQueryService,
    ops_list, ops_status, ops_status_missing, ops_watch,
};
pub use submit::{
    deploy_submit, machine_add, machine_drain, machine_resume, machine_update, owned_operation,
};

use crate::adapters::nats_authorization::MachineCredentialMint;
use crate::fact_cache::FactCache;
use crate::intent::machine_roster::MachineRosterStore;
use crate::intent::service::NatsIntentReader;
use crate::operation_api::admission::OperationControllers;
use crate::operations::deploy::driver::DeployOperationDriver;
use crate::operations::machine_lifecycle::MachineLifecycleOperation;
use crate::operations::machine_update::MachineUpdateOperation;
use crate::roles::machine::client::{NatsMachineFactsReader, NatsMachineLogsTailer};
use ployz_core::ids::MachineId;
use std::sync::Arc;

#[derive(Clone)]
pub struct OperationApiHandlers {
    controllers: OperationControllers,
    deploy_driver: Arc<DeployOperationDriver>,
    machine_update: Arc<MachineUpdateOperation>,
    machine_lifecycle: Arc<MachineLifecycleOperation>,
    machine_mint: Arc<MachineCredentialMint>,
    local_machine_id: MachineId,
    intent_change_client: async_nats::Client,
    machine_roster: MachineRosterStore,
    machine_query: Arc<MachineQueryService>,
    service_query: Arc<ServiceQueryService>,
    runtime_snapshot_query: Arc<RuntimeSnapshotQueryService>,
    logs_query: Arc<LogsQueryService>,
}

impl OperationApiHandlers {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn execute_operations(
        controllers: OperationControllers,
        deploy_driver: DeployOperationDriver,
        machine_update: MachineUpdateOperation,
        machine_lifecycle: MachineLifecycleOperation,
        machine_mint: MachineCredentialMint,
        local_machine_id: MachineId,
        intent_change_client: async_nats::Client,
        machine_roster: MachineRosterStore,
        facts: FactCache,
        facts_reader: NatsMachineFactsReader,
        intent_reader: NatsIntentReader,
        logs_tailer: NatsMachineLogsTailer,
    ) -> Self {
        let machine_query =
            MachineQueryService::new(intent_reader.clone(), facts.clone(), facts_reader.clone());
        let service_query = ServiceQueryService::new(intent_reader.clone());
        let runtime_snapshot_query = RuntimeSnapshotQueryService::new(
            intent_reader.clone(),
            facts.clone(),
            facts_reader.clone(),
        );
        let logs_query = LogsQueryService::new(intent_reader, facts_reader, logs_tailer);
        Self {
            controllers,
            deploy_driver: Arc::new(deploy_driver),
            machine_update: Arc::new(machine_update),
            machine_lifecycle: Arc::new(machine_lifecycle),
            machine_mint: Arc::new(machine_mint),
            local_machine_id,
            intent_change_client,
            machine_roster,
            machine_query: Arc::new(machine_query),
            service_query: Arc::new(service_query),
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

    pub(crate) fn runtime_snapshot_query(&self) -> &RuntimeSnapshotQueryService {
        &self.runtime_snapshot_query
    }

    pub(crate) fn logs_query(&self) -> &LogsQueryService {
        &self.logs_query
    }

    pub(crate) fn machine_update(&self) -> &MachineUpdateOperation {
        &self.machine_update
    }

    pub(crate) fn machine_lifecycle(&self) -> &MachineLifecycleOperation {
        &self.machine_lifecycle
    }

    pub(crate) fn local_machine_id(&self) -> &MachineId {
        &self.local_machine_id
    }
}
