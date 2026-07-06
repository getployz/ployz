//! Runtime wiring for the control role.

use crate::operation_api::service::{ApiServiceRuntimeError, start_operation_api_service_with_handlers};
use crate::config::ControlProcessConfig;
use crate::operation_api::admission::OperationControllers;
use crate::core_store::{CoreStore, CoreStoreError};
use crate::operations::deploy::driver::DeployOperationRuntime;
use crate::operations::deploy::DeployMachineCandidates;
use crate::intent::service::{NatsIntentReader, RunningIntentRuntime, start_intent_runtime};
use crate::operations::machine_lifecycle::MachineLifecycleOperationRuntime;
use crate::intent::machine_roster::MachineRosterStore;
use crate::roles::machine::client::{
    NatsMachineFactsReader, NatsMachineLogsTailer, NatsMachineSubstrateUpdater,
};
use crate::operations::machine_update::MachineUpdateOperationRuntime;
use crate::intent::namespace_intent::NamespaceIntentStore;
use crate::adapters::nats_authorization::{
    MachineCredentialMintRuntime, MintResumeError, MintVerifyEndpoint, NatsAuthorizationRuntime,
    NatsReloadRunner, RenderFailure, SystemctlNatsReloadRunner,
};
use crate::operation_api::OperationApiHandlers;
use crate::operations::log::OperationRepository;
use crate::process_support::shutdown_signal;
use crate::fact_cache::{
    RunningRuntimeFactsCache, RuntimeFactsCacheError, start_runtime_facts_cache,
};
use crate::tasks::TaskRegistry;
use ployz_nats::connect::{NatsConnectError, connect_authenticated};
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
    facts_cache: RunningRuntimeFactsCache,
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
        self.facts_cache.shutdown().await;
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

    let core_store = CoreStore::open(config.core_db_path.clone())
        .await
        .map_err(ControlRuntimeError::OpenCoreStore)?;
    let repository = OperationRepository::open(core_store.clone(), client.clone());
    let controllers = OperationControllers::new(repository, config.machine_bootstrap.clone());
    let authorization = NatsAuthorizationRuntime::start(
        config.nats_authorization.authorized_users_file.clone(),
        reload,
    );
    authorization
        .handle()
        .render(None)
        .await
        .map_err(ControlRuntimeError::RenderNatsAuthorization)?;
    // Start the facts cache only after authorization has rendered and
    // reloaded permissions: its subscription to plz.v1.facts.* must not be
    // established before the grant exists, or NATS rejects it asynchronously
    // and the cache never resubscribes. Nothing between here and the
    // operation API consumes the cache, so this ordering is free.
    let facts_cache = start_runtime_facts_cache(client.clone())
        .await
        .map_err(ControlRuntimeError::StartFactsCache)?;
    let facts = facts_cache.cache();
    let deploy_tasks = TaskRegistry::default();
    let machine_update_tasks = TaskRegistry::default();
    let machine_lifecycle_tasks = TaskRegistry::default();
    let mint_tasks = TaskRegistry::default();
    let namespace_intent = NamespaceIntentStore::new(core_store.clone());
    let machine_roster = MachineRosterStore::new(core_store.clone());
    let deploy_runtime = DeployOperationRuntime::new(
        client.clone(),
        namespace_intent.clone(),
        controllers.clone(),
        DeployMachineCandidates::same_machines(config.deploy_machines.clone()),
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
        machine_roster.clone(),
        namespace_intent,
        INTENT_PUBLISH_INTERVAL,
    )
    .await
    .map_err(ControlRuntimeError::StartIntent)?;
    let machine_updater = NatsMachineSubstrateUpdater::new(client.clone());
    let machine_update_runtime = MachineUpdateOperationRuntime::new(
        controllers.clone(),
        machine_updater,
        machine_update_tasks.clone(),
    );
    let machine_lifecycle_runtime = MachineLifecycleOperationRuntime::new(
        client.clone(),
        controllers.clone(),
        machine_roster.clone(),
        machine_lifecycle_tasks.clone(),
    );
    let operation_api = start_operation_api_service_with_handlers(
        client.clone(),
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
            client.clone(),
            machine_roster,
            facts,
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
        facts_cache,
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
    OpenCoreStore(CoreStoreError),
    StartFactsCache(RuntimeFactsCacheError),
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
            Self::OpenCoreStore(error) => {
                write!(formatter, "failed to open core state store: {error}")
            }
            Self::StartFactsCache(error) => {
                write!(formatter, "failed to start runtime facts cache: {error}")
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
                write!(formatter, "failed to start intent service: {error}")
            }
            Self::StartOperationApi(error) => {
                write!(formatter, "failed to start operation API service: {error}")
            }
            Self::ShutdownSignal(error) => {
                write!(formatter, "failed to wait for shutdown: {error}")
            }
            Self::ShutdownOperationApi(error) => {
                write!(formatter, "failed to stop operation API service: {error}")
            }
        }
    }
}

impl std::error::Error for ControlRuntimeError {}
