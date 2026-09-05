//! Assemble and stream complete Runtime Watch frames from replicated observations
//! plus entry-local membership and RTT samples.

use std::{
    future::Future,
    time::{Duration, SystemTime},
};

use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::{Stream, StreamExt};
use ployz_core::{
    CertificateAvailability, CertificateBackoff, CertificateFailureKind, CertificateObservation,
    ContainerId, ContainerObservation, DockerVolume, DockerVolumeId, IngressHost, IssuanceClock,
    IssuanceFailure, Machine, MachineId, MachineObservation, MembershipObservation,
    RuntimeWatchFrame, RuntimeWatchIncompleteIds, RuntimeWatchPayloadError,
    encode_runtime_watch_frame,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

use crate::{
    corrosion::{CertificateRow, Error, ReplicatedObservations, ReplicatedStore},
    global_reconcile::GlobalReconcileObservations,
    hosted_dns::Reservation,
    logs::RpcStream,
    machine::{LocalMachine, RuntimeWatchTelemetry},
};

/// How often Watch samples membership and RTT from the local admin socket.
const TELEMETRY_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Replicated observations used to assemble one Runtime Watch frame.
///
/// Loaded from the store. Never from live Machine RPC or Docker fan-out.
#[derive(Clone)]
pub(crate) struct RuntimeWatchSnapshot {
    pub machines: ReplicatedObservations<Machine, MachineId>,
    pub containers: ReplicatedObservations<ContainerObservation, ContainerId>,
    pub volumes: ReplicatedObservations<DockerVolume, DockerVolumeId>,
    pub certificates: ReplicatedObservations<(IngressHost, CertificateRow), IngressHost>,
    pub hosted_dns: Option<Reservation>,
}

/// Latest entry-local membership/RTT sample used to assemble a frame.
#[derive(Clone)]
struct LatestSample {
    telemetry: Option<RuntimeWatchTelemetry>,
    observed_at: String,
}

impl RuntimeWatchSnapshot {
    /// Read Machines, Containers, Docker Volumes, certificates, and hosted DNS from the store.
    ///
    /// # Errors
    ///
    /// Returns when a replicated row cannot be read or decoded.
    pub(crate) async fn from_store(store: &ReplicatedStore) -> Result<Self, Error> {
        Ok(Self {
            machines: store.machines().await?,
            containers: store.containers().await?,
            volumes: store.volumes().await?,
            certificates: store.certificate_rows().await?,
            hosted_dns: store.domain_reservation().await?,
        })
    }
}

/// Serve complete Runtime Watch frames from the replicated store.
///
/// Subscribes first, then yields one complete frame immediately. Later frames
/// are assembled on store wakeups, Global reconcile observation changes, or
/// once-per-second membership/RTT samples from the same local admin source as
/// ListMachines and inspect RTT. The
/// notification payload is not the observation. Unchanged observations do not
/// yield, including when only `observed_at` advances. Store failure ends the
/// stream. Admin telemetry failure keeps replicated rows.
///
/// # Errors
///
/// Returns if store subscriptions cannot be opened.
pub(crate) async fn serve_replicated_runtime_watch(
    store: ReplicatedStore,
    local: LocalMachine,
    entry_id: MachineId,
    global_reconcile: GlobalReconcileObservations,
) -> Result<RpcStream, Error> {
    let changes = store.subscribe_runtime_watch_changes().await?;
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + TELEMETRY_SAMPLE_INTERVAL,
        TELEMETRY_SAMPLE_INTERVAL,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    Ok(serve_runtime_watch(
        entry_id,
        move || {
            let store = store.clone();
            async move { RuntimeWatchSnapshot::from_store(&store).await }
        },
        move || {
            let local = local.clone();
            async move {
                LatestSample {
                    telemetry: local.runtime_watch_telemetry().await,
                    observed_at: rfc3339(SystemTime::now()),
                }
            }
        },
        changes,
        futures_util::stream::unfold(interval, |mut interval| async move {
            interval.tick().await;
            Some(((), interval))
        }),
        global_reconcile,
    ))
}

/// Serve complete Runtime Watch frames from replicated store reads.
///
/// Yields one complete frame immediately, then another when a store
/// notification, Global reconcile observation, or telemetry sample wakes
/// assembly and the assembled
/// observation changed. Ticks resample membership/RTT only. Store wakes
/// reload the snapshot only. `observed_at` is the time of the latest
/// membership/RTT sample and is ignored when deciding whether the
/// observation changed.
fn serve_runtime_watch<L, Fut, S, SFut, C, T>(
    entry_id: MachineId,
    load: L,
    sample: S,
    changes: C,
    ticks: T,
    mut global_reconcile: GlobalReconcileObservations,
) -> RpcStream
where
    L: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = Result<RuntimeWatchSnapshot, Error>> + Send,
    S: Fn() -> SFut + Send + 'static,
    SFut: Future<Output = LatestSample> + Send,
    C: Stream<Item = Result<(), Error>> + Send + 'static,
    T: Stream<Item = ()> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel(8);
    tokio::spawn(async move {
        let mut changes = std::pin::pin!(changes);
        let mut ticks = std::pin::pin!(ticks);
        let mut global_reconcile_open = true;
        let mut last = None;
        let (loaded, mut latest) = tokio::join!(load(), sample());
        let mut snapshot = match loaded {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = sender
                    .send(Err(Status::unavailable(error.to_string())))
                    .await;
                return;
            }
        };
        loop {
            // Keep the snapshot so a tick resamples without another store read.
            let frame = assemble_runtime_watch_frame(
                snapshot.clone(),
                &entry_id,
                latest.telemetry.as_ref(),
                global_reconcile.borrow().clone(),
                latest.observed_at.clone(),
            );
            if last
                .as_ref()
                .is_none_or(|previous| observation_changed(previous, &frame))
            {
                let payload = match encode_runtime_watch_frame(&frame) {
                    Ok(payload) => payload,
                    Err(error @ RuntimeWatchPayloadError::MessageTooLarge { .. }) => {
                        let _ = sender
                            .send(Err(Status::out_of_range(error.to_string())))
                            .await;
                        return;
                    }
                    Err(error) => {
                        let _ = sender.send(Err(Status::internal(error.to_string()))).await;
                        return;
                    }
                };
                if sender.send(Ok(payload)).await.is_err() {
                    return;
                }
                last = Some(frame);
            }
            tokio::select! {
                biased;
                () = sender.closed() => return,
                changed = global_reconcile.changed(), if global_reconcile_open => {
                    if changed.is_err() {
                        global_reconcile_open = false;
                    }
                }
                Some(()) = ticks.next() => {
                    latest = sample().await;
                }
                changed = changes.next() => match changed {
                    Some(Ok(())) => match load().await {
                        Ok(next) => snapshot = next,
                        Err(error) => {
                            let _ = sender
                                .send(Err(Status::unavailable(error.to_string())))
                                .await;
                            return;
                        }
                    },
                    Some(Err(error)) => {
                        let _ = sender
                            .send(Err(Status::unavailable(error.to_string())))
                            .await;
                        return;
                    }
                    None => {
                        let _ = sender
                            .send(Err(Status::unavailable(
                                "Runtime Watch store subscription ended",
                            )))
                            .await;
                        return;
                    }
                },
            }
        }
    });
    ReceiverStream::new(receiver)
}

fn observation_changed(previous: &RuntimeWatchFrame, next: &RuntimeWatchFrame) -> bool {
    let RuntimeWatchFrame {
        machines,
        containers,
        volumes,
        certificates,
        hosted_dns_hostname,
        incomplete_ids,
        observed_at: _,
    } = previous;
    let RuntimeWatchFrame {
        machines: next_machines,
        containers: next_containers,
        volumes: next_volumes,
        certificates: next_certificates,
        hosted_dns_hostname: next_hosted_dns_hostname,
        incomplete_ids: next_incomplete_ids,
        observed_at: _,
    } = next;
    machines != next_machines
        || containers != next_containers
        || volumes != next_volumes
        || certificates != next_certificates
        || hosted_dns_hostname != next_hosted_dns_hostname
        || incomplete_ids != next_incomplete_ids
}

/// Assemble one complete Runtime Watch frame.
///
/// Services can be derived from the replicated Containers. Certificate Material,
/// HTTP-01 challenge bytes, hosted DNS token/endpoint, Relay credentials, and Pairing
/// credentials are not copied onto the frame. Incomplete IDs are preserved as IDs.
///
/// When `telemetry` is `None`, replicated Machine rows stay and membership is unknown
/// except the entry Machine, which is Up, with no selected endpoint or RTT.
#[must_use]
pub(crate) fn assemble_runtime_watch_frame(
    snapshot: RuntimeWatchSnapshot,
    entry_id: &MachineId,
    telemetry: Option<&RuntimeWatchTelemetry>,
    global_reconcile_failures: Vec<ployz_core::GlobalReconcileFailureObservation>,
    observed_at: String,
) -> RuntimeWatchFrame {
    let mut machines = match telemetry {
        Some(telemetry) => telemetry.overlay(snapshot.machines.observations, entry_id),
        None => unavailable_machine_observations(snapshot.machines.observations, entry_id),
    };
    if let Some(entry) = machines
        .iter_mut()
        .find(|observation| observation.machine.id == *entry_id)
    {
        entry.global_reconcile_failures = global_reconcile_failures;
    }
    let mut containers = snapshot.containers.observations;
    containers.sort_by_key(|container| container.container_id);
    let certificates = snapshot
        .certificates
        .observations
        .into_iter()
        .map(|(hostname, row)| redact_certificate(hostname, &row))
        .collect();
    RuntimeWatchFrame {
        machines,
        containers,
        volumes: snapshot.volumes.observations,
        certificates,
        hosted_dns_hostname: snapshot.hosted_dns.map(|reservation| reservation.name),
        incomplete_ids: RuntimeWatchIncompleteIds {
            machines: snapshot.machines.incomplete_ids,
            containers: snapshot.containers.incomplete_ids,
            volumes: snapshot.volumes.incomplete_ids,
            certificates: snapshot.certificates.incomplete_ids,
        },
        observed_at,
    }
}

fn unavailable_machine_observations(
    machines: Vec<Machine>,
    entry_id: &MachineId,
) -> Vec<MachineObservation> {
    machines
        .into_iter()
        .map(|machine| {
            let membership = if &machine.id == entry_id {
                MembershipObservation::Up
            } else {
                MembershipObservation::Unknown
            };
            MachineObservation::new(machine, membership)
        })
        .collect()
}

fn redact_certificate(hostname: IngressHost, row: &CertificateRow) -> CertificateObservation {
    let backoff = row.clock().map(certificate_backoff);
    let status = if row.material().is_some() {
        CertificateAvailability::Available
    } else if row.challenge().is_some() {
        CertificateAvailability::Pending
    } else if backoff.is_some() || row.last_error().is_some() {
        CertificateAvailability::Failure
    } else {
        CertificateAvailability::Unknown
    };
    CertificateObservation {
        hostname,
        status,
        last_error: row.last_error().map(str::to_owned),
        backoff,
    }
}

fn certificate_backoff(clock: IssuanceClock) -> CertificateBackoff {
    CertificateBackoff {
        failure_kind: match clock.last_failure() {
            IssuanceFailure::DoesNotResolve => CertificateFailureKind::DoesNotResolve,
            IssuanceFailure::ResolvesElsewhere => CertificateFailureKind::ResolvesElsewhere,
            IssuanceFailure::Authority => CertificateFailureKind::Authority,
        },
        next_attempt_at: rfc3339(clock.next_attempt_at()),
        failures: clock.failures(),
    }
}

fn rfc3339(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
#[path = "runtime_watch_tests.rs"]
mod tests;
