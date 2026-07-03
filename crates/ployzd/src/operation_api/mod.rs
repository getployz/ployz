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
pub use submit::{deploy_submit, machine_add, machine_update, owned_operation};

use crate::controllers::OperationControllers;
use crate::deploy_runtime::DeployOperationRuntime;
use crate::machine_runtime::client::NatsMachineLogsTailer;
use crate::machine_update_runtime::MachineUpdateOperationRuntime;
use crate::nats_authorization::MachineCredentialMintRuntime;
use ployz_core::ids::MachineId;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use std::sync::Arc;

#[derive(Clone)]
pub struct OperationApiHandlers {
    controllers: OperationControllers,
    deploy_runtime: Arc<DeployOperationRuntime>,
    machine_update_runtime: Arc<MachineUpdateOperationRuntime>,
    machine_mint: Arc<MachineCredentialMintRuntime>,
    local_machine_id: MachineId,
    /// Cluster-truth store for the writes this layer owns (machine
    /// activation on join completion) and the first-machine idempotency
    /// read. The query runtimes stay genuinely read-only.
    core_state: AsyncNatsCoreStateStore,
    machine_query: Arc<MachineQueryRuntime>,
    service_query: Arc<ServiceQueryRuntime>,
    runtime_snapshot_query: Arc<RuntimeSnapshotQueryRuntime>,
    logs_query: Arc<LogsQueryRuntime>,
}

impl OperationApiHandlers {
    #[must_use]
    pub fn execute_operations(
        controllers: OperationControllers,
        deploy_runtime: DeployOperationRuntime,
        machine_update_runtime: MachineUpdateOperationRuntime,
        machine_mint: MachineCredentialMintRuntime,
        local_machine_id: MachineId,
        core_state: AsyncNatsCoreStateStore,
        observations: AsyncNatsObservationStore,
        logs_tailer: NatsMachineLogsTailer,
    ) -> Self {
        let machine_query = MachineQueryRuntime::new(core_state.clone(), observations.clone());
        let service_query = ServiceQueryRuntime::new(core_state.clone());
        let runtime_snapshot_query =
            RuntimeSnapshotQueryRuntime::new(core_state.clone(), observations.clone());
        let logs_query = LogsQueryRuntime::new(observations, logs_tailer);
        Self {
            controllers,
            deploy_runtime: Arc::new(deploy_runtime),
            machine_update_runtime: Arc::new(machine_update_runtime),
            machine_mint: Arc::new(machine_mint),
            local_machine_id,
            core_state,
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

    pub(crate) fn local_machine_id(&self) -> &MachineId {
        &self.local_machine_id
    }
}
