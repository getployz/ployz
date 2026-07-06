//! User-facing operation service handlers.

mod error_map;
mod first_machine;
mod machine_join;
mod queries;
mod submit;

pub use first_machine::init_first_machine_activate;
pub use machine_join::{machine_join_redeem, machine_join_report};
pub use queries::{
    LogsQueryRuntime, MachineQueryRuntime, RuntimeSnapshotQueryRuntime, ServiceQueryRuntime,
    ops_list, ops_status, ops_status_missing, ops_watch,
};
pub use submit::{
    deploy_submit, machine_add, machine_drain, machine_resume, machine_update, owned_operation,
};

use crate::controllers::OperationControllers;
use crate::operations::deploy::driver::DeployOperationRuntime;
use crate::intent::service::NatsIntentReader;
use crate::operations::machine_lifecycle::MachineLifecycleOperationRuntime;
use crate::intent::machine_roster::MachineRosterStore;
use crate::roles::machine::client::{NatsMachineFactsReader, NatsMachineLogsTailer};
use crate::operations::machine_update::MachineUpdateOperationRuntime;
use crate::adapters::nats_authorization::MachineCredentialMintRuntime;
use crate::fact_cache::RuntimeFactsCache;
use ployz_core::ids::MachineId;
use std::sync::Arc;

#[derive(Clone)]
pub struct OperationApiHandlers {
    controllers: OperationControllers,
    deploy_runtime: Arc<DeployOperationRuntime>,
    machine_update_runtime: Arc<MachineUpdateOperationRuntime>,
    machine_lifecycle_runtime: Arc<MachineLifecycleOperationRuntime>,
    machine_mint: Arc<MachineCredentialMintRuntime>,
    local_machine_id: MachineId,
    intent_change_client: async_nats::Client,
    machine_roster: MachineRosterStore,
    machine_query: Arc<MachineQueryRuntime>,
    service_query: Arc<ServiceQueryRuntime>,
    runtime_snapshot_query: Arc<RuntimeSnapshotQueryRuntime>,
    logs_query: Arc<LogsQueryRuntime>,
}

impl OperationApiHandlers {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn execute_operations(
        controllers: OperationControllers,
        deploy_runtime: DeployOperationRuntime,
        machine_update_runtime: MachineUpdateOperationRuntime,
        machine_lifecycle_runtime: MachineLifecycleOperationRuntime,
        machine_mint: MachineCredentialMintRuntime,
        local_machine_id: MachineId,
        intent_change_client: async_nats::Client,
        machine_roster: MachineRosterStore,
        facts: RuntimeFactsCache,
        facts_reader: NatsMachineFactsReader,
        intent_reader: NatsIntentReader,
        logs_tailer: NatsMachineLogsTailer,
    ) -> Self {
        let machine_query =
            MachineQueryRuntime::new(intent_reader.clone(), facts.clone(), facts_reader.clone());
        let service_query = ServiceQueryRuntime::new(intent_reader.clone());
        let runtime_snapshot_query = RuntimeSnapshotQueryRuntime::new(
            intent_reader.clone(),
            facts.clone(),
            facts_reader.clone(),
        );
        let logs_query = LogsQueryRuntime::new(intent_reader, facts_reader, logs_tailer);
        Self {
            controllers,
            deploy_runtime: Arc::new(deploy_runtime),
            machine_update_runtime: Arc::new(machine_update_runtime),
            machine_lifecycle_runtime: Arc::new(machine_lifecycle_runtime),
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

    pub(crate) fn machine_query(&self) -> &MachineQueryRuntime {
        &self.machine_query
    }

    pub(crate) fn service_query(&self) -> &ServiceQueryRuntime {
        &self.service_query
    }

    pub(crate) fn runtime_snapshot_query(&self) -> &RuntimeSnapshotQueryRuntime {
        &self.runtime_snapshot_query
    }

    pub(crate) fn logs_query(&self) -> &LogsQueryRuntime {
        &self.logs_query
    }

    pub(crate) fn machine_update_runtime(&self) -> &MachineUpdateOperationRuntime {
        &self.machine_update_runtime
    }

    pub(crate) fn machine_lifecycle_runtime(&self) -> &MachineLifecycleOperationRuntime {
        &self.machine_lifecycle_runtime
    }

    pub(crate) fn local_machine_id(&self) -> &MachineId {
        &self.local_machine_id
    }
}
