//! Keeper lifecycle and bounded roster reconciliation.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use ployz_core::corrosion::{
    BuiltinWireguardLocalSubnetReallocation, BuiltinWireguardMeshOutcome,
    ContainerIsolationDegradationReason, ContainerIsolationTestimony,
    DesiredBuiltinWireguardMachinePeer, DesiredContainerIsolation, EbpfMeshDegradationReason,
    EbpfMeshDegraded, MachineTransport, MeshComponentDegraded, MeshComponentNotAttempted,
    MeshComponentReady, MeshConvergenceTestimony, MeshDegradation, MeshNotAttemptedReason,
    OperatorWriteProvenance, Principal, WireGuardHandshakeEvidence, project_builtin_wireguard_mesh,
    project_container_isolation,
};
use ployz_core::ids::MachineRowId;
use ployz_core::network::WireGuardPublicKey;
use ployz_core::roles::PloyzdRole;
use ployz_host_runner::SupervisorBackend;
use ployz_host_runner::builtin_wireguard::{
    BuiltinWireguardLatestHandshake, EbpfDegradedReason, EbpfHostOutcome,
};
use ployz_host_runner::container_isolation::ContainerIsolationHostError;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::corrosion::CorrosionClient;

use super::provider::{BoundKeeperIdentity, KeeperMeshProvider, KeeperProviderError};
use super::status::{LocalMachineStatusWriter, MachineStatusWriteError, now};
use super::store::{KeeperCorrosion, KeeperStoreError};
use super::upgrade::{KeeperUpgradeSocket, restart_systemd_role};
use super::{KeeperRoleConfig, KeeperRoleConfigError};

const SUBNET_REPAIR_COURTESY_DELAY: Duration = Duration::from_secs(1);
const UPGRADE_FIRST_CONVERGE_TIMEOUT: Duration = Duration::from_secs(60);
const CONTAINER_INVALIDATION_COALESCE: Duration = Duration::from_millis(250);

/// Runs Keeper from the supervisor-owned environment until process shutdown.
pub async fn run_from_environment() -> Result<(), KeeperRoleRuntimeError> {
    let config =
        KeeperRoleConfig::from_environment().map_err(KeeperRoleRuntimeError::Configuration)?;
    let provider = KeeperMeshProvider::from_config(&config);
    let corrosion = CorrosionClient::new(config.corrosion().clone())
        .map_err(KeeperRoleRuntimeError::CorrosionConfiguration)?;
    let store = KeeperCorrosion::new(
        corrosion,
        config.cluster_id().clone(),
        config.local_machine_id().clone(),
    );
    if config.supervisor() == SupervisorBackend::OpenRc {
        return run_keeper(config, provider, store, wait_for_process_shutdown()).await;
    }

    let socket = KeeperUpgradeSocket::bind(config.upgrade_socket_path())
        .await
        .map_err(|error| KeeperRoleRuntimeError::UpgradeSocket {
            detail: error.to_string(),
        })?;
    let (stop, _) = watch::channel(false);
    let socket_store = config.upgrade_store().clone();
    let socket_timeout = config.host().command_timeout();
    let socket_shutdown = stop.subscribe();
    let socket_server = socket.serve(socket_store, socket_timeout, socket_shutdown);
    tokio::pin!(socket_server);
    let keeper = run_keeper(
        config,
        provider,
        store,
        wait_for_process_or_upgrade_shutdown(stop.subscribe()),
    );
    tokio::pin!(keeper);

    tokio::select! {
        result = &mut keeper => {
            let _ = stop.send(true);
            let _ = socket_server.await;
            result
        }
        result = &mut socket_server => {
            let _ = stop.send(true);
            let _ = keeper.await;
            match result {
                Ok(()) => Err(KeeperRoleRuntimeError::UpgradeSocketStopped),
                Err(error) => Err(KeeperRoleRuntimeError::UpgradeSocket {
                    detail: error.to_string(),
                }),
            }
        }
    }
}

async fn wait_for_process_or_upgrade_shutdown(mut stop: watch::Receiver<bool>) {
    tokio::select! {
        () = wait_for_process_shutdown() => {}
        changed = stop.changed() => {
            if let Err(error) = changed {
                tracing::debug!(error = %error, "Keeper upgrade shutdown channel closed");
            }
        }
    }
}

async fn run_keeper<Shutdown>(
    config: KeeperRoleConfig,
    provider: KeeperMeshProvider,
    store: KeeperCorrosion,
    shutdown: Shutdown,
) -> Result<(), KeeperRoleRuntimeError>
where
    Shutdown: Future<Output = ()>,
{
    let mut pending_upgrade = config
        .upgrade_store()
        .pending_upgrade()
        .map_err(KeeperRoleRuntimeError::UpgradeArtifactStore)?;
    let upgrade_deadline = pending_upgrade
        .as_ref()
        .map(|_| Instant::now() + UPGRADE_FIRST_CONVERGE_TIMEOUT);
    // This happens before the first Corrosion request so the local sidecar can
    // bind its gossip socket to deterministic mesh identity.
    let bound =
        provider
            .bind_ip(config.cluster_id())
            .await
            .map_err(|error| match pending_upgrade.as_ref() {
                Some(_) => KeeperRoleRuntimeError::UpgradeFirstConverge {
                    detail: error.to_string(),
                },
                None => KeeperRoleRuntimeError::provider(error),
            })?;
    tracing::info!(
        public_key = %bound.public_key.as_str(),
        bind_address = %bound.evidence.bind_address,
        "Keeper bound its local builtin mesh identity"
    );

    let writer = LocalMachineStatusWriter::new(
        config.cluster_id().clone(),
        config.local_machine_id().clone(),
        config.corrosion_version().to_owned(),
    );
    let mut last_successful_converge = None;
    let mut isolation = IsolationCache::default();
    let mut fold = FoldKind::Full;
    let mut invalidations = None;
    let mut periodic = tokio::time::interval(config.reconcile_interval());
    periodic.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume the immediate first tick; the explicit initial reconcile below
    // owns the startup fold.
    periodic.tick().await;
    let mut retry = RetryDelay::new(config.retry_initial(), config.retry_max());
    let mut subscription_retry = RetryDelay::new(config.retry_initial(), config.retry_max());
    tokio::pin!(shutdown);
    loop {
        if let Some(error) = first_converge_timeout(
            pending_upgrade.is_some(),
            upgrade_deadline,
            "first convergence deadline elapsed",
        ) {
            return Err(error);
        }
        if invalidations.is_none() {
            match store.subscribe_invalidations().await {
                Ok(streams) => {
                    invalidations = Some(streams);
                    subscription_retry.reset();
                    // Every subscription is registered before the following
                    // Full fold queries truth. Their streams retain changes
                    // that arrive during that startup or reconnect barrier.
                    fold = subscription_barrier_fold();
                }
                Err(error) => {
                    let delay = subscription_retry.next();
                    tracing::warn!(error = %error, ?delay, "Keeper subscriptions disconnected; backing off before a full re-query");
                    tokio::select! {
                        () = &mut shutdown => return Ok(()),
                        () = tokio::time::sleep(delay) => {}
                    }
                    continue;
                }
            }
        }
        let result = match fold {
            FoldKind::Full => {
                reconcile_once(
                    &config,
                    &provider,
                    &store,
                    &writer,
                    &bound,
                    &mut last_successful_converge,
                    &mut isolation,
                )
                .await
            }
            FoldKind::IsolationOnly => {
                reconcile_isolation_only(&provider, &store, &writer, &mut isolation).await
            }
        };
        match result {
            Ok(ReconcileProgress::Settled) => {
                if pending_upgrade.is_some() {
                    confirm_upgraded_keeper(&config).await?;
                    pending_upgrade = None;
                }
                retry.reset();
            }
            Ok(ReconcileProgress::Requery) => {
                retry.reset();
                fold = FoldKind::Full;
                continue;
            }
            Err(ReconcileError::Retry { detail }) => {
                if let Some(error) =
                    first_converge_timeout(pending_upgrade.is_some(), upgrade_deadline, &detail)
                {
                    return Err(error);
                }
                let delay = retry.next();
                fold = fold_after_retry(fold);
                tracing::warn!(error = %detail, ?delay, "Keeper convergence will retry");
                tokio::select! {
                    () = &mut shutdown => return Ok(()),
                    () = tokio::time::sleep(delay) => {}
                }
                continue;
            }
            Err(ReconcileError::Fatal(error)) => {
                return Err(first_converge_failure(pending_upgrade.is_some(), error));
            }
        }

        let Some(streams) = invalidations.as_mut() else {
            unreachable!("invalidation streams were installed")
        };
        enum Wake {
            Shutdown,
            Full(Result<(), KeeperStoreError>),
            Containers(Result<(), KeeperStoreError>),
        }
        let wake = tokio::select! {
            () = &mut shutdown => Wake::Shutdown,
            _ = periodic.tick() => Wake::Full(Ok(())),
            result = streams.roster.next() => Wake::Full(result),
            result = streams.containers.next() => Wake::Containers(result),
        };
        match wake {
            Wake::Shutdown => return Ok(()),
            Wake::Full(Ok(())) => fold = FoldKind::Full,
            Wake::Full(Err(error)) | Wake::Containers(Err(error)) => {
                tracing::warn!(error = %error, "Keeper subscription disconnected; forcing full re-query");
                invalidations = None;
                fold = FoldKind::Full;
            }
            Wake::Containers(Ok(())) => {
                fold = FoldKind::IsolationOnly;
                let deadline = tokio::time::sleep(CONTAINER_INVALIDATION_COALESCE);
                tokio::pin!(deadline);
                loop {
                    let wake = tokio::select! {
                        () = &mut shutdown => Wake::Shutdown,
                        _ = periodic.tick() => Wake::Full(Ok(())),
                        result = streams.roster.next() => Wake::Full(result),
                        result = streams.containers.next() => Wake::Containers(result),
                        () = &mut deadline => break,
                    };
                    match wake {
                        Wake::Shutdown => return Ok(()),
                        Wake::Full(Ok(())) => {
                            fold = preempt_fold(fold, FoldKind::Full);
                            break;
                        }
                        Wake::Full(Err(error)) | Wake::Containers(Err(error)) => {
                            tracing::warn!(error = %error, "Keeper subscription disconnected during container coalescing");
                            invalidations = None;
                            fold = FoldKind::Full;
                            break;
                        }
                        Wake::Containers(Ok(())) => {
                            fold = preempt_fold(fold, FoldKind::IsolationOnly);
                        }
                    }
                }
            }
        }
    }
}

fn first_converge_failure(
    upgrade_pending: bool,
    error: KeeperRoleRuntimeError,
) -> KeeperRoleRuntimeError {
    if upgrade_pending {
        KeeperRoleRuntimeError::UpgradeFirstConverge {
            detail: error.to_string(),
        }
    } else {
        error
    }
}

fn first_converge_timeout(
    upgrade_pending: bool,
    deadline: Option<Instant>,
    detail: &str,
) -> Option<KeeperRoleRuntimeError> {
    if upgrade_pending && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Some(KeeperRoleRuntimeError::UpgradeFirstConverge {
            detail: detail.to_owned(),
        })
    } else {
        None
    }
}

async fn confirm_upgraded_keeper(config: &KeeperRoleConfig) -> Result<(), KeeperRoleRuntimeError> {
    if config.supervisor() != SupervisorBackend::Systemd {
        return Err(KeeperRoleRuntimeError::UpgradeFirstConverge {
            detail: "an armed upgrade requires systemd".to_owned(),
        });
    }
    for role in [PloyzdRole::Api, PloyzdRole::Gateway, PloyzdRole::Dns] {
        restart_systemd_role(role, config.host().command_timeout())
            .await
            .map_err(|detail| KeeperRoleRuntimeError::UpgradeFirstConverge { detail })?;
    }
    config.upgrade_store().confirm_armed().map_err(|error| {
        KeeperRoleRuntimeError::UpgradeFirstConverge {
            detail: error.to_string(),
        }
    })
}

async fn reconcile_once(
    config: &KeeperRoleConfig,
    provider: &KeeperMeshProvider,
    store: &KeeperCorrosion,
    writer: &LocalMachineStatusWriter,
    bound: &BoundKeeperIdentity,
    last_successful_converge: &mut Option<ployz_core::corrosion::CorrosionTimestamp>,
    isolation: &mut IsolationCache,
) -> Result<ReconcileProgress, ReconcileError> {
    let snapshot = store.read_roster().await.map_err(retry_store)?;
    let container_rows = store.read_containers().await;
    if last_successful_converge.is_none() {
        *last_successful_converge = snapshot
            .local_status
            .as_ref()
            .and_then(last_success_from_status);
    }
    isolation.seed(snapshot.local_status.as_ref());
    let carried_isolation = snapshot
        .local_status
        .as_ref()
        .and_then(|status| status.container_isolation.clone())
        .or_else(|| isolation.testimony.clone());
    let isolation_projection = container_rows
        .map(|rows| project_container_isolation(snapshot.cluster.prefix.clone(), rows));
    isolation.eligible = false;
    let observed_local_machine_document = snapshot
        .machines
        .accepted
        .iter()
        .find(|row| row.source.key == config.local_machine_id().as_str())
        .map(|row| row.source.document.clone());
    let cluster_prefix = snapshot.cluster.prefix.clone();
    let outcome = project_builtin_wireguard_mesh(
        &snapshot.cluster,
        config.local_machine_id().clone(),
        &bound.public_key,
        snapshot.machines,
        snapshot.peers,
    )
    .map_err(|source| ReconcileError::Fatal(KeeperRoleRuntimeError::Projection(source)))?;
    match outcome {
        BuiltinWireguardMeshOutcome::NoRoster { evidence } => {
            tracing::info!(?evidence, "Keeper observed an empty builtin mesh roster");
            Ok(ReconcileProgress::Settled)
        }
        BuiltinWireguardMeshOutcome::Fenced {
            local_machine_id,
            reason,
            evidence,
        } => {
            tracing::warn!(%local_machine_id, ?reason, ?evidence, "local machine is fenced from the accepted roster");
            provider.fence(bound).await.map_err(provider_failure)?;
            // The machine row sweep owns stale status removal. A fenced Keeper
            // must not recreate testimony for an identity no longer accepted.
            Ok(ReconcileProgress::Settled)
        }
        BuiltinWireguardMeshOutcome::KeyMismatch {
            mismatches,
            evidence,
        } => {
            tracing::warn!(
                ?mismatches,
                ?evidence,
                "Keeper refused a roster with mismatched WireGuard identities"
            );
            let attempted_at = now().map_err(retry_status)?;
            write_testimony(
                store,
                writer,
                Some(MeshConvergenceTestimony::KeyMismatch {
                    attempted_at,
                    mismatches,
                }),
                carried_isolation,
                None,
            )
            .await?;
            Ok(ReconcileProgress::Settled)
        }
        BuiltinWireguardMeshOutcome::ReallocateLocalContainerSubnet { repair, evidence } => {
            tracing::warn!(machine_id = %repair.machine_id(), occupied_subnets = repair.occupied_subnets().len(), ?evidence, "Keeper must repair its lost local container subnet claim");
            let Some(observed_document) = observed_local_machine_document else {
                return Err(ReconcileError::Fatal(KeeperRoleRuntimeError::Invariant(
                    "subnet repair did not retain the accepted local machine row evidence",
                )));
            };
            repair_local_subnet(store, cluster_prefix, repair, &observed_document).await?;
            tokio::time::sleep(SUBNET_REPAIR_COURTESY_DELAY).await;
            let courtesy = store.read_roster().await.map_err(retry_store)?;
            let courtesy_outcome = project_builtin_wireguard_mesh(
                &courtesy.cluster,
                config.local_machine_id().clone(),
                &bound.public_key,
                courtesy.machines,
                courtesy.peers,
            )
            .map_err(|source| ReconcileError::Fatal(KeeperRoleRuntimeError::Projection(source)))?;
            match courtesy_outcome {
                BuiltinWireguardMeshOutcome::ReallocateLocalContainerSubnet {
                    repair,
                    evidence,
                } => {
                    tracing::warn!(machine_id = %repair.machine_id(), occupied_subnets = repair.occupied_subnets().len(), ?evidence, "courtesy roster re-read found another lost local subnet claim");
                }
                BuiltinWireguardMeshOutcome::NoRoster { evidence } => {
                    tracing::info!(?evidence, "courtesy roster re-read found an empty roster");
                }
                BuiltinWireguardMeshOutcome::Fenced {
                    local_machine_id,
                    reason,
                    evidence,
                } => {
                    tracing::warn!(%local_machine_id, ?reason, ?evidence, "courtesy roster re-read found the local machine fenced");
                }
                BuiltinWireguardMeshOutcome::KeyMismatch {
                    mismatches,
                    evidence,
                } => {
                    tracing::warn!(
                        ?mismatches,
                        ?evidence,
                        "courtesy roster re-read found mismatched WireGuard identities"
                    );
                }
                BuiltinWireguardMeshOutcome::Desired(desired) => {
                    tracing::info!(evidence = ?desired.evidence, "courtesy roster re-read accepted the repaired local subnet claim");
                }
            }
            Ok(ReconcileProgress::Requery)
        }
        BuiltinWireguardMeshOutcome::Desired(desired) => {
            isolation.eligible = true;
            tracing::info!(
                machine_peers = desired.machine_peers.len(),
                roaming_peers = desired.roaming_peers.len(),
                ebpf_routes = desired.ebpf_routes.len(),
                evidence = ?desired.evidence,
                "Keeper is applying the accepted builtin mesh roster"
            );
            let attempted_at = now().map_err(retry_status)?;
            let (isolation_testimony, isolation_error) = match isolation_projection {
                Ok(projection) => {
                    tracing::info!(
                        entries = projection.desired.entries.len(),
                        evidence = ?projection.evidence,
                        "Keeper is applying the accepted container isolation map"
                    );
                    let (testimony, error) = converge_isolation(
                        provider,
                        &projection.desired,
                        projection.evidence.observed_rows,
                        attempted_at,
                        isolation,
                    )
                    .await;
                    (testimony, error.map(provider_failure))
                }
                Err(error) => {
                    let detail = error.to_string();
                    let testimony = ContainerIsolationTestimony::Degraded {
                        attempted_at,
                        last_successful_converge: isolation.last_successful_converge,
                        reason: ContainerIsolationDegradationReason::HostEffect {
                            message: detail.clone(),
                        },
                    };
                    isolation.testimony = Some(testimony.clone());
                    (testimony, Some(ReconcileError::Retry { detail }))
                }
            };
            match provider.converge_peers(&desired).await {
                Ok(host) => {
                    let handshakes = Some(map_machine_handshakes(
                        &desired.machine_peers,
                        &host.latest_handshakes,
                    ));
                    let testimony = match host.ebpf {
                        EbpfHostOutcome::Ready { .. } => {
                            *last_successful_converge = Some(attempted_at);
                            MeshConvergenceTestimony::Converged {
                                bind_address:
                                    ployz_core::corrosion::derive_builtin_wireguard_member(
                                        config.cluster_id(),
                                        &bound.public_key,
                                    )
                                    .bind_address(),
                                attempted_at,
                                last_successful_converge: attempted_at,
                                wireguard: MeshComponentReady {
                                    converged_at: attempted_at,
                                },
                                ebpf: MeshComponentReady {
                                    converged_at: attempted_at,
                                },
                            }
                        }
                        EbpfHostOutcome::Degraded { reason } => {
                            MeshConvergenceTestimony::Degraded {
                                bind_address:
                                    ployz_core::corrosion::derive_builtin_wireguard_member(
                                        config.cluster_id(),
                                        &bound.public_key,
                                    )
                                    .bind_address(),
                                attempted_at,
                                last_successful_converge: *last_successful_converge,
                                degradation: MeshDegradation::Ebpf {
                                    wireguard: MeshComponentReady {
                                        converged_at: attempted_at,
                                    },
                                    ebpf: EbpfMeshDegraded {
                                        reason: map_ebpf_degradation(reason),
                                    },
                                },
                            }
                        }
                    };
                    write_testimony(
                        store,
                        writer,
                        Some(testimony),
                        Some(isolation_testimony),
                        handshakes,
                    )
                    .await?;
                    match isolation_error {
                        Some(error) => Err(error),
                        None => Ok(ReconcileProgress::Settled),
                    }
                }
                Err(error) => {
                    let detail = error.to_string();
                    let testimony = MeshConvergenceTestimony::Degraded {
                        bind_address: ployz_core::corrosion::derive_builtin_wireguard_member(
                            config.cluster_id(),
                            &bound.public_key,
                        )
                        .bind_address(),
                        attempted_at,
                        last_successful_converge: *last_successful_converge,
                        degradation: MeshDegradation::Wireguard {
                            wireguard: MeshComponentDegraded {
                                message: detail.clone(),
                            },
                            ebpf: MeshComponentNotAttempted {
                                reason: MeshNotAttemptedReason::DependencyDegraded,
                            },
                        },
                    };
                    write_testimony(
                        store,
                        writer,
                        Some(testimony),
                        Some(isolation_testimony),
                        None,
                    )
                    .await?;
                    Err(provider_failure(error))
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoldKind {
    Full,
    IsolationOnly,
}

fn preempt_fold(current: FoldKind, incoming: FoldKind) -> FoldKind {
    match (current, incoming) {
        (FoldKind::Full, _) | (_, FoldKind::Full) => FoldKind::Full,
        (FoldKind::IsolationOnly, FoldKind::IsolationOnly) => FoldKind::IsolationOnly,
    }
}

fn subscription_barrier_fold() -> FoldKind {
    FoldKind::Full
}

fn fold_after_retry(failed: FoldKind) -> FoldKind {
    match failed {
        FoldKind::Full | FoldKind::IsolationOnly => FoldKind::Full,
    }
}

#[derive(Debug, Default)]
struct IsolationCache {
    eligible: bool,
    desired: Option<DesiredContainerIsolation>,
    testimony: Option<ContainerIsolationTestimony>,
    last_successful_converge: Option<ployz_core::corrosion::CorrosionTimestamp>,
}

impl IsolationCache {
    const fn allows_isolation_only(&self) -> bool {
        self.eligible
    }

    fn seed(&mut self, status: Option<&ployz_core::corrosion::MachineStatusDocument>) {
        if self.testimony.is_some() {
            return;
        }
        let Some(testimony) = status.and_then(|status| status.container_isolation.clone()) else {
            return;
        };
        self.last_successful_converge = last_isolation_success(&testimony);
        self.testimony = Some(testimony);
    }

    fn is_successfully_applied(&self, desired: &DesiredContainerIsolation) -> bool {
        self.desired.as_ref() == Some(desired)
            && matches!(
                self.testimony,
                Some(ContainerIsolationTestimony::Converged { .. })
            )
    }
}

async fn reconcile_isolation_only(
    provider: &KeeperMeshProvider,
    store: &KeeperCorrosion,
    writer: &LocalMachineStatusWriter,
    isolation: &mut IsolationCache,
) -> Result<ReconcileProgress, ReconcileError> {
    if !isolation.allows_isolation_only() {
        tracing::debug!("container isolation wake retained the fenced wall");
        return Ok(ReconcileProgress::Settled);
    }
    let snapshot = store.read_isolation().await.map_err(retry_store)?;
    isolation.seed(snapshot.local_status.as_ref());
    let projection = project_container_isolation(snapshot.cluster.prefix, snapshot.containers);
    if isolation_capacity_error(projection.evidence.observed_rows).is_none()
        && isolation.is_successfully_applied(&projection.desired)
    {
        tracing::debug!(evidence = ?projection.evidence, "container invalidation was semantically unchanged");
        return Ok(ReconcileProgress::Settled);
    }

    let attempted_at = now().map_err(retry_status)?;
    let (testimony, error) = converge_isolation(
        provider,
        &projection.desired,
        projection.evidence.observed_rows,
        attempted_at,
        isolation,
    )
    .await;
    let (mesh, carried_handshakes) = match snapshot.local_status {
        Some(status) => (status.mesh, status.wireguard_handshakes),
        None => (None, None),
    };
    write_testimony(store, writer, mesh, Some(testimony), carried_handshakes).await?;
    match error {
        Some(error) => Err(provider_failure(error)),
        None => Ok(ReconcileProgress::Settled),
    }
}

async fn converge_isolation(
    provider: &KeeperMeshProvider,
    desired: &DesiredContainerIsolation,
    observed_rows: usize,
    attempted_at: ployz_core::corrosion::CorrosionTimestamp,
    isolation: &mut IsolationCache,
) -> (ContainerIsolationTestimony, Option<KeeperProviderError>) {
    let result = if let Some(error) = isolation_capacity_error(observed_rows) {
        Err(error)
    } else {
        provider.converge_isolation(desired).await
    };
    let testimony = match &result {
        Ok(()) => {
            isolation.last_successful_converge = Some(attempted_at);
            ContainerIsolationTestimony::Converged {
                attempted_at,
                last_successful_converge: attempted_at,
                entries: desired.entries.len(),
            }
        }
        Err(error) => ContainerIsolationTestimony::Degraded {
            attempted_at,
            last_successful_converge: isolation.last_successful_converge,
            reason: map_isolation_degradation(error),
        },
    };
    isolation.desired = Some(desired.clone());
    isolation.testimony = Some(testimony.clone());
    (testimony, result.err())
}

fn isolation_capacity_error(observed_rows: usize) -> Option<KeeperProviderError> {
    let capacity = usize::try_from(ployz_ebpf_common::MAX_ISOLATION_ENTRIES)
        .expect("isolation map capacity fits usize");
    if observed_rows > capacity {
        Some(KeeperProviderError::Isolation(
            ContainerIsolationHostError::DesiredSetTooLarge {
                desired: observed_rows,
                capacity,
            },
        ))
    } else {
        None
    }
}

fn last_isolation_success(
    testimony: &ContainerIsolationTestimony,
) -> Option<ployz_core::corrosion::CorrosionTimestamp> {
    match testimony {
        ContainerIsolationTestimony::Converged {
            last_successful_converge,
            ..
        } => Some(*last_successful_converge),
        ContainerIsolationTestimony::Degraded {
            last_successful_converge,
            ..
        } => *last_successful_converge,
    }
}

fn map_isolation_degradation(error: &KeeperProviderError) -> ContainerIsolationDegradationReason {
    match error {
        KeeperProviderError::Isolation(ContainerIsolationHostError::MissingControlProgram {
            path,
        }) => ContainerIsolationDegradationReason::MissingControlProgram {
            path: path.display().to_string(),
        },
        KeeperProviderError::Isolation(ContainerIsolationHostError::MissingBytecode { path }) => {
            ContainerIsolationDegradationReason::MissingBytecode {
                path: path.display().to_string(),
            }
        }
        KeeperProviderError::Isolation(ContainerIsolationHostError::MissingBpffs { path }) => {
            ContainerIsolationDegradationReason::MissingBpffs {
                path: path.display().to_string(),
            }
        }
        KeeperProviderError::Isolation(ContainerIsolationHostError::MissingCgroupV2 { path }) => {
            ContainerIsolationDegradationReason::MissingCgroupV2 {
                path: path.display().to_string(),
            }
        }
        KeeperProviderError::Isolation(ContainerIsolationHostError::DesiredSetTooLarge {
            desired,
            capacity,
        }) => ContainerIsolationDegradationReason::DesiredSetTooLarge {
            desired: *desired,
            capacity: *capacity,
        },
        KeeperProviderError::Host(_)
        | KeeperProviderError::Isolation(
            ContainerIsolationHostError::RowsFileTooLarge { .. }
            | ContainerIsolationHostError::HostEffect { .. },
        )
        | KeeperProviderError::Poisoned
        | KeeperProviderError::TimedOut { .. }
        | KeeperProviderError::Task { .. } => ContainerIsolationDegradationReason::HostEffect {
            message: error.to_string(),
        },
    }
}

async fn repair_local_subnet(
    store: &KeeperCorrosion,
    cluster_prefix: ployz_core::network::MachineEndpointSupernet,
    repair: BuiltinWireguardLocalSubnetReallocation,
    observed_document: &str,
) -> Result<(), ReconcileError> {
    let (machine_id, mut machine, occupied_subnets) = repair.into_parts();
    let subnet = cluster_prefix
        .allocate_random_free(occupied_subnets)
        .map_err(|source| {
            ReconcileError::Fatal(KeeperRoleRuntimeError::SubnetAllocation(source))
        })?;
    let written_at = now().map_err(retry_status)?;
    let previous = apply_local_subnet_repair(&machine_id, &mut machine, subnet.clone(), written_at)
        .map_err(|detail| ReconcileError::Fatal(KeeperRoleRuntimeError::Invariant(detail)))?;
    store
        .rewrite_local_machine(&machine_id, observed_document, &machine)
        .await
        .map_err(retry_store)?;
    tracing::warn!(%machine_id, previous_subnet = ?previous, replacement_subnet = ?subnet, "Keeper rewrote its local machine row to repair the container subnet collision");
    Ok(())
}

fn apply_local_subnet_repair(
    machine_id: &ployz_core::ids::MachineRowId,
    machine: &mut ployz_core::corrosion::MachineDocument,
    subnet: ployz_core::network::MachineEndpointSubnet,
    written_at: ployz_core::corrosion::CorrosionTimestamp,
) -> Result<ployz_core::network::MachineEndpointSubnet, &'static str> {
    let previous = match &mut machine.transport {
        MachineTransport::Wireguard { subnet_v4, .. } => std::mem::replace(subnet_v4, subnet),
        MachineTransport::Tailscale { .. } => {
            return Err("builtin WireGuard subnet repair carried a Tailscale machine");
        }
    };
    machine.provenance = OperatorWriteProvenance {
        written_by: Principal::Machine {
            machine_id: machine_id.clone(),
        },
        written_at,
    };
    Ok(previous)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconcileProgress {
    Settled,
    Requery,
}

fn last_success_from_status(
    status: &ployz_core::corrosion::MachineStatusDocument,
) -> Option<ployz_core::corrosion::CorrosionTimestamp> {
    match status.mesh.as_ref()? {
        MeshConvergenceTestimony::Converged {
            last_successful_converge,
            ..
        } => Some(*last_successful_converge),
        MeshConvergenceTestimony::Degraded {
            last_successful_converge,
            ..
        } => *last_successful_converge,
        MeshConvergenceTestimony::NoRoster { .. }
        | MeshConvergenceTestimony::KeyMismatch { .. } => None,
    }
}

fn map_ebpf_degradation(reason: EbpfDegradedReason) -> EbpfMeshDegradationReason {
    match reason {
        EbpfDegradedReason::MissingBridge { ifname } => {
            EbpfMeshDegradationReason::MissingBridge { ifname }
        }
        EbpfDegradedReason::HostEffect { message } => {
            EbpfMeshDegradationReason::HostEffect { message }
        }
    }
}

fn map_machine_handshakes(
    machine_peers: &[DesiredBuiltinWireguardMachinePeer],
    observed: &BTreeMap<WireGuardPublicKey, BuiltinWireguardLatestHandshake>,
) -> BTreeMap<MachineRowId, WireGuardHandshakeEvidence> {
    machine_peers
        .iter()
        .filter_map(|peer| {
            let evidence = match observed.get(&peer.public_key)? {
                BuiltinWireguardLatestHandshake::Never => WireGuardHandshakeEvidence::Never,
                BuiltinWireguardLatestHandshake::AtUnixSeconds { unix_seconds } => {
                    WireGuardHandshakeEvidence::At {
                        unix_seconds: *unix_seconds,
                    }
                }
            };
            Some((peer.machine_id.clone(), evidence))
        })
        .collect()
}

async fn write_testimony(
    store: &KeeperCorrosion,
    writer: &LocalMachineStatusWriter,
    mesh: Option<MeshConvergenceTestimony>,
    container_isolation: Option<ContainerIsolationTestimony>,
    wireguard_handshakes: Option<BTreeMap<MachineRowId, WireGuardHandshakeEvidence>>,
) -> Result<(), ReconcileError> {
    let statement = writer
        .statement(mesh, container_isolation, wireguard_handshakes)
        .map_err(retry_status)?;
    store.execute(statement).await.map_err(retry_store)
}

fn provider_failure(error: KeeperProviderError) -> ReconcileError {
    match error {
        KeeperProviderError::TimedOut { .. }
        | KeeperProviderError::Task { .. }
        | KeeperProviderError::Poisoned => {
            ReconcileError::Fatal(KeeperRoleRuntimeError::provider(error))
        }
        KeeperProviderError::Host(_) | KeeperProviderError::Isolation(_) => ReconcileError::Retry {
            detail: error.to_string(),
        },
    }
}

fn retry_store(error: KeeperStoreError) -> ReconcileError {
    ReconcileError::Retry {
        detail: error.to_string(),
    }
}

fn retry_status(error: MachineStatusWriteError) -> ReconcileError {
    ReconcileError::Retry {
        detail: error.to_string(),
    }
}

enum ReconcileError {
    Retry { detail: String },
    Fatal(KeeperRoleRuntimeError),
}

#[derive(Debug, Clone, Copy)]
struct RetryDelay {
    initial: Duration,
    maximum: Duration,
    next: Duration,
}

impl RetryDelay {
    const fn new(initial: Duration, maximum: Duration) -> Self {
        Self {
            initial,
            maximum,
            next: initial,
        }
    }

    fn reset(&mut self) {
        self.next = self.initial;
    }

    fn next(&mut self) -> Duration {
        let current = self.next;
        self.next = self.next.saturating_mul(2).min(self.maximum);
        current
    }
}

async fn wait_for_process_shutdown() {
    #[cfg(unix)]
    {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        let interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
        match (terminate, interrupt) {
            (Ok(mut terminate), Ok(mut interrupt)) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = interrupt.recv() => {}
                }
            }
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!(error = %error, "could not install Keeper shutdown signal handler");
                if let Err(error) = tokio::signal::ctrl_c().await {
                    tracing::warn!(error = %error, "could not wait for Keeper shutdown signal");
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %error, "could not wait for Keeper shutdown signal");
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KeeperRoleRuntimeError {
    #[error(transparent)]
    Configuration(KeeperRoleConfigError),
    #[error("could not build the local Corrosion client: {0}")]
    CorrosionConfiguration(crate::corrosion::CorrosionClientConfigError),
    #[error("Keeper mesh provider failed: {detail}")]
    Provider { detail: String },
    #[error("Keeper could not project the accepted builtin mesh: {0}")]
    Projection(ployz_core::corrosion::BuiltinWireguardMeshError),
    #[error("Keeper could not allocate a replacement local container subnet: {0}")]
    SubnetAllocation(ployz_core::network::MachineEndpointSubnetAllocationError),
    #[error("Keeper runtime invariant failed: {0}")]
    Invariant(&'static str),
    #[error("could not inspect local upgrade artifact state: {0}")]
    UpgradeArtifactStore(ployz_host_runner::ArtifactStoreError),
    #[error("upgraded Keeper did not reach its first converge point: {detail}")]
    UpgradeFirstConverge { detail: String },
    #[error("Keeper upgrade socket failed: {detail}")]
    UpgradeSocket { detail: String },
    #[error("Keeper upgrade socket stopped before Keeper shutdown")]
    UpgradeSocketStopped,
}

impl KeeperRoleRuntimeError {
    fn provider(error: KeeperProviderError) -> Self {
        Self::Provider {
            detail: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::corrosion::{
        DesiredBuiltinWireguardLocal, DesiredBuiltinWireguardMachinePeer,
        DesiredMachineContainerRoute, MachineDocument, MachineStatusDocument,
    };
    use ployz_core::ids::{ClusterId, MachineRowId};
    use ployz_core::network::{MachineEndpointSubnet, WireGuardPublicKey};
    use serde_json::json;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    #[tokio::test(start_paused = true)]
    async fn pending_upgrade_first_converge_timeout_requests_systemd_revert() {
        let deadline = Instant::now() + UPGRADE_FIRST_CONVERGE_TIMEOUT;
        tokio::time::advance(UPGRADE_FIRST_CONVERGE_TIMEOUT).await;

        assert!(matches!(
            first_converge_timeout(true, Some(deadline), "mesh convergence failed"),
            Some(KeeperRoleRuntimeError::UpgradeFirstConverge { detail })
                if detail == "mesh convergence failed"
        ));
    }

    #[test]
    fn pending_upgrade_fatal_first_converge_requests_systemd_revert() {
        assert!(matches!(
            first_converge_failure(true, KeeperRoleRuntimeError::Invariant("projection failed")),
            KeeperRoleRuntimeError::UpgradeFirstConverge { detail }
                if detail.contains("projection failed")
        ));
    }

    fn timestamp(value: &str) -> ployz_core::corrosion::CorrosionTimestamp {
        ployz_core::corrosion::CorrosionTimestamp::try_new(value).expect("timestamp")
    }

    fn local() -> DesiredBuiltinWireguardLocal {
        let cluster_id = ClusterId::try_new(CLUSTER).expect("cluster");
        let public_key =
            WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("key");
        let identity =
            ployz_core::corrosion::derive_builtin_wireguard_member(&cluster_id, &public_key);
        DesiredBuiltinWireguardLocal {
            public_key,
            subnet_v6: identity.subnet(),
            bind_address: identity.bind_address(),
        }
    }

    #[test]
    fn handshake_testimony_names_only_current_machine_peers() {
        let machine_id = MachineRowId::try_new(MACHINE).expect("machine");
        let never_machine_id =
            MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("never machine");
        let machine_key =
            WireGuardPublicKey::try_new("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=")
                .expect("machine key");
        let never_machine_key =
            WireGuardPublicKey::try_new("AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=")
                .expect("never machine key");
        let roaming_key =
            WireGuardPublicKey::try_new("AwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwM=")
                .expect("roaming key");
        let cluster_id = ClusterId::try_new(CLUSTER).expect("cluster");
        let identity =
            ployz_core::corrosion::derive_builtin_wireguard_member(&cluster_id, &machine_key);
        let never_identity =
            ployz_core::corrosion::derive_builtin_wireguard_member(&cluster_id, &never_machine_key);
        let machine_peers = vec![
            DesiredBuiltinWireguardMachinePeer {
                machine_id: machine_id.clone(),
                public_key: machine_key.clone(),
                subnet_v6: identity.subnet(),
                endpoint: None,
                container_route: DesiredMachineContainerRoute::Claimed {
                    subnet: MachineEndpointSubnet::try_new("10.210.2.0/24").expect("subnet"),
                },
            },
            DesiredBuiltinWireguardMachinePeer {
                machine_id: never_machine_id.clone(),
                public_key: never_machine_key.clone(),
                subnet_v6: never_identity.subnet(),
                endpoint: None,
                container_route: DesiredMachineContainerRoute::Claimed {
                    subnet: MachineEndpointSubnet::try_new("10.210.3.0/24").expect("subnet"),
                },
            },
        ];
        let observed = std::collections::BTreeMap::from([
            (
                machine_key,
                ployz_host_runner::builtin_wireguard::BuiltinWireguardLatestHandshake::AtUnixSeconds {
                    unix_seconds: 1_754_390_400,
                },
            ),
            (
                never_machine_key,
                ployz_host_runner::builtin_wireguard::BuiltinWireguardLatestHandshake::Never,
            ),
            (
                roaming_key,
                ployz_host_runner::builtin_wireguard::BuiltinWireguardLatestHandshake::AtUnixSeconds {
                    unix_seconds: 1_754_390_399,
                },
            ),
        ]);

        assert_eq!(
            map_machine_handshakes(&machine_peers, &observed),
            std::collections::BTreeMap::from([
                (
                    machine_id,
                    ployz_core::corrosion::WireGuardHandshakeEvidence::At {
                        unix_seconds: 1_754_390_400,
                    },
                ),
                (
                    never_machine_id,
                    ployz_core::corrosion::WireGuardHandshakeEvidence::Never,
                ),
            ])
        );
    }

    #[test]
    fn successful_single_machine_roster_publishes_an_empty_handshake_map() {
        let roaming_key =
            WireGuardPublicKey::try_new("AwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwM=")
                .expect("roaming key");
        let observed = std::collections::BTreeMap::from([(
            roaming_key,
            ployz_host_runner::builtin_wireguard::BuiltinWireguardLatestHandshake::AtUnixSeconds {
                unix_seconds: 1_754_390_399,
            },
        )]);

        let handshakes = Some(map_machine_handshakes(&[], &observed));
        assert_eq!(handshakes, Some(std::collections::BTreeMap::new()));
    }

    #[test]
    fn retry_delay_is_bounded_and_resets() {
        let mut retry = RetryDelay::new(Duration::from_millis(2), Duration::from_millis(5));
        assert_eq!(retry.next(), Duration::from_millis(2));
        assert_eq!(retry.next(), Duration::from_millis(4));
        assert_eq!(retry.next(), Duration::from_millis(5));
        assert_eq!(retry.next(), Duration::from_millis(5));
        retry.reset();
        assert_eq!(retry.next(), Duration::from_millis(2));
    }

    #[test]
    fn roster_wake_preempts_a_coalesced_container_wake() {
        assert_eq!(
            preempt_fold(FoldKind::IsolationOnly, FoldKind::Full),
            FoldKind::Full
        );
        assert_eq!(
            preempt_fold(FoldKind::IsolationOnly, FoldKind::IsolationOnly),
            FoldKind::IsolationOnly
        );
    }

    #[test]
    fn successful_subscription_forces_a_full_requery_barrier() {
        assert_eq!(subscription_barrier_fold(), FoldKind::Full);
    }

    #[test]
    fn isolation_failure_promotes_retry_to_full_reconciliation() {
        assert_eq!(fold_after_retry(FoldKind::IsolationOnly), FoldKind::Full);
        assert_eq!(fold_after_retry(FoldKind::Full), FoldKind::Full);
    }

    #[test]
    fn fenced_cache_suppresses_isolation_only_dispatch() {
        let cache = IsolationCache::default();
        assert!(!cache.allows_isolation_only());
    }

    #[test]
    fn successful_semantically_equal_isolation_state_is_suppressed() {
        let desired = DesiredContainerIsolation {
            prefix: ployz_core::network::MachineEndpointSupernet::try_new("10.77.0.0/16")
                .expect("prefix"),
            entries: Vec::new(),
        };
        let attempted_at = timestamp("2026-08-05T10:01:00Z");
        let cache = IsolationCache {
            eligible: true,
            desired: Some(desired.clone()),
            testimony: Some(ContainerIsolationTestimony::Converged {
                attempted_at,
                last_successful_converge: attempted_at,
                entries: 0,
            }),
            last_successful_converge: Some(attempted_at),
        };

        assert!(cache.is_successfully_applied(&desired));
    }

    #[test]
    fn degraded_equal_isolation_state_is_dispatched_for_retry() {
        let desired = DesiredContainerIsolation {
            prefix: ployz_core::network::MachineEndpointSupernet::try_new("10.77.0.0/16")
                .expect("prefix"),
            entries: Vec::new(),
        };
        let attempted_at = timestamp("2026-08-05T10:01:00Z");
        let cache = IsolationCache {
            eligible: true,
            desired: Some(desired.clone()),
            testimony: Some(ContainerIsolationTestimony::Degraded {
                attempted_at,
                last_successful_converge: None,
                reason: ContainerIsolationDegradationReason::MissingCgroupV2 {
                    path: "/sys/fs/cgroup".to_owned(),
                },
            }),
            last_successful_converge: None,
        };

        assert!(!cache.is_successfully_applied(&desired));
    }

    #[test]
    fn raw_container_count_over_capacity_becomes_typed_isolation_degradation() {
        let capacity = usize::try_from(ployz_ebpf_common::MAX_ISOLATION_ENTRIES).expect("capacity");
        let error = isolation_capacity_error(capacity + 1).expect("over capacity");
        assert_eq!(
            map_isolation_degradation(&error),
            ContainerIsolationDegradationReason::DesiredSetTooLarge {
                desired: capacity + 1,
                capacity,
            }
        );
    }

    #[test]
    fn stale_subnet_repair_evidence_is_visible_and_retryable() {
        let machine_id = MachineRowId::try_new(MACHINE).expect("machine");

        let error = retry_store(KeeperStoreError::StaleLocalMachineRepair {
            machine_id: machine_id.clone(),
        });

        assert!(matches!(
            error,
            ReconcileError::Retry { detail }
                if detail.contains(machine_id.as_str()) && detail.contains("became stale")
        ));
    }

    #[test]
    fn local_subnet_repair_changes_only_subnet_and_required_provenance() {
        let machine_id = MachineRowId::try_new(MACHINE).expect("machine");
        let mut machine = serde_json::from_value::<MachineDocument>(json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "written_by": { "kind": "peer", "peer_id": "01ARZ3NDEKTSV4RRFFQ69G5FAY" },
            "written_at": "2026-08-05T10:00:00Z",
            "name": "machine-one",
            "lifecycle": "active",
            "transport": {
                "kind": "wireguard",
                "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                "addr_v6": "fd00::1",
                "endpoint": "192.0.2.1:51820",
                "subnet_v4": "10.210.1.0/24"
            },
            "storage": { "mode": "plain", "reason": { "kind": "default" } }
        }))
        .expect("machine fixture");
        let before = machine.clone();
        let replacement = MachineEndpointSubnet::try_new("10.210.2.0/24").expect("subnet");
        let written_at = timestamp("2026-08-05T10:01:00Z");

        let previous =
            apply_local_subnet_repair(&machine_id, &mut machine, replacement.clone(), written_at)
                .expect("builtin repair");

        assert_eq!(
            previous,
            MachineEndpointSubnet::try_new("10.210.1.0/24").expect("subnet")
        );
        assert_eq!(machine.v, before.v);
        assert_eq!(machine.cluster_id, before.cluster_id);
        assert_eq!(machine.name, before.name);
        assert_eq!(machine.lifecycle, before.lifecycle);
        assert_eq!(machine.storage, before.storage);
        let MachineTransport::Wireguard {
            pubkey: before_key,
            addr_v6: before_addr,
            endpoint: before_endpoint,
            ..
        } = before.transport
        else {
            panic!("fixture is builtin WireGuard")
        };
        let MachineTransport::Wireguard {
            pubkey,
            addr_v6,
            endpoint,
            subnet_v4,
        } = machine.transport
        else {
            panic!("repair preserves builtin WireGuard")
        };
        assert_eq!(pubkey, before_key);
        assert_eq!(addr_v6, before_addr);
        assert_eq!(endpoint, before_endpoint);
        assert_eq!(subnet_v4, replacement);
        assert_eq!(
            machine.provenance,
            OperatorWriteProvenance {
                written_by: Principal::Machine { machine_id },
                written_at,
            }
        );
    }

    #[test]
    fn e_bpf_degradation_preserves_missing_bridge_identity() {
        assert_eq!(
            map_ebpf_degradation(EbpfDegradedReason::MissingBridge {
                ifname: "br-ployz".to_owned(),
            }),
            EbpfMeshDegradationReason::MissingBridge {
                ifname: "br-ployz".to_owned(),
            }
        );
    }

    #[test]
    fn restart_seeds_last_success_from_durable_local_testimony() {
        let last_successful_converge = timestamp("2026-08-04T11:00:00Z");
        let attempted_at = timestamp("2026-08-04T12:00:00Z");
        let local = local();
        let status = MachineStatusDocument {
            v: ployz_core::corrosion::CorrosionDocumentVersion::V1,
            cluster_id: ClusterId::try_new(CLUSTER).expect("cluster"),
            machine_id: MachineRowId::try_new(MACHINE).expect("machine"),
            ployz_version: "0.1.0".to_owned(),
            corrosion_version: "0.2.0-beta.0".to_owned(),
            architecture: "x86_64".to_owned(),
            free_disk_bytes: 1,
            free_memory_bytes: 2,
            load: ployz_core::corrosion::MachineLoadBand::Idle,
            observed_at: attempted_at,
            mesh: Some(MeshConvergenceTestimony::Degraded {
                bind_address: local.bind_address,
                attempted_at,
                last_successful_converge: Some(last_successful_converge),
                degradation: MeshDegradation::Ebpf {
                    wireguard: MeshComponentReady {
                        converged_at: attempted_at,
                    },
                    ebpf: EbpfMeshDegraded {
                        reason: EbpfMeshDegradationReason::MissingBridge {
                            ifname: "br-ployz".to_owned(),
                        },
                    },
                },
            }),
            container_isolation: None,
            wireguard_handshakes: None,
        };

        assert_eq!(
            last_success_from_status(&status),
            Some(last_successful_converge)
        );
    }
}
