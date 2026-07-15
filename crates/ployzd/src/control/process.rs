//! Process wiring for the control role.

use crate::certificate::{AcmeIssuer, CertificateManager};
use crate::config::ControlProcessConfig;
use crate::control::authorization::{
    HostNatsReloadRunner, MachineCredentialMint, MintResumeError, MintVerifyEndpoint,
    NatsAuthorizationWriter, NatsReloadRunner, RenderFailure,
};
use crate::control::intent::ingress_intent::{
    IngressIntentStore, IngressProjectionStore, PloyzDnsTargetStore,
};
use crate::control::intent::machine_roster::MachineRosterStore;
use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::nats_authorizations::{
    NatsAuthorizationStore, NatsAuthorizationStoreError,
};
use crate::control::intent::service::{
    NatsIntentReader, RunningIntentService, start_intent_service,
};
use crate::control::operation_evidence::OperationRepository;
use crate::control::operations::credential_grant::CredentialGrantOperation;
use crate::control::operations::deploy::driver::{DeployOperationDriver, DeployOperationStores};
use crate::control::operations::ingress_configure::IngressConfigureOperation;
use crate::control::operations::machine_lifecycle::MachineLifecycleOperation;
use crate::control::operations::machine_update::MachineUpdateOperation;
use crate::control::operator_api::admission::OperationControllers;
use crate::control::operator_api::service::{
    ApiServiceError, start_operation_api_service_with_handlers,
};
use crate::control::operator_api::{OperationApiHandlers, OperationWorkers};
use crate::control::projection::ingress_endpoint::{
    IngressEndpointStartError, RunningIngressEndpointProjection, start_ingress_endpoint_projection,
};
use crate::control::projection::runtime::{
    RunningRuntimeProjection, RuntimeProjectionHealthState, start_runtime_projection,
};
use crate::control::reconciler::certificate::{
    CertificateRenewalHealth, CertificateRenewalHealthState, start_certificate_renewal_task,
};
use crate::control::reconciler::managed_dns::start_managed_dns_task;
use crate::control::role_client::machine::{
    NatsMachineFactsReader, NatsMachineLogsTailer, NatsMachineSubstrateUpdater,
};
use crate::control::store::{CoreStore, CoreStoreError};
use crate::lease::LeaseClient;
use crate::process_support::shutdown_signal;
use crate::recovery::{IntentMirror, PendingMachineJoinMirror};
use crate::role_testimony::{
    RoleTestimonyCache, RoleTestimonyCacheError, RunningRoleTestimonyCache,
    start_role_testimony_cache,
};
use crate::seed::{SeedCoreError, seed_core_from_snapshot};
use crate::tasks::TaskRegistry;
use ployz_core::state::{ControlPlaneEpoch, PendingMachineJoinRecovery};
use ployz_nats::connect::{NatsConnectError, connect_authenticated};
use ployz_nats::service_runtime::{NatsClient, NatsServiceShutdownError, RunningNatsService};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CONTROL_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const INTENT_PUBLISH_INTERVAL: Duration = Duration::from_secs(30);
const REACHABILITY_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

pub struct RunningControlProcess {
    intent: RunningIntentService,
    operation_api: RunningNatsService,
    credential_grant_tasks: TaskRegistry,
    ingress_configure_tasks: TaskRegistry,
    deploy_tasks: TaskRegistry,
    service_restart_tasks: TaskRegistry,
    namespace_remove_tasks: TaskRegistry,
    network_repair_tasks: TaskRegistry,
    volume_remove_tasks: TaskRegistry,
    machine_update_tasks: TaskRegistry,
    machine_lifecycle_tasks: TaskRegistry,
    mint_tasks: TaskRegistry,
    reachability_tasks: TaskRegistry,
    managed_dns_tasks: TaskRegistry,
    certificate_issuance_tasks: TaskRegistry,
    certificate_renewal_tasks: TaskRegistry,
    certificate_renewal_health: CertificateRenewalHealth,
    testimony_cache: RunningRoleTestimonyCache,
    runtime_projection: RunningRuntimeProjection,
    ingress_endpoint_projection: RunningIngressEndpointProjection,
    authorization: NatsAuthorizationWriter,
}

impl RunningControlProcess {
    #[must_use]
    pub fn certificate_renewal_health(&self) -> CertificateRenewalHealthState {
        self.certificate_renewal_health.snapshot()
    }

    #[must_use]
    pub fn runtime_projection_health(&self) -> RuntimeProjectionHealthState {
        self.runtime_projection.health()
    }

    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        self.operation_api.shutdown().await?;
        self.ingress_endpoint_projection.shutdown().await?;
        self.runtime_projection.shutdown().await?;
        self.intent.shutdown().await?;
        self.credential_grant_tasks.abort_all();
        self.ingress_configure_tasks.abort_all();
        self.deploy_tasks.abort_all();
        self.service_restart_tasks.abort_all();
        self.namespace_remove_tasks.abort_all();
        self.network_repair_tasks.abort_all();
        self.volume_remove_tasks.abort_all();
        self.machine_update_tasks.abort_all();
        self.machine_lifecycle_tasks.abort_all();
        self.mint_tasks.abort_all();
        self.reachability_tasks.abort_all();
        self.managed_dns_tasks.abort_all();
        self.certificate_issuance_tasks.abort_all();
        self.certificate_renewal_tasks.abort_all();
        self.testimony_cache.shutdown().await;
        self.authorization.shutdown();
        Ok(())
    }
}

/// Record each machine's advertised endpoints onto the roster from the machine
/// testimony in the role testimony cache (ADR 0030). Runs on a slow tick; only writes on
/// change, never clears on a machine's silence — reachability is a durable
/// address property — and relies on the intent drumbeat to propagate the update
/// to every mirror.
async fn reconcile_reachability_loop(facts: RoleTestimonyCache, roster: MachineRosterStore) {
    let mut interval = tokio::time::interval(REACHABILITY_RECONCILE_INTERVAL);
    loop {
        interval.tick().await;
        for observation in facts.machine_endpoint_observations() {
            let _ = roster
                .set_endpoints(
                    &observation.machine_id,
                    observation.control_endpoints,
                    observation.mesh_endpoints,
                )
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
    start_control_process_with_client_and_reload(client, config, HostNatsReloadRunner::default())
        .await
}

pub async fn start_control_process_with_client(
    client: NatsClient,
    config: &ControlProcessConfig,
) -> Result<RunningControlProcess, ControlProcessError> {
    start_control_process_with_client_and_reload(client, config, HostNatsReloadRunner::default())
        .await
}

pub async fn start_control_process_with_client_and_reload(
    client: NatsClient,
    config: &ControlProcessConfig,
    reload: impl NatsReloadRunner,
) -> Result<RunningControlProcess, ControlProcessError> {
    start_control_process_with_client_reload_and_issuer(client, config, reload, None).await
}

pub async fn start_control_process_with_client_and_test_issuer(
    client: NatsClient,
    config: &ControlProcessConfig,
    reload: impl NatsReloadRunner,
    issuer: Arc<dyn AcmeIssuer>,
) -> Result<RunningControlProcess, ControlProcessError> {
    start_control_process_with_client_reload_and_issuer(client, config, reload, Some(issuer)).await
}

async fn start_control_process_with_client_reload_and_issuer(
    client: NatsClient,
    config: &ControlProcessConfig,
    reload: impl NatsReloadRunner,
    certificate_issuer: Option<Arc<dyn AcmeIssuer>>,
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
    reject_stale_core_epoch(&core_store, config.epoch_fence_mirror.as_deref()).await?;
    let repository = OperationRepository::open(core_store.clone(), client.clone());
    let pending_join_recovery = if let Some(mirror_path) = &config.seed_from_mirror {
        seed_core_from_mirror(
            &core_store,
            mirror_path,
            &config.certificate_manager.state_dir,
        )
        .await?;
        load_pending_join_recovery(mirror_path)
    } else {
        Vec::new()
    };
    let controllers = if pending_join_recovery.is_empty() {
        OperationControllers::new(repository, config.machine_bootstrap.clone())
    } else {
        OperationControllers::new_with_pending_join_recovery(
            repository,
            config.machine_bootstrap.clone(),
            pending_join_recovery,
        )
    };
    // The grant store is the source of truth; the conf is its projection. On a
    // fresh core, import the Host Runner-written conf into the store once — before the
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
    // Start the role testimony cache only after authorization has rendered and
    // reloaded permissions: its subscription to plz.v1.testimony.* must not be
    // established before the grant exists, or NATS rejects it asynchronously
    // and the cache never resubscribes. Nothing between here and the
    // operation API consumes the cache, so this ordering is free.
    let testimony_cache = start_role_testimony_cache(client.clone())
        .await
        .map_err(ControlProcessError::StartFactsCache)?;
    let facts = testimony_cache.cache();
    let deploy_tasks = TaskRegistry::default();
    let credential_grant_tasks = TaskRegistry::default();
    let ingress_configure_tasks = TaskRegistry::default();
    let service_restart_tasks = TaskRegistry::default();
    let namespace_remove_tasks = TaskRegistry::default();
    let network_repair_tasks = TaskRegistry::default();
    let volume_remove_tasks = TaskRegistry::default();
    let machine_update_tasks = TaskRegistry::default();
    let machine_lifecycle_tasks = TaskRegistry::default();
    let mint_tasks = TaskRegistry::default();
    let namespace_intent = NamespaceIntentStore::new(core_store.clone());
    let ployz_dns_target = PloyzDnsTargetStore::new(core_store.clone());
    let lease_client = LeaseClient::new(config.lease_worker_url.clone());
    let managed_dns_tasks = TaskRegistry::default();
    let certificate_issuance_tasks = TaskRegistry::default();
    let certificate_renewal_tasks = TaskRegistry::default();
    let (certificate_wake, certificate_wake_rx) = tokio::sync::mpsc::channel(1);
    let certificate_manager = match certificate_issuer {
        Some(issuer) => CertificateManager::with_issuer(
            core_store.clone(),
            client.clone(),
            config.certificate_manager.clone(),
            issuer,
        ),
        None => CertificateManager::new(
            core_store.clone(),
            client.clone(),
            config.certificate_manager.clone(),
        ),
    }
    .with_task_registry(certificate_issuance_tasks.clone());
    let machine_roster = MachineRosterStore::new(core_store.clone());
    let reachability_tasks = TaskRegistry::default();
    reachability_tasks.spawn(reconcile_reachability_loop(
        facts.clone(),
        machine_roster.clone(),
    ));
    let deploy_driver = DeployOperationDriver::new(
        DeployOperationStores {
            intent_change_client: client.clone(),
            namespace_intent: namespace_intent.clone(),
            ployz_dns_target: ployz_dns_target.clone(),
            ingress_projection: IngressProjectionStore::new(core_store.clone()),
            controllers: controllers.clone(),
        },
        certificate_manager.clone(),
        config.deploy_step_timeout,
        deploy_tasks.clone(),
    );
    let service_restart = crate::control::operations::service_restart::ServiceRestartOperation::new(
        client.clone(),
        controllers.clone(),
        config.deploy_step_timeout,
        service_restart_tasks.clone(),
    );
    let namespace_remove =
        crate::control::operations::namespace_remove::NamespaceRemoveOperation::new(
            client.clone(),
            namespace_intent.clone(),
            controllers.clone(),
            config.deploy_step_timeout,
            namespace_remove_tasks.clone(),
        );
    let volume_remove = crate::control::operations::volume_remove::VolumeRemoveOperation::new(
        client.clone(),
        namespace_intent.clone(),
        controllers.clone(),
        config.deploy_step_timeout,
        volume_remove_tasks.clone(),
    );
    let machine_mint = MachineCredentialMint::new(
        controllers.clone(),
        authorization.handle(),
        MintVerifyEndpoint::from_connect(&config.nats_connect),
        config.nats_authorization.machine_seed_file.clone(),
        mint_tasks.clone(),
    );
    let credential_grant = CredentialGrantOperation::new(
        controllers.clone(),
        authorization.handle(),
        client.clone(),
        credential_grant_tasks.clone(),
    );
    let ingress_configure = IngressConfigureOperation::new(
        controllers.clone(),
        IngressIntentStore::new(core_store.clone()),
        client.clone(),
        ingress_configure_tasks.clone(),
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
    let network_repair = crate::control::operations::network_repair::NetworkRepairOperation::new(
        controllers.clone(),
        intent_reader
            .clone()
            .with_request_timeout(config.deploy_step_timeout),
        NatsMachineFactsReader::new(client.clone())
            .with_request_timeout(config.deploy_step_timeout),
        client.clone(),
        config.deploy_step_timeout,
        network_repair_tasks.clone(),
    );
    let core_machine_id = config
        .deploy_machines
        .first()
        .cloned()
        .ok_or(ControlProcessError::MissingDeployMachine)?;
    start_managed_dns_task(
        &managed_dns_tasks,
        client.clone(),
        IngressIntentStore::new(core_store.clone()),
        ployz_dns_target.clone(),
        controllers.repository().clone(),
        lease_client.clone(),
        certificate_wake,
    );
    let certificate_renewal_health = start_certificate_renewal_task(
        &certificate_renewal_tasks,
        certificate_manager,
        NatsMachineFactsReader::new(client.clone()),
        machine_roster.clone(),
        ployz_dns_target,
        lease_client,
        certificate_wake_rx,
    );
    let intent = start_intent_service(
        client.clone(),
        core_machine_id.clone(),
        namespace_intent,
        core_store.clone(),
        INTENT_PUBLISH_INTERVAL,
    )
    .await
    .map_err(ControlProcessError::StartIntent)?;
    let ingress_endpoint_projection = start_ingress_endpoint_projection(
        client.clone(),
        IngressProjectionStore::new(core_store.clone()),
        core_store
            .control_plane_epoch()
            .await
            .map_err(ControlProcessError::ReadCoreEpoch)?,
        controllers.repository().clone(),
    )
    .await
    .map_err(ControlProcessError::StartIngressEndpointProjection)?;
    let runtime_projection = start_runtime_projection(
        client.clone(),
        intent_reader.clone(),
        facts.clone(),
        core_store.clone(),
    )
    .await
    .map_err(|error| ControlProcessError::StartRuntimeProjection {
        message: error.to_string(),
    })?;
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
                credential_grant,
                ingress_configure,
                deploy: deploy_driver,
                service_restart,
                namespace_remove,
                network_repair,
                volume_remove,
                machine_update,
                machine_lifecycle,
                machine_mint,
            },
            config.dataplane_endpoint_supernet.clone(),
            core_store.clone(),
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
        credential_grant_tasks,
        ingress_configure_tasks,
        deploy_tasks,
        service_restart_tasks,
        namespace_remove_tasks,
        network_repair_tasks,
        volume_remove_tasks,
        machine_update_tasks,
        machine_lifecycle_tasks,
        mint_tasks,
        reachability_tasks,
        managed_dns_tasks,
        certificate_issuance_tasks,
        certificate_renewal_tasks,
        certificate_renewal_health,
        testimony_cache,
        runtime_projection,
        ingress_endpoint_projection,
        authorization,
    })
}

pub async fn run_control_until_shutdown(
    config: &ControlProcessConfig,
) -> Result<(), ControlProcessError> {
    let shutdown = shutdown_signal().map_err(ControlProcessError::ShutdownSignal)?;
    let runtime = start_control_process(config).await?;
    shutdown
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
    certificate_state_dir: &Path,
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
    let snapshot = IntentMirror::new(mirror_path.to_path_buf())
        .load()
        .ok_or_else(|| ControlProcessError::SeedMirrorMissing(mirror_path.to_path_buf()))?;
    seed_core_from_snapshot(core_store, &snapshot, certificate_state_dir)
        .await
        .map_err(ControlProcessError::SeedCore)?;
    Ok(())
}

async fn reject_stale_core_epoch(
    core_store: &CoreStore,
    mirror_path: Option<&Path>,
) -> Result<(), ControlProcessError> {
    let Some(mirror_path) = mirror_path else {
        return Ok(());
    };
    let Some(local) = core_store
        .control_plane_epoch_if_present()
        .await
        .map_err(ControlProcessError::ReadCoreEpoch)?
    else {
        return Ok(());
    };
    let Some(snapshot) = IntentMirror::new(mirror_path.to_path_buf()).load() else {
        return Ok(());
    };
    if snapshot.epoch > local {
        return Err(ControlProcessError::StaleCoreEpoch {
            local,
            mirror: snapshot.epoch,
            mirror_path: mirror_path.to_path_buf(),
        });
    }
    Ok(())
}

/// Pending machine-add recovery hints trusted at promotion. The pending mirror
/// is only believed when its epoch matches the intent mirror the core seeded
/// from, and never for a machine the seeded roster already holds active —
/// replaying such a hint would let a stale join overwrite roster truth.
fn load_pending_join_recovery(mirror_path: &Path) -> Vec<PendingMachineJoinRecovery> {
    let Some(intent) = IntentMirror::new(mirror_path.to_path_buf()).load() else {
        return Vec::new();
    };
    let Some(snapshot) =
        PendingMachineJoinMirror::new(mirror_path.with_file_name("pending-machine-joins.json"))
            .load()
    else {
        return Vec::new();
    };
    if snapshot.epoch != intent.epoch {
        eprintln!(
            "ployzd control warning: phase=pending-join-recovery ignoring pending-join mirror whose epoch does not match the seeded intent pending_epoch={} intent_epoch={}",
            snapshot.epoch.get(),
            intent.epoch.get()
        );
        return Vec::new();
    }
    snapshot
        .pending
        .into_iter()
        .filter(|recovery| {
            let already_active = intent
                .active_machines
                .iter()
                .any(|machine| machine.machine_id == recovery.machine_id);
            if already_active {
                eprintln!(
                    "ployzd control warning: phase=pending-join-recovery skipping pending join for machine already active in the seeded roster machine_id={}",
                    recovery.machine_id.as_str()
                );
            }
            !already_active
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ControlProcessError {
    #[error("machine add requires configured join template")]
    MissingMachineJoinTemplate,
    #[error("control runtime requires a deploy machine")]
    MissingDeployMachine,
    #[error("{0}")]
    ConnectNats(NatsConnectError),
    #[error("failed to open core state store: {0}")]
    OpenCoreStore(CoreStoreError),
    #[error("failed to read core epoch: {0}")]
    ReadCoreEpoch(CoreStoreError),
    #[error(
        "refusing stale core startup: local epoch {} is behind mirrored epoch {} at {}",
        local.get(),
        mirror.get(),
        mirror_path.display()
    )]
    StaleCoreEpoch {
        local: ControlPlaneEpoch,
        mirror: ControlPlaneEpoch,
        mirror_path: PathBuf,
    },
    #[error("core-promote seed mirror is missing or unreadable: {}", .0.display())]
    SeedMirrorMissing(PathBuf),
    #[error("failed to seed core store from mirror: {0}")]
    SeedCore(SeedCoreError),
    #[error("failed to seed authorization grants: {0}")]
    SeedAuthorizations(NatsAuthorizationStoreError),
    #[error("failed to start role testimony cache: {0}")]
    StartFactsCache(RoleTestimonyCacheError),
    #[error("failed to render NATS authorization: {0}")]
    RenderNatsAuthorization(RenderFailure),
    #[error("failed to reconcile unfinished machine-add mints: {0}")]
    ResumeMachineAddMints(MintResumeError),
    #[error("failed to start intent service: {0}")]
    StartIntent(ployz_nats::service_runtime::NatsServiceRuntimeError),
    #[error("failed to start ingress endpoint projection: {0}")]
    StartIngressEndpointProjection(IngressEndpointStartError),
    #[error("failed to start runtime projection: {message}")]
    StartRuntimeProjection { message: String },
    #[error("failed to start operation API service: {0}")]
    StartOperationApi(ApiServiceError),
    #[error("failed to wait for shutdown: {0}")]
    ShutdownSignal(std::io::Error),
    #[error("failed to stop operation API service: {0}")]
    ShutdownOperationApi(NatsServiceShutdownError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::OperationId;
    use ployz_core::machine::{IssuedJoinToken, JoinTokenExpiresAt, MachineName, RawJoinToken};
    use ployz_core::roles::InstallRolePolicy;
    use ployz_core::state::{
        ActiveMachineState, ControlPlaneEpoch, IntentSnapshot, MachineLifecycle,
        PendingMachineJoinRecoverySnapshot,
    };
    use ployz_test_support::ids::machine_id;

    fn empty_snapshot(epoch: ControlPlaneEpoch) -> IntentSnapshot {
        IntentSnapshot {
            epoch,
            core_machine_id: machine_id("machine_a"),
            active_machines: Vec::new(),
            dataplane_projection: ployz_core::dataplane::DataplaneProjection::try_new(
                Vec::new(),
                None,
            )
            .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            volume_pins: Vec::new(),
            nats_authorizations: Vec::new(),
            automatic_hostname_configuration:
                ployz_core::ingress::AutomaticHostnameConfiguration::Ployz,
            ployz_dns_target: ployz_core::ingress::PloyzDnsTargetIntent::Enabled,
            active_certificates: Vec::new(),
        }
    }

    fn pending_join(machine_id_value: &str) -> PendingMachineJoinRecovery {
        let raw_join_token =
            RawJoinToken::try_new(format!("join_{machine_id_value}")).expect("valid join token");
        PendingMachineJoinRecovery {
            machine_id: machine_id(machine_id_value),
            name: MachineName::try_new(machine_id_value).expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
            endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new(
                ployz_core::dataplane::default_endpoint_subnet(&machine_id(machine_id_value)),
            )
            .expect("valid endpoint subnet"),
            join_token: IssuedJoinToken::new(
                raw_join_token.fingerprint().expect("fingerprint"),
                JoinTokenExpiresAt::try_new(4_102_444_800).expect("valid expiry"),
            ),
        }
    }

    fn active_machine(machine_id_value: &str) -> ActiveMachineState {
        ActiveMachineState {
            machine_id: machine_id(machine_id_value),
            name: MachineName::try_new(machine_id_value).expect("valid machine name"),
            activated_by: OperationId::try_new(format!("op_add_{machine_id_value}"))
                .expect("valid operation id"),
            roles: InstallRolePolicy::install_all(),
            lifecycle: MachineLifecycle::Active,
            control_endpoints: Vec::new(),
            mesh_endpoints: Vec::new(),
            endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new("10.198.0.0/24")
                .expect("valid endpoint subnet"),
            wireguard_public_key: ployz_core::dataplane::WireGuardPublicKey::try_new(format!(
                "public-{machine_id_value}"
            ))
            .expect("public key"),
        }
    }

    #[tokio::test]
    async fn seeds_a_fresh_store_from_the_mirror() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror_path = dir.path().join("intent-mirror.json");
        IntentMirror::new(mirror_path.clone())
            .store(&empty_snapshot(ControlPlaneEpoch::initial().next().next()))
            .expect("write mirror");

        let store = CoreStore::open_in_memory().await.expect("store");
        seed_core_from_mirror(&store, &mirror_path, dir.path())
            .await
            .expect("seed succeeds");
        // Fence lands at max(mirror=3, fresh=1).next() = 4, above the succeeded core.
        assert_eq!(store.control_plane_epoch().await.expect("epoch").get(), 4);
    }

    #[tokio::test]
    async fn a_configured_but_missing_mirror_is_an_error() {
        let store = CoreStore::open_in_memory().await.expect("store");
        let error = seed_core_from_mirror(
            &store,
            Path::new("/no/such/intent-mirror.json"),
            Path::new("/tmp"),
        )
        .await
        .expect_err("missing mirror errors");
        assert!(matches!(error, ControlProcessError::SeedMirrorMissing(_)));
    }

    #[test]
    fn pending_join_mirror_with_stale_epoch_is_ignored() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror_path = dir.path().join("intent-mirror.json");
        IntentMirror::new(mirror_path.clone())
            .store(&empty_snapshot(ControlPlaneEpoch::initial().next()))
            .expect("write intent mirror");
        PendingMachineJoinMirror::new(dir.path().join("pending-machine-joins.json"))
            .store(&PendingMachineJoinRecoverySnapshot {
                epoch: ControlPlaneEpoch::initial(),
                pending: vec![pending_join("machine_b")],
            })
            .expect("write pending join mirror");

        assert!(load_pending_join_recovery(&mirror_path).is_empty());
    }

    #[test]
    fn pending_join_for_machine_already_active_in_seeded_roster_is_skipped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror_path = dir.path().join("intent-mirror.json");
        let mut snapshot = empty_snapshot(ControlPlaneEpoch::initial());
        snapshot.active_machines = vec![active_machine("machine_b")];
        IntentMirror::new(mirror_path.clone())
            .store(&snapshot)
            .expect("write intent mirror");
        PendingMachineJoinMirror::new(dir.path().join("pending-machine-joins.json"))
            .store(&PendingMachineJoinRecoverySnapshot {
                epoch: ControlPlaneEpoch::initial(),
                pending: vec![pending_join("machine_b"), pending_join("machine_c")],
            })
            .expect("write pending join mirror");

        let recovered = load_pending_join_recovery(&mirror_path);

        let [recovery] = recovered.as_slice() else {
            panic!("exactly the not-yet-active machine is recovered: {recovered:?}");
        };
        assert_eq!(recovery.machine_id, machine_id("machine_c"));
    }

    #[tokio::test]
    async fn refuses_startup_when_local_core_epoch_is_behind_machine_mirror() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror_path = dir.path().join("intent-mirror.json");
        IntentMirror::new(mirror_path.clone())
            .store(&empty_snapshot(ControlPlaneEpoch::initial().next()))
            .expect("write mirror");
        let store = CoreStore::open_in_memory().await.expect("store");
        assert_eq!(
            store.control_plane_epoch().await.expect("epoch"),
            ControlPlaneEpoch::initial()
        );

        let error = reject_stale_core_epoch(&store, Some(&mirror_path))
            .await
            .expect_err("stale core rejected");

        assert!(matches!(error, ControlProcessError::StaleCoreEpoch { .. }));
    }

    #[tokio::test]
    async fn allows_startup_when_local_core_epoch_matches_machine_mirror() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror_path = dir.path().join("intent-mirror.json");
        IntentMirror::new(mirror_path.clone())
            .store(&empty_snapshot(ControlPlaneEpoch::initial()))
            .expect("write mirror");
        let store = CoreStore::open_in_memory().await.expect("store");
        assert_eq!(
            store.control_plane_epoch().await.expect("epoch"),
            ControlPlaneEpoch::initial()
        );

        reject_stale_core_epoch(&store, Some(&mirror_path))
            .await
            .expect("matching epoch allowed");
    }
}
