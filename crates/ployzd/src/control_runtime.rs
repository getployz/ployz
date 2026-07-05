//! Runtime wiring for the control role.

use crate::api_runtime::{ApiServiceRuntimeError, start_operation_api_service_with_handlers};
use crate::config::ControlProcessConfig;
use crate::controllers::OperationControllers;
use crate::deploy_runtime::DeployOperationRuntime;
use crate::deploy_worker::DeployExecutionMachineScope;
use crate::intent::{NatsIntentReader, RunningIntentRuntime, start_intent_runtime};
use crate::machine_lifecycle_runtime::MachineLifecycleOperationRuntime;
use crate::machine_runtime::client::{
    NatsMachineFactsReader, NatsMachineLogsTailer, NatsMachineSubstrateUpdater,
};
use crate::machine_update_runtime::MachineUpdateOperationRuntime;
use crate::nats_authorization::{
    MachineCredentialMintRuntime, MintResumeError, MintVerifyEndpoint, NatsAuthorizationRuntime,
    NatsReloadRunner, RenderFailure, SystemctlNatsReloadRunner,
};
use crate::operation_api::OperationApiHandlers;
use crate::process_support::shutdown_signal;
use crate::tasks::TaskRegistry;
use ployz_nats::bootstrap::{BootstrapAssuranceError, BootstrapPlan, BootstrapRefusal};
use ployz_nats::connect::{NatsConnectError, connect_authenticated};
use ployz_nats::core_state::{AsyncNatsCoreStateStore, CoreStateStoreError};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::operations::{AsyncNatsOperationEventLog, AsyncNatsOperationStatusStore};
use ployz_nats::service_runtime::{NatsClient, NatsServiceShutdownError, RunningNatsService};
use std::fmt;
use std::time::Duration;

const CONTROL_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const INTENT_PUBLISH_INTERVAL: Duration = Duration::from_secs(30);

pub struct RunningControlRuntime {
    intent: RunningIntentRuntime,
    operation_api: RunningNatsService,
    deploy_tasks: TaskRegistry,
    machine_update_tasks: TaskRegistry,
    machine_lifecycle_tasks: TaskRegistry,
    mint_tasks: TaskRegistry,
    authorization: NatsAuthorizationRuntime,
}

impl RunningControlRuntime {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        self.operation_api.shutdown().await?;
        self.intent.shutdown().await?;
        self.deploy_tasks.abort_all();
        self.machine_update_tasks.abort_all();
        self.machine_lifecycle_tasks.abort_all();
        self.mint_tasks.abort_all();
        self.authorization.shutdown();
        Ok(())
    }
}

pub async fn start_control_runtime(
    config: &ControlProcessConfig,
) -> Result<RunningControlRuntime, ControlRuntimeError> {
    let client = connect_authenticated(&config.nats_connect, CONTROL_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(ControlRuntimeError::ConnectNats)?;
    start_control_runtime_with_client_and_reload(client, config, SystemctlNatsReloadRunner).await
}

pub async fn start_control_runtime_with_client(
    client: NatsClient,
    config: &ControlProcessConfig,
) -> Result<RunningControlRuntime, ControlRuntimeError> {
    start_control_runtime_with_client_and_reload(client, config, SystemctlNatsReloadRunner).await
}

pub async fn start_control_runtime_with_client_and_reload(
    client: NatsClient,
    config: &ControlProcessConfig,
    reload: impl NatsReloadRunner,
) -> Result<RunningControlRuntime, ControlRuntimeError> {
    if config.machine_bootstrap.join_material.is_none() {
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
    let controllers = OperationControllers::new(
        event_log,
        status_store,
        core_state.clone(),
        config.machine_bootstrap.clone(),
    );
    let authorization = NatsAuthorizationRuntime::start(
        config.nats_authorization.authorized_users_file.clone(),
        reload,
    );
    authorization
        .handle()
        .render(None)
        .await
        .map_err(ControlRuntimeError::RenderNatsAuthorization)?;
    let deploy_tasks = TaskRegistry::default();
    let machine_update_tasks = TaskRegistry::default();
    let machine_lifecycle_tasks = TaskRegistry::default();
    let mint_tasks = TaskRegistry::default();
    let deploy_runtime = DeployOperationRuntime::new(
        client.clone(),
        core_state.clone(),
        observations.clone(),
        controllers.clone(),
        DeployExecutionMachineScope::same_machines(config.deploy_machines.clone()),
        config.deploy_step_timeout,
        deploy_tasks.clone(),
    );
    let machine_mint = MachineCredentialMintRuntime::new(
        controllers.clone(),
        authorization.handle(),
        MintVerifyEndpoint::from_connect(&config.nats_connect),
        config.nats_authorization.machine_seed_file.clone(),
        mint_tasks.clone(),
    );
    // Startup reconciliation (one bounded pass, owned by control start): a
    // control crash between machine-add acceptance and material-ready
    // leaves the mint without a worker. Resume those mints now, before the
    // operation API takes new requests.
    machine_mint
        .resume_unfinished_mints()
        .await
        .map_err(ControlRuntimeError::ResumeMachineAddMints)?;
    let logs_tailer = NatsMachineLogsTailer::new(client.clone());
    let facts_reader = NatsMachineFactsReader::new(client.clone());
    let intent_reader = NatsIntentReader::new(client.clone());
    let intent = start_intent_runtime(
        client.clone(),
        core_state.clone(),
        config.nats_authorization.machine_lifecycles_file.clone(),
        INTENT_PUBLISH_INTERVAL,
    )
    .await
    .map_err(ControlRuntimeError::StartIntent)?;
    let machine_updater = NatsMachineSubstrateUpdater::new(client.clone());
    let machine_update_runtime = MachineUpdateOperationRuntime::new(
        controllers.clone(),
        core_state.clone(),
        machine_updater,
        machine_update_tasks.clone(),
    );
    let machine_lifecycle_runtime = MachineLifecycleOperationRuntime::new(
        controllers.clone(),
        core_state.clone(),
        config.nats_authorization.machine_lifecycles_file.clone(),
        machine_lifecycle_tasks.clone(),
    );
    let operation_api = start_operation_api_service_with_handlers(
        client,
        OperationApiHandlers::execute_operations(
            controllers,
            deploy_runtime,
            machine_update_runtime,
            machine_lifecycle_runtime,
            machine_mint,
            config
                .deploy_machines
                .first()
                .cloned()
                .ok_or(ControlRuntimeError::MissingDeployMachine)?,
            core_state,
            observations,
            facts_reader,
            intent_reader,
            logs_tailer,
        ),
    )
    .await
    .map_err(ControlRuntimeError::StartOperationApi)?;

    Ok(RunningControlRuntime {
        intent,
        operation_api,
        deploy_tasks,
        machine_update_tasks,
        machine_lifecycle_tasks,
        mint_tasks,
        authorization,
    })
}

pub async fn run_control_until_shutdown(
    config: &ControlProcessConfig,
) -> Result<(), ControlRuntimeError> {
    let runtime = start_control_runtime(config).await?;
    shutdown_signal()
        .await
        .map_err(ControlRuntimeError::ShutdownSignal)?;
    runtime
        .shutdown()
        .await
        .map_err(ControlRuntimeError::ShutdownOperationApi)
}

#[derive(Debug)]
pub enum ControlRuntimeError {
    MissingMachineJoinTemplate,
    MissingDeployMachine,
    ConnectNats(NatsConnectError),
    PlanBootstrap(BootstrapRefusal),
    AssureBootstrap(BootstrapAssuranceError),
    OpenCoreState(CoreStateStoreError),
    OpenObservations(ObservationStoreError),
    OpenOperationStatus(ployz_nats::operations::OperationStatusStoreError),
    RenderNatsAuthorization(RenderFailure),
    ResumeMachineAddMints(MintResumeError),
    StartIntent(ployz_nats::service_runtime::NatsServiceRuntimeError),
    StartOperationApi(ApiServiceRuntimeError),
    ShutdownSignal(std::io::Error),
    ShutdownOperationApi(NatsServiceShutdownError),
}

impl fmt::Display for ControlRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMachineJoinTemplate => {
                write!(formatter, "machine add requires configured join template")
            }
            Self::MissingDeployMachine => {
                write!(formatter, "control runtime requires a deploy machine")
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
            Self::RenderNatsAuthorization(error) => {
                write!(formatter, "failed to render NATS authorization: {error}")
            }
            Self::ResumeMachineAddMints(error) => {
                write!(
                    formatter,
                    "failed to reconcile unfinished machine-add mints: {error}"
                )
            }
            Self::StartIntent(error) => {
                write!(formatter, "failed to start intent service: {error:?}")
            }
            Self::StartOperationApi(error) => {
                write!(
                    formatter,
                    "failed to start operation API service: {error:?}"
                )
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
