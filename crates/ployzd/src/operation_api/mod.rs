//! User-facing operation service handlers.

mod error_map;
mod first_node;
mod machine_join;
mod queries;
mod submit;

pub use first_node::init_first_node_activate;
pub use machine_join::{machine_join_redeem, machine_join_report};
pub use queries::{
    LogsQueryRuntime, MachineQueryRuntime, ServiceQueryRuntime, ops_status, ops_status_missing,
    ops_watch,
};
pub use submit::{backup_create, deploy_submit, machine_add, owned_operation};

use crate::backup_runtime::BackupOperationRuntime;
use crate::controllers::OperationControllers;
use crate::deploy_runtime::DeployOperationRuntime;
use crate::nats_authorization::MachineCredentialMintRuntime;
use crate::node_rpc::NatsNodeLogsTailer;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use std::sync::Arc;

#[derive(Clone)]
pub struct OperationApiHandlers {
    controllers: OperationControllers,
    deploy_runtime: Arc<DeployOperationRuntime>,
    backup_runtime: Arc<BackupOperationRuntime>,
    machine_mint: Arc<MachineCredentialMintRuntime>,
    /// Cluster-truth store for the writes this layer owns (machine
    /// activation on join completion) and the first-node idempotency
    /// read. The query runtimes stay genuinely read-only.
    core_state: AsyncNatsCoreStateStore,
    machine_query: Arc<MachineQueryRuntime>,
    service_query: Arc<ServiceQueryRuntime>,
    logs_query: Arc<LogsQueryRuntime>,
}

impl OperationApiHandlers {
    #[must_use]
    pub fn execute_operations(
        controllers: OperationControllers,
        deploy_runtime: DeployOperationRuntime,
        backup_runtime: BackupOperationRuntime,
        machine_mint: MachineCredentialMintRuntime,
        core_state: AsyncNatsCoreStateStore,
        observations: AsyncNatsObservationStore,
        logs_tailer: NatsNodeLogsTailer,
    ) -> Self {
        let machine_query = MachineQueryRuntime::new(core_state.clone(), observations.clone());
        let service_query = ServiceQueryRuntime::new(core_state.clone());
        let logs_query = LogsQueryRuntime::new(observations, logs_tailer);
        Self {
            controllers,
            deploy_runtime: Arc::new(deploy_runtime),
            backup_runtime: Arc::new(backup_runtime),
            machine_mint: Arc::new(machine_mint),
            core_state,
            machine_query: Arc::new(machine_query),
            service_query: Arc::new(service_query),
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

    pub(crate) fn logs_query(&self) -> &LogsQueryRuntime {
        &self.logs_query
    }
}
