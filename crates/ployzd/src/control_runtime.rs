//! Runtime wiring for the control role.

use crate::api_runtime::{ApiServiceRuntimeError, start_operation_api_service_with_handlers};
use crate::backup_runtime::{BackupTaskRegistry, BackupWorkerStartError, OwnedBackupLauncher};
use crate::config::ControlProcessConfig;
use crate::controllers::OperationControllers;
use crate::deploy_runtime::{DeployOperationRuntime, DeployTaskRegistry};
use crate::deploy_worker::DeployExecutionNodeScope;
use crate::node_rpc::NatsNodeLogsTailer;
use crate::operation_api::{MachineQueryRuntime, OperationApiHandlers};
use ployz_core::ids::OperationOwnerId;
use ployz_nats::bootstrap::{BootstrapAssuranceError, BootstrapPlan, BootstrapRefusal};
use ployz_nats::connect::{NatsConnectError, connect_with_timeout};
use ployz_nats::core_state::{AsyncNatsCoreStateStore, CoreStateStoreError};
use ployz_nats::objects::{AsyncNatsBackupObjectStore, BackupObjectStoreError};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::operations::{AsyncNatsOperationEventLog, AsyncNatsOperationStatusStore};
use ployz_nats::service_runtime::{NatsClient, NatsServiceShutdownError, RunningNatsService};
use std::fmt;
use std::time::Duration;

const CONTROL_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_OPERATION_OWNER_ID: &str = "control";

pub struct RunningControlRuntime {
    operation_api: RunningNatsService,
    deploy_tasks: DeployTaskRegistry,
    backup_tasks: BackupTaskRegistry,
}

impl RunningControlRuntime {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        self.operation_api.shutdown().await?;
        self.deploy_tasks.abort_all();
        self.backup_tasks.abort_all();
        Ok(())
    }
}

pub async fn start_control_runtime(
    config: &ControlProcessConfig,
) -> Result<RunningControlRuntime, ControlRuntimeError> {
    let client = connect_with_timeout(&config.nats_url(), CONTROL_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(ControlRuntimeError::ConnectNats)?;
    start_control_runtime_with_client(client, config).await
}

pub async fn start_control_runtime_with_client(
    client: NatsClient,
    config: &ControlProcessConfig,
) -> Result<RunningControlRuntime, ControlRuntimeError> {
    if config.machine_bootstrap.join_template.is_none() {
        return Err(ControlRuntimeError::MissingMachineJoinTemplate);
    }
    let plan = BootstrapPlan::for_single_server_client(&client)
        .map_err(ControlRuntimeError::PlanBootstrap)?;
    let jetstream = async_nats::jetstream::new(client.clone());
    ployz_nats::bootstrap::assure_nats_resources(&jetstream, &plan)
        .await
        .map_err(ControlRuntimeError::AssureBootstrap)?;

    let event_log = AsyncNatsOperationEventLog::new(jetstream.clone());
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .map_err(ControlRuntimeError::OpenCoreState)?;
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
        .await
        .map_err(ControlRuntimeError::OpenObservations)?;
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&jetstream)
        .await
        .map_err(ControlRuntimeError::OpenOperationStatus)?;
    let backups = AsyncNatsBackupObjectStore::from_jetstream(&jetstream)
        .await
        .map_err(ControlRuntimeError::OpenBackupObjects)?;
    let owner_id = OperationOwnerId::try_new(CONTROL_OPERATION_OWNER_ID)
        .expect("control owner id is static and valid");
    let controllers = OperationControllers::with_owner(
        event_log,
        status_store,
        owner_id,
        config.machine_bootstrap.clone(),
    );
    let deploy_tasks = DeployTaskRegistry::default();
    let backup_tasks = BackupTaskRegistry::default();
    let deploy_runtime = DeployOperationRuntime::new(
        client.clone(),
        core_state.clone(),
        observations.clone(),
        controllers.clone(),
        DeployExecutionNodeScope::same_nodes(config.deploy_nodes.clone()),
        config.deploy_step_timeout,
        deploy_tasks.clone(),
    );
    let machine_query = MachineQueryRuntime::new(core_state, observations);
    let logs_tailer = NatsNodeLogsTailer::new(client.clone());
    let backup_launcher = OwnedBackupLauncher::new(
        jetstream,
        controllers.clone(),
        backups,
        backup_tasks.clone(),
    );
    backup_launcher
        .start_worker()
        .await
        .map_err(ControlRuntimeError::StartBackupWorker)?;
    let operation_api = start_operation_api_service_with_handlers(
        client,
        OperationApiHandlers::execute_operations(
            controllers,
            deploy_runtime,
            machine_query,
            logs_tailer,
        ),
    )
    .await
    .map_err(ControlRuntimeError::StartOperationApi)?;

    Ok(RunningControlRuntime {
        operation_api,
        deploy_tasks,
        backup_tasks,
    })
}

pub async fn run_control_until_shutdown(
    config: &ControlProcessConfig,
) -> Result<(), ControlRuntimeError> {
    let runtime = start_control_runtime(config).await?;
    wait_for_shutdown_signal()
        .await
        .map_err(ControlRuntimeError::ShutdownSignal)?;
    runtime
        .shutdown()
        .await
        .map_err(ControlRuntimeError::ShutdownOperationApi)
}

async fn wait_for_shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}

#[derive(Debug)]
pub enum ControlRuntimeError {
    MissingMachineJoinTemplate,
    ConnectNats(NatsConnectError),
    PlanBootstrap(BootstrapRefusal),
    AssureBootstrap(BootstrapAssuranceError),
    OpenCoreState(CoreStateStoreError),
    OpenObservations(ObservationStoreError),
    OpenOperationStatus(ployz_nats::operations::OperationStatusStoreError),
    OpenBackupObjects(BackupObjectStoreError),
    StartOperationApi(ApiServiceRuntimeError),
    StartBackupWorker(BackupWorkerStartError),
    ShutdownSignal(std::io::Error),
    ShutdownOperationApi(NatsServiceShutdownError),
}

impl fmt::Display for ControlRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMachineJoinTemplate => {
                write!(formatter, "machine add requires configured join template")
            }
            Self::ConnectNats(error) => write!(formatter, "{error}"),
            Self::PlanBootstrap(error) => write!(formatter, "NATS bootstrap refused: {error}"),
            Self::AssureBootstrap(error) => write!(formatter, "{error}"),
            Self::OpenCoreState(error) => {
                write!(formatter, "failed to open core state store: {error}")
            }
            Self::OpenObservations(error) => {
                write!(formatter, "failed to open observation store: {error}")
            }
            Self::OpenOperationStatus(error) => {
                write!(
                    formatter,
                    "failed to open operation status store: {error:?}"
                )
            }
            Self::OpenBackupObjects(error) => {
                write!(formatter, "failed to open backup object store: {error:?}")
            }
            Self::StartOperationApi(error) => {
                write!(
                    formatter,
                    "failed to start operation API service: {error:?}"
                )
            }
            Self::StartBackupWorker(error) => {
                write!(formatter, "failed to start backup worker: {error:?}")
            }
            Self::ShutdownSignal(error) => {
                write!(formatter, "failed to wait for shutdown: {error}")
            }
            Self::ShutdownOperationApi(error) => {
                write!(formatter, "failed to stop operation API service: {error:?}")
            }
        }
    }
}

impl std::error::Error for ControlRuntimeError {}
