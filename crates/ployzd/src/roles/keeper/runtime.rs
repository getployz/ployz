//! Keeper lifecycle and bounded roster reconciliation.

use std::future::Future;
use std::time::Duration;

use ployz_core::corrosion::{
    BuiltinWireguardMeshOutcome, EbpfMeshDegradationReason, EbpfMeshDegraded,
    MeshComponentDegraded, MeshComponentNotAttempted, MeshComponentReady, MeshConvergenceTestimony,
    MeshDegradation, MeshNotAttemptedReason, project_builtin_wireguard_mesh,
};
use ployz_host_runner::builtin_wireguard::{EbpfDegradedReason, EbpfHostOutcome};

use crate::corrosion::CorrosionClient;

use super::provider::{
    BoundKeeperIdentity, KeeperHostFold, KeeperMeshProvider, KeeperProviderError,
};
use super::status::{LocalMachineStatusWriter, MachineStatusWriteError, now};
use super::store::{KeeperCorrosion, KeeperStoreError};
use super::{KeeperRoleConfig, KeeperRoleConfigError};

/// Runs Keeper from the supervisor-owned environment until process shutdown.
pub async fn run_from_environment() -> Result<(), KeeperRoleRuntimeError> {
    let config =
        KeeperRoleConfig::from_environment().map_err(KeeperRoleRuntimeError::Configuration)?;
    let provider =
        KeeperMeshProvider::from_config(&config).map_err(KeeperRoleRuntimeError::provider)?;
    let corrosion = CorrosionClient::new(config.corrosion().clone())
        .map_err(KeeperRoleRuntimeError::CorrosionConfiguration)?;
    let store = KeeperCorrosion::new(
        corrosion,
        config.cluster_id().clone(),
        config.local_machine_id().clone(),
    );
    run_keeper(config, provider, store, wait_for_process_shutdown()).await
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
    // This happens before the first Corrosion request so the local sidecar can
    // bind its gossip socket to deterministic mesh identity.
    let bound = provider
        .bind_ip(config.cluster_id(), config.local_machine_id())
        .await
        .map_err(KeeperRoleRuntimeError::provider)?;
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
    let mut periodic = tokio::time::interval(config.reconcile_interval());
    periodic.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume the immediate first tick; the explicit initial reconcile below
    // owns startup testimony.
    periodic.tick().await;
    let mut retry = RetryDelay::new(config.retry_initial(), config.retry_max());
    let mut subscription_retry = RetryDelay::new(config.retry_initial(), config.retry_max());
    tokio::pin!(shutdown);
    loop {
        match reconcile_once(
            &config,
            &provider,
            &store,
            &writer,
            &bound,
            &mut last_successful_converge,
        )
        .await
        {
            Ok(()) => retry.reset(),
            Err(ReconcileError::Retry { detail }) => {
                let delay = retry.next();
                tracing::warn!(error = %detail, ?delay, "Keeper convergence will retry");
                tokio::select! {
                    () = &mut shutdown => return Ok(()),
                    () = tokio::time::sleep(delay) => {}
                }
                continue;
            }
            Err(ReconcileError::Fatal(error)) => return Err(error),
        }

        tokio::select! {
            () = &mut shutdown => return Ok(()),
            _ = periodic.tick() => {
                tracing::debug!("periodic Keeper roster re-query");
            }
            invalidation = store.wait_for_invalidation() => {
                match invalidation {
                    Ok(()) => {
                        subscription_retry.reset();
                        tracing::debug!("Keeper roster subscription invalidated");
                    }
                    Err(error) => {
                        let delay = subscription_retry.next();
                        tracing::warn!(error = %error, ?delay, "Keeper roster subscription disconnected; backing off before a full re-query");
                        tokio::select! {
                            () = &mut shutdown => return Ok(()),
                            () = tokio::time::sleep(delay) => {}
                        }
                    }
                }
            }
        }
    }
}

async fn reconcile_once(
    config: &KeeperRoleConfig,
    provider: &KeeperMeshProvider,
    store: &KeeperCorrosion,
    writer: &LocalMachineStatusWriter,
    bound: &BoundKeeperIdentity,
    last_successful_converge: &mut Option<ployz_core::corrosion::CorrosionTimestamp>,
) -> Result<(), ReconcileError> {
    let snapshot = store.read_roster().await.map_err(retry_store)?;
    if last_successful_converge.is_none() {
        *last_successful_converge = snapshot
            .local_status
            .as_ref()
            .and_then(last_success_from_status);
    }
    let outcome = project_builtin_wireguard_mesh(
        &snapshot.cluster,
        config.local_machine_id().clone(),
        &bound.public_key,
        snapshot.machines,
        snapshot.peers,
    )
    .map_err(|source| ReconcileError::Fatal(KeeperRoleRuntimeError::Projection(source)))?;
    let attempted_at = now().map_err(retry_status)?;

    match keeper_decision(&outcome) {
        KeeperDecision::NoRoster => {
            write_testimony(
                store,
                writer,
                MeshConvergenceTestimony::NoRoster { attempted_at },
            )
            .await?;
            Ok(())
        }
        KeeperDecision::Fence { reason } => {
            tracing::warn!(?reason, "local machine is fenced from the accepted roster");
            provider
                .converge_peers(&outcome, bound)
                .await
                .map_err(provider_failure)?;
            // The machine row sweep owns stale status removal. A fenced Keeper
            // must not recreate testimony for an identity no longer accepted.
            Ok(())
        }
        KeeperDecision::KeyMismatch { mismatches } => {
            write_testimony(
                store,
                writer,
                MeshConvergenceTestimony::KeyMismatch {
                    attempted_at,
                    mismatches: mismatches.to_vec(),
                },
            )
            .await?;
            Ok(())
        }
        KeeperDecision::Converge => match provider.converge_peers(&outcome, bound).await {
            Ok(KeeperHostFold::Applied(host)) => {
                let testimony = match host.ebpf {
                    EbpfHostOutcome::Ready { .. } => {
                        *last_successful_converge = Some(attempted_at);
                        MeshConvergenceTestimony::Converged {
                            bind_address: ployz_core::corrosion::derive_builtin_wireguard_member(
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
                    EbpfHostOutcome::Degraded { reason } => MeshConvergenceTestimony::Degraded {
                        bind_address: ployz_core::corrosion::derive_builtin_wireguard_member(
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
                    },
                };
                write_testimony(store, writer, testimony).await
            }
            Ok(KeeperHostFold::Skipped) => {
                Err(ReconcileError::Fatal(KeeperRoleRuntimeError::Invariant(
                    "Core desired mesh unexpectedly skipped its host fold",
                )))
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
                write_testimony(store, writer, testimony).await?;
                Err(provider_failure(error))
            }
        },
    }
}

#[derive(Debug, PartialEq, Eq)]
enum KeeperDecision<'a> {
    NoRoster,
    Fence {
        reason: &'a ployz_core::corrosion::BuiltinWireguardFenceReason,
    },
    KeyMismatch {
        mismatches: &'a [ployz_core::corrosion::BuiltinWireguardKeyMismatch],
    },
    Converge,
}

fn keeper_decision(outcome: &BuiltinWireguardMeshOutcome) -> KeeperDecision<'_> {
    match outcome {
        BuiltinWireguardMeshOutcome::NoRoster { .. } => KeeperDecision::NoRoster,
        BuiltinWireguardMeshOutcome::Fenced { reason, .. } => KeeperDecision::Fence { reason },
        BuiltinWireguardMeshOutcome::KeyMismatch { mismatches, .. } => {
            KeeperDecision::KeyMismatch { mismatches }
        }
        BuiltinWireguardMeshOutcome::Desired(_) => KeeperDecision::Converge,
    }
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
        | MeshConvergenceTestimony::Fenced { .. }
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

async fn write_testimony(
    store: &KeeperCorrosion,
    writer: &LocalMachineStatusWriter,
    testimony: MeshConvergenceTestimony,
) -> Result<(), ReconcileError> {
    let statement = writer.statement(testimony).map_err(retry_status)?;
    store.execute(statement).await.map_err(retry_store)
}

fn provider_failure(error: KeeperProviderError) -> ReconcileError {
    match error {
        KeeperProviderError::TimedOut { .. }
        | KeeperProviderError::Task { .. }
        | KeeperProviderError::Poisoned => {
            ReconcileError::Fatal(KeeperRoleRuntimeError::provider(error))
        }
        KeeperProviderError::Configuration(_) | KeeperProviderError::Host(_) => {
            ReconcileError::Retry {
                detail: error.to_string(),
            }
        }
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
    #[error("Keeper runtime invariant failed: {0}")]
    Invariant(&'static str),
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
        BuiltinWireguardFenceReason, BuiltinWireguardRosterEvidence, DesiredBuiltinWireguardLocal,
        DesiredBuiltinWireguardMesh, MachineStatusDocument,
    };
    use ployz_core::ids::{ClusterId, MachineRowId};
    use ployz_core::network::WireGuardPublicKey;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    fn timestamp(value: &str) -> ployz_core::corrosion::CorrosionTimestamp {
        ployz_core::corrosion::CorrosionTimestamp::try_new(value).expect("timestamp")
    }

    fn local() -> DesiredBuiltinWireguardLocal {
        let cluster_id = ClusterId::try_new(CLUSTER).expect("cluster");
        let machine_id = MachineRowId::try_new(MACHINE).expect("machine");
        let public_key =
            WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("key");
        let identity =
            ployz_core::corrosion::derive_builtin_wireguard_member(&cluster_id, &public_key);
        DesiredBuiltinWireguardLocal {
            machine_id,
            public_key,
            subnet_v6: identity.subnet(),
            bind_address: identity.bind_address(),
        }
    }

    fn evidence() -> BuiltinWireguardRosterEvidence {
        BuiltinWireguardRosterEvidence {
            machine_skipped: Vec::new(),
            machine_shadows: Vec::new(),
            peer_skipped: Vec::new(),
            peer_shadows: Vec::new(),
            address_mismatches: Vec::new(),
            identity_conflicts: Vec::new(),
        }
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
    fn pure_decision_keeps_host_and_status_actions_distinct() {
        let no_roster = BuiltinWireguardMeshOutcome::NoRoster {
            evidence: evidence(),
        };
        assert_eq!(keeper_decision(&no_roster), KeeperDecision::NoRoster);

        let fenced = BuiltinWireguardMeshOutcome::Fenced {
            local_machine_id: MachineRowId::try_new(MACHINE).expect("machine"),
            reason: BuiltinWireguardFenceReason::MissingLocalMachine,
            evidence: evidence(),
        };
        assert!(matches!(
            keeper_decision(&fenced),
            KeeperDecision::Fence {
                reason: BuiltinWireguardFenceReason::MissingLocalMachine
            }
        ));

        let key_mismatch = BuiltinWireguardMeshOutcome::KeyMismatch {
            mismatches: Vec::new(),
            evidence: evidence(),
        };
        assert!(matches!(
            keeper_decision(&key_mismatch),
            KeeperDecision::KeyMismatch { mismatches } if mismatches.is_empty()
        ));

        let desired = BuiltinWireguardMeshOutcome::Desired(DesiredBuiltinWireguardMesh {
            local: local(),
            machine_peers: Vec::new(),
            roaming_peers: Vec::new(),
            ebpf_routes: Vec::new(),
            evidence: evidence(),
        });
        assert_eq!(keeper_decision(&desired), KeeperDecision::Converge);
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
        };

        assert_eq!(
            last_success_from_status(&status),
            Some(last_successful_converge)
        );
    }
}
