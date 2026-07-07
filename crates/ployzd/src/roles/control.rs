//! Process wiring for the control role.

use crate::adapters::nats_authorization::{
    MachineCredentialMint, MintResumeError, MintVerifyEndpoint, NatsAuthorizationWriter,
    NatsReloadRunner, RenderFailure, SystemctlNatsReloadRunner,
};
use crate::config::ControlProcessConfig;
use crate::core_store::{CoreStore, CoreStoreError};
use crate::fact_cache::{FactCache, FactCacheError, RunningFactCache, start_fact_cache};
use crate::intent::machine_roster::MachineRosterStore;
use crate::intent::namespace_intent::NamespaceIntentStore;
use crate::intent::nats_authorizations::{NatsAuthorizationStore, NatsAuthorizationStoreError};
use crate::intent::service::{NatsIntentReader, RunningIntentService, start_intent_service};
use crate::operation_api::admission::OperationControllers;
use crate::operation_api::service::{ApiServiceError, start_operation_api_service_with_handlers};
use crate::operation_api::{OperationApiHandlers, OperationWorkers};
use crate::operations::deploy::DeployMachineCandidates;
use crate::operations::deploy::driver::DeployOperationDriver;
use crate::operations::log::OperationRepository;
use crate::operations::machine_lifecycle::MachineLifecycleOperation;
use crate::operations::machine_update::MachineUpdateOperation;
use crate::process_support::shutdown_signal;
use crate::roles::machine::client::{
    NatsMachineFactsReader, NatsMachineLogsTailer, NatsMachineSubstrateUpdater,
};
use crate::roles::machine::intent_mirror::MachineIntentMirror;
use crate::seed::{SeedCoreError, seed_core_from_snapshot};
use crate::tasks::TaskRegistry;
use ployz_nats::connect::{NatsConnectError, connect_authenticated};
use ployz_nats::service_runtime::{NatsClient, NatsServiceShutdownError, RunningNatsService};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const CONTROL_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const INTENT_PUBLISH_INTERVAL: Duration = Duration::from_secs(30);
const REACHABILITY_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

pub struct RunningControlProcess {
    intent: RunningIntentService,
    operation_api: RunningNatsService,
    deploy_tasks: TaskRegistry,
    machine_update_tasks: TaskRegistry,
    machine_lifecycle_tasks: TaskRegistry,
    mint_tasks: TaskRegistry,
    reachability_tasks: TaskRegistry,
    facts_cache: RunningFactCache,
    authorization: NatsAuthorizationWriter,
}

impl RunningControlProcess {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        self.operation_api.shutdown().await?;
        self.intent.shutdown().await?;
        self.deploy_tasks.abort_all();
        self.machine_update_tasks.abort_all();
        self.machine_lifecycle_tasks.abort_all();
        self.mint_tasks.abort_all();
        self.reachability_tasks.abort_all();
        self.facts_cache.shutdown().await;
        self.authorization.shutdown();
        Ok(())
    }
}

/// Record each machine's advertised public endpoint onto the roster from the
/// public-IP testimony in the fact cache (ADR 0030). Runs on a slow tick; only
/// writes on change, never clears on a machine's silence — reachability is a
/// durable address property — and relies on the intent drumbeat to propagate the
/// update to every mirror.
async fn reconcile_reachability_loop(facts: FactCache, roster: MachineRosterStore) {
    let mut interval = tokio::time::interval(REACHABILITY_RECONCILE_INTERVAL);
    loop {
        interval.tick().await;
        for observation in facts.machine_public_ips() {
            let _ = roster
                .set_public_endpoint(&observation.machine_id, observation.public_ip)
                .await;
        }
    }
}

pub async fn start_control_process(
    config: &ControlProcessConfig,
) -> Result<RunningControlProcess, ControlProcessError> {
    let client = connect_authenticated(&config.nats_connect, CONTROL_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(ControlProcessError::ConnectNats)?;
    start_control_process_with_client_and_reload(client, config, SystemctlNatsReloadRunner).await
}

pub async fn start_control_process_with_client(
    client: NatsClient,
    config: &ControlProcessConfig,
) -> Result<RunningControlProcess, ControlProcessError> {
    start_control_process_with_client_and_reload(client, config, SystemctlNatsReloadRunner).await
}

pub async fn start_control_process_with_client_and_reload(
    client: NatsClient,
    config: &ControlProcessConfig,
    reload: impl NatsReloadRunner,
) -> Result<RunningControlProcess, ControlProcessError> {
    // A normal core needs a machine-add join template to admit new machines, so
    // fail fast if it is missing. A promoted core (one seeding from a mirror) is
    // allowed to start without one: recovery restores service first, and machine-add
    // rejects gracefully (admission.rs) until a template is configured.
    if config.seed_from_mirror.is_none() && config.machine_bootstrap.join_material.is_none() {
        return Err(ControlProcessError::MissingMachineJoinTemplate);
    }

    let core_store = CoreStore::open(config.core_db_path.clone())
        .await
        .map_err(ControlProcessError::OpenCoreStore)?;
    if let Some(mirror_path) = &config.seed_from_mirror {
        seed_core_from_mirror(&core_store, mirror_path).await?;
    }
    let repository = OperationRepository::open(core_store.clone(), client.clone());
    let controllers = OperationControllers::new(repository, config.machine_bootstrap.clone());
    // The grant store is the source of truth; the conf is its projection. On a
    // fresh core, import the keeper-written conf into the store once — before the
    // first render, or rendering the empty store would wipe the conf.
    let nats_authorizations = NatsAuthorizationStore::new(core_store.clone());
    nats_authorizations
        .seed_from_conf_if_empty(&config.nats_authorization.authorized_users_file)
        .await
        .map_err(ControlProcessError::SeedAuthorizations)?;
    let authorization = NatsAuthorizationWriter::start(
        config.nats_authorization.authorized_users_file.clone(),
        nats_authorizations,
        reload,
    );
    authorization
        .handle()
        .render(None)
        .await
        .map_err(ControlProcessError::RenderNatsAuthorization)?;
    // Start the facts cache only after authorization has rendered and
    // reloaded permissions: its subscription to plz.v1.testimony.* must not be
    // established before the grant exists, or NATS rejects it asynchronously
    // and the cache never resubscribes. Nothing between here and the
    // operation API consumes the cache, so this ordering is free.
    let facts_cache = start_fact_cache(client.clone())
        .await
        .map_err(ControlProcessError::StartFactsCache)?;
    let facts = facts_cache.cache();
    let deploy_tasks = TaskRegistry::default();
    let machine_update_tasks = TaskRegistry::default();
    let machine_lifecycle_tasks = TaskRegistry::default();
    let mint_tasks = TaskRegistry::default();
    let namespace_intent = NamespaceIntentStore::new(core_store.clone());
    let machine_roster = MachineRosterStore::new(core_store.clone());
    let reachability_tasks = TaskRegistry::default();
    reachability_tasks.spawn(reconcile_reachability_loop(
        facts.clone(),
        machine_roster.clone(),
    ));
    let deploy_driver = DeployOperationDriver::new(
        client.clone(),
        namespace_intent.clone(),
        controllers.clone(),
        DeployMachineCandidates::same_machines(config.deploy_machines.clone()),
        config.deploy_step_timeout,
        deploy_tasks.clone(),
    );
    let machine_mint = MachineCredentialMint::new(
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
        .map_err(ControlProcessError::ResumeMachineAddMints)?;
    let logs_tailer = NatsMachineLogsTailer::new(client.clone());
    let facts_reader = NatsMachineFactsReader::new(client.clone());
    let intent_reader = NatsIntentReader::new(client.clone());
    let intent = start_intent_service(
        client.clone(),
        machine_roster.clone(),
        namespace_intent,
        core_store.clone(),
        INTENT_PUBLISH_INTERVAL,
    )
    .await
    .map_err(ControlProcessError::StartIntent)?;
    let machine_updater = NatsMachineSubstrateUpdater::new(client.clone());
    let machine_update = MachineUpdateOperation::new(
        controllers.clone(),
        machine_updater,
        machine_update_tasks.clone(),
    );
    let machine_lifecycle = MachineLifecycleOperation::new(
        client.clone(),
        controllers.clone(),
        machine_roster.clone(),
        machine_lifecycle_tasks.clone(),
    );
    let operation_api = start_operation_api_service_with_handlers(
        client.clone(),
        OperationApiHandlers::execute_operations(
            controllers,
            OperationWorkers {
                deploy: deploy_driver,
                machine_update,
                machine_lifecycle,
                machine_mint,
            },
            config
                .deploy_machines
                .first()
                .cloned()
                .ok_or(ControlProcessError::MissingDeployMachine)?,
            client.clone(),
            machine_roster,
            facts,
            facts_reader,
            intent_reader,
            logs_tailer,
        ),
    )
    .await
    .map_err(ControlProcessError::StartOperationApi)?;

    Ok(RunningControlProcess {
        intent,
        operation_api,
        deploy_tasks,
        machine_update_tasks,
        machine_lifecycle_tasks,
        mint_tasks,
        reachability_tasks,
        facts_cache,
        authorization,
    })
}

pub async fn run_control_until_shutdown(
    config: &ControlProcessConfig,
) -> Result<(), ControlProcessError> {
    let runtime = start_control_process(config).await?;
    shutdown_signal()
        .await
        .map_err(ControlProcessError::ShutdownSignal)?;
    runtime
        .shutdown()
        .await
        .map_err(ControlProcessError::ShutdownOperationApi)
}

/// Seed a fresh core store from the machine's local intent mirror at promotion
/// (ADR 0031). Idempotent — the seed's own fresh-store guard makes a control
/// restart a no-op once the store has served as a core; a missing mirror is a
/// hard error (there is nothing to promote from).
async fn seed_core_from_mirror(
    core_store: &CoreStore,
    mirror_path: &Path,
) -> Result<(), ControlProcessError> {
    // Once the store has been promoted it is authoritative; skip the mirror entirely
    // so a later restart neither re-reads it nor bricks on a since-deleted mirror.
    // Only a fresh store needs seeding (seed_core_from_snapshot's own guard would
    // no-op an already-seeded store, but only *after* a load the mirror must survive).
    if core_store
        .control_plane_epoch_if_present()
        .await
        .map_err(|error| ControlProcessError::SeedCore(SeedCoreError::Epoch(error)))?
        .is_some()
    {
        return Ok(());
    }
    let snapshot = MachineIntentMirror::new(mirror_path.to_path_buf())
        .load()
        .ok_or_else(|| ControlProcessError::SeedMirrorMissing(mirror_path.to_path_buf()))?;
    seed_core_from_snapshot(core_store, &snapshot)
        .await
        .map_err(ControlProcessError::SeedCore)?;
    Ok(())
}

#[derive(Debug)]
pub enum ControlProcessError {
    MissingMachineJoinTemplate,
    MissingDeployMachine,
    ConnectNats(NatsConnectError),
    OpenCoreStore(CoreStoreError),
    SeedMirrorMissing(PathBuf),
    SeedCore(SeedCoreError),
    SeedAuthorizations(NatsAuthorizationStoreError),
    StartFactsCache(FactCacheError),
    RenderNatsAuthorization(RenderFailure),
    ResumeMachineAddMints(MintResumeError),
    StartIntent(ployz_nats::service_runtime::NatsServiceRuntimeError),
    StartOperationApi(ApiServiceError),
    ShutdownSignal(std::io::Error),
    ShutdownOperationApi(NatsServiceShutdownError),
}

impl fmt::Display for ControlProcessError {
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
            Self::SeedMirrorMissing(path) => {
                write!(
                    formatter,
                    "core-promote seed mirror is missing or unreadable: {}",
                    path.display()
                )
            }
            Self::SeedCore(error) => {
                write!(formatter, "failed to seed core store from mirror: {error}")
            }
            Self::SeedAuthorizations(error) => {
                write!(formatter, "failed to seed authorization grants: {error}")
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

impl std::error::Error for ControlProcessError {}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::state::{ControlPlaneEpoch, IntentSnapshot};

    fn empty_snapshot(epoch: ControlPlaneEpoch) -> IntentSnapshot {
        IntentSnapshot {
            epoch,
            active_machines: Vec::new(),
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            authorized_users: Vec::new(),
        }
    }

    #[tokio::test]
    async fn seeds_a_fresh_store_from_the_mirror() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror_path = dir.path().join("intent-mirror.json");
        MachineIntentMirror::new(mirror_path.clone())
            .store(&empty_snapshot(ControlPlaneEpoch::initial().next().next()))
            .expect("write mirror");

        let store = CoreStore::open_in_memory().await.expect("store");
        seed_core_from_mirror(&store, &mirror_path)
            .await
            .expect("seed succeeds");
        // Fence lands at max(mirror=3, fresh=1).next() = 4, above the succeeded core.
        assert_eq!(store.control_plane_epoch().await.expect("epoch").get(), 4);
    }

    #[tokio::test]
    async fn a_configured_but_missing_mirror_is_an_error() {
        let store = CoreStore::open_in_memory().await.expect("store");
        let error = seed_core_from_mirror(&store, Path::new("/no/such/intent-mirror.json"))
            .await
            .expect_err("missing mirror errors");
        assert!(matches!(error, ControlProcessError::SeedMirrorMissing(_)));
    }
}
