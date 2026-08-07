//! Operation listing and durable evidence attachment.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

use ployz_core::corrosion::{
    CorrosionDeployOutcome, CorrosionDeployState, CorrosionOperation, CorrosionOperationState,
    CorrosionTimestamp, MachineLoadBand, OperationDocument,
};
use ployz_core::ids::MachineRowId;
use ployz_core::machine::MachineName;
use ployz_core::placement::{PlacementElimination, PlacementEliminationReason, PlacementShortfall};
use ployz_core::{
    LensCollection, LensSnapshot, OperationEvidence, OperationEvidenceEvent, OperationWatchEvent,
    OperationWatchRefusal, PlacementBid, SilenceClassification, SilentMachine,
    operation_watch_route,
};

use crate::commands::{OpsCommand, OpsListCommand, OpsWatchCommand};
use crate::mesh::http::{DEFAULT_MESH_SSE_IDLE_TIMEOUT, MAX_MESH_SSE_FRAME_BYTES, SseReply};
use crate::remote::{OperatorRemote, OperatorRemoteError};

const RECONNECT_WINDOW: Duration = Duration::from_secs(15);
const RECONNECT_DELAYS: &[Duration] = &[
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

pub async fn execute(command: OpsCommand) -> Result<String, OpsExecutionError> {
    match command {
        OpsCommand::List(command) => list(command).await,
        OpsCommand::Watch(command) => {
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            watch_to(&command, &mut stdout).await?;
            Ok(String::new())
        }
    }
}

async fn list(command: OpsListCommand) -> Result<String, OpsExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let snapshot = remote.lens(LensCollection::Operations).await?;
    let LensSnapshot::Operations { mut rows } = snapshot else {
        return Err(OpsExecutionError::WrongLens);
    };
    rows.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let now = current_timestamp()?;
    let mut output = String::from("ID\tKIND\tSTATE\tDRIVER\n");
    for row in rows {
        let (kind, state) = listed_kind_state(&row.document, now);
        output.push_str(row.id.as_str());
        output.push('\t');
        output.push_str(kind);
        output.push('\t');
        output.push_str(state);
        output.push('\t');
        output.push_str(row.document.machine_id.as_str());
        output.push('\n');
    }
    Ok(output)
}

/// Replays and follows an operation, suppressing stable sequence duplicates after reconnect.
pub async fn watch_to(
    command: &OpsWatchCommand,
    output: &mut impl std::io::Write,
) -> Result<(), OpsExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let route = operation_watch_route(&command.operation_id);
    watch_with_attach_to(
        || {
            let remote = remote.clone();
            let route = route.clone();
            async move {
                remote
                    .request_sse_with_refusal::<(), OperationWatchEvent, OperationWatchRefusal>(
                        hyper::Method::GET,
                        &route,
                        None,
                        DEFAULT_MESH_SSE_IDLE_TIMEOUT,
                        MAX_MESH_SSE_FRAME_BYTES,
                    )
                    .await
                    .map_err(OpsExecutionError::Remote)
            }
        },
        output,
    )
    .await
}

async fn watch_with_attach_to<Attach, AttachFuture>(
    mut attach: Attach,
    output: &mut impl std::io::Write,
) -> Result<(), OpsExecutionError>
where
    Attach: FnMut() -> AttachFuture,
    AttachFuture: Future<
        Output = Result<SseReply<OperationWatchEvent, OperationWatchRefusal>, OpsExecutionError>,
    >,
{
    let mut reconnect_started: Option<Instant> = None;
    let mut reconnect = 0usize;
    let mut last_sequence = 0u64;
    // The wire carries machine row ids; bids in gathered evidence teach the
    // watcher the human names, so later lines render names instead of ids.
    let mut machine_names: BTreeMap<MachineRowId, MachineName> = BTreeMap::new();

    loop {
        let attach = attach();
        let reply = if let Some(started) = reconnect_started {
            let Some(remaining) = RECONNECT_WINDOW.checked_sub(started.elapsed()) else {
                return Err(OpsExecutionError::StreamDown {
                    last_error: "operation stream reconnect window elapsed".to_owned(),
                });
            };
            match tokio::time::timeout(remaining, attach).await {
                Ok(reply) => reply,
                Err(_) => {
                    return Err(OpsExecutionError::StreamDown {
                        last_error: "operation stream reconnect window elapsed".to_owned(),
                    });
                }
            }
        } else {
            attach.await
        };
        let mut stream = match reply {
            Ok(SseReply::Stream(stream)) => stream,
            Ok(SseReply::Refused(refusal)) => return Err(refusal.into()),
            Err(error) => {
                wait_to_reconnect(&mut reconnect_started, &mut reconnect, error.to_string())
                    .await?;
                continue;
            }
        };

        let disconnect_reason = loop {
            let next_event = if let Some(started) = reconnect_started {
                let Some(remaining) = RECONNECT_WINDOW.checked_sub(started.elapsed()) else {
                    return Err(OpsExecutionError::StreamDown {
                        last_error: "operation stream reconnect window elapsed".to_owned(),
                    });
                };
                match tokio::time::timeout(remaining, stream.next_event()).await {
                    Ok(event) => event,
                    Err(_) => {
                        return Err(OpsExecutionError::StreamDown {
                            last_error: "operation stream reconnect window elapsed".to_owned(),
                        });
                    }
                }
            } else {
                stream.next_event().await
            };
            match next_event {
                Ok(Some(envelope)) => {
                    let expected_name = envelope.data.event_name();
                    if envelope.event.as_deref() != Some(expected_name) {
                        return Err(OpsExecutionError::UnexpectedEventName {
                            expected: expected_name,
                            found: envelope.event,
                        });
                    }
                    match envelope.data {
                        OperationWatchEvent::Evidence { event } => {
                            let sequence = event.sequence.get();
                            if !accept_replayed_evidence(
                                &mut last_sequence,
                                &mut reconnect_started,
                                &mut reconnect,
                                sequence,
                            )? {
                                continue;
                            }
                            let terminal = match &event.evidence {
                                OperationEvidence::Terminal { operation } => {
                                    Some(operation_terminal_succeeded(operation)?)
                                }
                                OperationEvidence::Created
                                | OperationEvidence::OpClaimWon
                                | OperationEvidence::OpClaimLost { .. }
                                | OperationEvidence::PlacementGathered { .. }
                                | OperationEvidence::PlacementPicked { .. }
                                | OperationEvidence::DebrisSwept { .. }
                                | OperationEvidence::PullingImage
                                | OperationEvidence::ImageResolved
                                | OperationEvidence::ContainerCreated { .. }
                                | OperationEvidence::ContainerStarted { .. }
                                | OperationEvidence::HealthGateSkipped
                                | OperationEvidence::IncumbentStopped { .. }
                                | OperationEvidence::IncumbentRestarted { .. }
                                | OperationEvidence::PromotionPrepared
                                | OperationEvidence::RowsCommitted
                                | OperationEvidence::ServiceClaimWon
                                | OperationEvidence::ServiceClaimLost { .. }
                                | OperationEvidence::Drained
                                | OperationEvidence::IncumbentRemoved { .. }
                                | OperationEvidence::Unrecognized => None,
                            };
                            if let OperationEvidence::PlacementGathered { bids, .. } =
                                &event.evidence
                            {
                                for bid in bids {
                                    machine_names
                                        .insert(bid.machine_id.clone(), bid.machine_name.clone());
                                }
                            }
                            render_evidence(output, &event, &machine_names)?;
                            match terminal {
                                Some(true) => return Ok(()),
                                Some(false) => {
                                    let OperationEvidence::Terminal { operation } = &event.evidence
                                    else {
                                        unreachable!("terminal outcome came from terminal evidence")
                                    };
                                    let (kind, state) = operation_kind_state(operation);
                                    return Err(OpsExecutionError::OperationFailed { kind, state });
                                }
                                None => {}
                            }
                        }
                        OperationWatchEvent::Terminal { refusal } => return Err((*refusal).into()),
                    }
                }
                Ok(None) => {
                    break "operation evidence stream ended before terminal evidence".to_owned();
                }
                Err(error) => break error.to_string(),
            }
        };
        wait_to_reconnect(&mut reconnect_started, &mut reconnect, disconnect_reason).await?;
    }
}

fn accept_replayed_evidence(
    last_sequence: &mut u64,
    reconnect_started: &mut Option<Instant>,
    reconnect: &mut usize,
    sequence: u64,
) -> Result<bool, OpsExecutionError> {
    *reconnect_started = None;
    *reconnect = 0;
    accept_sequence(last_sequence, sequence)
}

fn accept_sequence(last_sequence: &mut u64, sequence: u64) -> Result<bool, OpsExecutionError> {
    if sequence <= *last_sequence {
        return Ok(false);
    }
    if sequence != *last_sequence + 1 {
        return Err(OpsExecutionError::EvidenceGap {
            expected: *last_sequence + 1,
            found: sequence,
        });
    }
    *last_sequence = sequence;
    Ok(true)
}

async fn wait_to_reconnect(
    started: &mut Option<Instant>,
    reconnect: &mut usize,
    last_error: String,
) -> Result<(), OpsExecutionError> {
    let started = *started.get_or_insert_with(Instant::now);
    let Some(delay) = RECONNECT_DELAYS.get(*reconnect).copied() else {
        return Err(OpsExecutionError::StreamDown { last_error });
    };
    if started.elapsed().saturating_add(delay) > RECONNECT_WINDOW {
        return Err(OpsExecutionError::StreamDown { last_error });
    }
    *reconnect += 1;
    tokio::time::sleep(delay).await;
    Ok(())
}

fn render_evidence(
    output: &mut impl std::io::Write,
    event: &OperationEvidenceEvent,
    machine_names: &BTreeMap<MachineRowId, MachineName>,
) -> Result<(), OpsExecutionError> {
    let detail = match &event.evidence {
        OperationEvidence::Created => "created".to_owned(),
        OperationEvidence::PullingImage => "pulling image".to_owned(),
        OperationEvidence::ImageResolved => "image resolved".to_owned(),
        OperationEvidence::ContainerCreated {
            container_id,
            machine,
        } => {
            format!(
                "container created {}{}",
                container_id.as_str(),
                on_machine(machine.as_ref(), machine_names)
            )
        }
        OperationEvidence::ContainerStarted {
            container_id,
            machine,
        } => {
            format!(
                "container started {}{}",
                container_id.as_str(),
                on_machine(machine.as_ref(), machine_names)
            )
        }
        OperationEvidence::OpClaimWon => "operation claim won".to_owned(),
        OperationEvidence::OpClaimLost { winner } => format!("operation claim lost to {winner}"),
        OperationEvidence::PlacementGathered { bids, silent } => {
            render_placement_gathered(bids, silent)
        }
        OperationEvidence::PlacementPicked {
            targets,
            eliminations,
            shortfall,
        } => render_placement_picked(targets, eliminations, shortfall.as_ref(), machine_names),
        OperationEvidence::DebrisSwept { removed, machine } => {
            format!(
                "swept {} leftover container(s){}",
                removed.len(),
                on_machine(machine.as_ref(), machine_names)
            )
        }
        OperationEvidence::HealthGateSkipped => "health gate skipped".to_owned(),
        OperationEvidence::IncumbentStopped {
            container_id,
            machine,
        } => {
            format!(
                "incumbent stopped {}{}",
                container_id.as_str(),
                on_machine(machine.as_ref(), machine_names)
            )
        }
        OperationEvidence::IncumbentRestarted {
            container_id,
            machine,
        } => {
            format!(
                "incumbent restarted {}{}",
                container_id.as_str(),
                on_machine(machine.as_ref(), machine_names)
            )
        }
        OperationEvidence::IncumbentRemoved {
            container_id,
            machine,
        } => {
            format!(
                "incumbent removed {}{}",
                container_id.as_str(),
                on_machine(machine.as_ref(), machine_names)
            )
        }
        OperationEvidence::Drained => "drained".to_owned(),
        OperationEvidence::PromotionPrepared => "promotion prepared".to_owned(),
        OperationEvidence::RowsCommitted => "rows committed".to_owned(),
        OperationEvidence::ServiceClaimWon => "service claim won".to_owned(),
        OperationEvidence::ServiceClaimLost { winner } => format!("service claim lost to {winner}"),
        OperationEvidence::Terminal { operation } => {
            let (kind, state) = operation_kind_state(operation);
            format!("terminal {kind} {state}")
        }
        OperationEvidence::Unrecognized => "unrecognized evidence from a newer daemon".to_owned(),
    };
    writeln!(
        output,
        "{}\t{}\t{detail}",
        event.sequence.get(),
        event.timestamp
    )
    .map_err(OpsExecutionError::Output)?;
    output.flush().map_err(OpsExecutionError::Output)
}

fn on_machine(
    machine: Option<&MachineRowId>,
    machine_names: &BTreeMap<MachineRowId, MachineName>,
) -> String {
    machine
        .map(|machine| format!(" on {}", machine_label(machine, machine_names)))
        .unwrap_or_default()
}

/// The machine's human name when a bid taught it, otherwise its row id.
pub(crate) fn machine_label(
    machine: &MachineRowId,
    machine_names: &BTreeMap<MachineRowId, MachineName>,
) -> String {
    machine_names
        .get(machine)
        .map_or_else(|| machine.to_string(), |name| name.as_str().to_owned())
}

/// One operator-facing phrase per tier-0 elimination reason.
pub(crate) fn elimination_reason(
    reason: &PlacementEliminationReason,
    machine_names: &BTreeMap<MachineRowId, MachineName>,
) -> String {
    match reason {
        PlacementEliminationReason::Draining => "draining".to_owned(),
        PlacementEliminationReason::PlatformUnsupported { architecture } => {
            format!("architecture {architecture} does not run this image")
        }
        PlacementEliminationReason::FreeDiskBelowFloor { free_disk_bytes } => {
            format!(
                "free disk below the placement floor ({} free)",
                format_bytes(*free_disk_bytes)
            )
        }
        PlacementEliminationReason::VolumeNotHeld { holder } => {
            format!(
                "does not hold the data volume (held by {})",
                machine_label(holder, machine_names)
            )
        }
        PlacementEliminationReason::OutsidePinSet => "outside the pin set".to_owned(),
    }
}

fn format_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB {
        format!("{}.{} GiB", bytes / GIB, (bytes % GIB) * 10 / GIB)
    } else if bytes >= MIB {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} B")
    }
}

fn handshake_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3600)
    }
}

const fn load_band(load: MachineLoadBand) -> &'static str {
    match load {
        MachineLoadBand::Idle => "idle",
        MachineLoadBand::Normal => "normal",
        MachineLoadBand::Hot => "hot",
    }
}

fn render_placement_gathered(bids: &[PlacementBid], silent: &[SilentMachine]) -> String {
    let mut detail = format!(
        "placement gathered: {} bid(s), {} silent",
        bids.len(),
        silent.len()
    );
    for bid in bids {
        let volumes = if bid.volumes_held.is_empty() {
            "no volumes".to_owned()
        } else {
            format!(
                "holds {}",
                bid.volumes_held
                    .iter()
                    .map(|volume| volume.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        detail.push_str(&format!(
            "\n  bid from {}: {}, {} container(s), {volumes}",
            bid.machine_name.as_str(),
            load_band(bid.load),
            bid.total_container_count
        ));
    }
    for entry in silent {
        let classification = match &entry.classification {
            SilenceClassification::ExpectedSilent {
                handshake_age_seconds,
            } => format!(
                "expected-silent (handshake {})",
                handshake_age(*handshake_age_seconds)
            ),
            SilenceClassification::AnomalousSilent { reason } => {
                format!("anomalous-silent ({reason})")
            }
        };
        detail.push_str(&format!(
            "\n  no bid from {}: {classification}",
            entry.machine_id
        ));
    }
    detail
}

fn render_placement_picked(
    targets: &[MachineRowId],
    eliminations: &[PlacementElimination],
    shortfall: Option<&PlacementShortfall>,
    machine_names: &BTreeMap<MachineRowId, MachineName>,
) -> String {
    // Stacking shows as a replica count: a machine appears once per replica
    // in the wire's target list, grouped here in first-appearance order.
    let mut grouped: Vec<(&MachineRowId, usize)> = Vec::new();
    for target in targets {
        if let Some(entry) = grouped.iter_mut().find(|(machine, _)| *machine == target) {
            entry.1 += 1;
        } else {
            grouped.push((target, 1));
        }
    }
    let mut detail = format!(
        "placement picked: {} replica(s) on {} machine(s)",
        targets.len(),
        grouped.len()
    );
    for (machine, replicas) in grouped {
        let label = machine_label(machine, machine_names);
        if replicas > 1 {
            detail.push_str(&format!("\n  target {label} ({replicas} replicas)"));
        } else {
            detail.push_str(&format!("\n  target {label}"));
        }
    }
    for elimination in eliminations {
        detail.push_str(&format!(
            "\n  eliminated {}: {}",
            machine_label(&elimination.machine_id, machine_names),
            elimination_reason(&elimination.reason, machine_names)
        ));
    }
    if let Some(shortfall) = shortfall {
        detail.push_str(&format!(
            "\n  shortfall: requested {}, placed {}",
            shortfall.requested.get(),
            shortfall.placed
        ));
    }
    detail
}

fn current_timestamp() -> Result<CorrosionTimestamp, OpsExecutionError> {
    let formatted = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| OpsExecutionError::Clock(error.to_string()))?;
    CorrosionTimestamp::try_new(formatted)
        .map_err(|error| OpsExecutionError::Clock(error.to_string()))
}

/// The listing state: a non-terminal deploy whose driver heartbeat has gone
/// stale renders `stalled` instead of its stored `created`/`running` state.
fn listed_kind_state(
    operation: &OperationDocument,
    now: CorrosionTimestamp,
) -> (&'static str, &'static str) {
    let (kind, state) = operation_kind_state(operation);
    if matches!(operation.operation(), CorrosionOperation::Deploy { .. })
        && operation.is_stalled(now)
    {
        return (kind, "stalled");
    }
    (kind, state)
}

fn operation_kind_state(operation: &OperationDocument) -> (&'static str, &'static str) {
    match operation.operation() {
        CorrosionOperation::Build { state, .. } => ("build", basic_state(state)),
        CorrosionOperation::Deploy { state, .. } => ("deploy", deploy_state(state)),
        CorrosionOperation::MachineAdd { state, .. } => ("machine-add", basic_state(state)),
        CorrosionOperation::MachineRemove { state, .. } => ("machine-remove", basic_state(state)),
        CorrosionOperation::Recovery { state, .. } => ("recovery", basic_state(state)),
    }
}

fn operation_terminal_succeeded(operation: &OperationDocument) -> Result<bool, OpsExecutionError> {
    match operation.operation() {
        CorrosionOperation::Build { state, .. }
        | CorrosionOperation::MachineAdd { state, .. }
        | CorrosionOperation::MachineRemove { state, .. }
        | CorrosionOperation::Recovery { state, .. } => match state {
            CorrosionOperationState::Succeeded { .. } => Ok(true),
            CorrosionOperationState::Failed { .. } => Ok(false),
            CorrosionOperationState::Created { .. } | CorrosionOperationState::Running { .. } => {
                Err(OpsExecutionError::NonterminalTerminalEvidence)
            }
        },
        CorrosionOperation::Deploy { state, .. } => match state {
            CorrosionDeployState::Terminal { outcome, .. } => match outcome {
                CorrosionDeployOutcome::Completed { .. }
                | CorrosionDeployOutcome::CompletedWithWarnings { .. } => Ok(true),
                CorrosionDeployOutcome::PartiallyCompleted { .. }
                | CorrosionDeployOutcome::PartiallyCompletedWithWarnings { .. }
                | CorrosionDeployOutcome::Failed { .. }
                | CorrosionDeployOutcome::Cancelled { .. } => Ok(false),
            },
            CorrosionDeployState::Created { .. } | CorrosionDeployState::Running { .. } => {
                Err(OpsExecutionError::NonterminalTerminalEvidence)
            }
        },
    }
}

const fn basic_state(state: &CorrosionOperationState) -> &'static str {
    match state {
        CorrosionOperationState::Created { .. } => "created",
        CorrosionOperationState::Running { .. } => "running",
        CorrosionOperationState::Succeeded { .. } => "succeeded",
        CorrosionOperationState::Failed { .. } => "failed",
    }
}

const fn deploy_state(state: &CorrosionDeployState) -> &'static str {
    match state {
        CorrosionDeployState::Created { .. } => "created",
        CorrosionDeployState::Running { .. } => "running",
        CorrosionDeployState::Terminal { outcome, .. } => match outcome {
            CorrosionDeployOutcome::Completed { .. } => "completed",
            CorrosionDeployOutcome::CompletedWithWarnings { .. } => "completed-with-warnings",
            CorrosionDeployOutcome::PartiallyCompleted { .. } => "partially-completed",
            CorrosionDeployOutcome::PartiallyCompletedWithWarnings { .. } => {
                "partially-completed-with-warnings"
            }
            CorrosionDeployOutcome::Failed { .. } => "failed",
            CorrosionDeployOutcome::Cancelled { .. } => "cancelled",
        },
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpsExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error("operations lens returned the wrong collection")]
    WrongLens,
    #[error("operation stream event name was {found:?}, expected {expected}")]
    UnexpectedEventName {
        expected: &'static str,
        found: Option<String>,
    },
    #[error("operation evidence skipped sequence {expected}; received {found}")]
    EvidenceGap { expected: u64, found: u64 },
    #[error("operation stream labelled a nonterminal summary as terminal")]
    NonterminalTerminalEvidence,
    #[error("operation finished unsuccessfully: {kind} {state}")]
    OperationFailed {
        kind: &'static str,
        state: &'static str,
    },
    #[error("operation stream remained unavailable after bounded reconnects: {last_error}")]
    StreamDown { last_error: String },
    #[error("cannot write operation progress: {0}")]
    Output(std::io::Error),
    #[error("cannot read the local clock: {0}")]
    Clock(String),
    #[error("operation {operation_id} was not found")]
    NotFound { operation_id: String },
    #[error("operation driver is no longer rostered: {detail}")]
    OwnerNoLongerRostered { detail: String },
    #[error("operation driver is dark: {detail}")]
    DriverDark { detail: String },
    #[error("durable detail is unavailable for operation {operation_id}")]
    DetailUnavailable { operation_id: String },
    #[error("operation watch proxy loop was refused")]
    ProxyLoop,
}

impl From<OperationWatchRefusal> for OpsExecutionError {
    fn from(refusal: OperationWatchRefusal) -> Self {
        match refusal {
            OperationWatchRefusal::NotFound { operation_id } => Self::NotFound {
                operation_id: operation_id.to_string(),
            },
            OperationWatchRefusal::OwnerNoLongerRostered {
                operation,
                observation,
            } => Self::OwnerNoLongerRostered {
                detail: format!(
                    "operation {} owned by {}; observation {observation:?}",
                    operation.operation_id, operation.operation.machine_id
                ),
            },
            OperationWatchRefusal::DriverDark {
                operation,
                observation,
            } => Self::DriverDark {
                detail: format!(
                    "operation {} owned by {}; observation {observation:?}",
                    operation.operation_id, operation.operation.machine_id
                ),
            },
            OperationWatchRefusal::DetailUnavailable { operation } => Self::DetailUnavailable {
                operation_id: operation.operation_id.to_string(),
            },
            OperationWatchRefusal::ProxyLoop => Self::ProxyLoop,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use ployz_core::OperationEvidenceSequence;
    use ployz_core::corrosion::{
        CorrosionDeployOutcome, CorrosionDeployServiceResult, CorrosionDeployTargets,
        CorrosionDeployTransition, CorrosionDocumentVersion, CorrosionTimestamp,
        OperationInitiator,
    };
    use ployz_core::ids::{ClusterId, MachineRowId, NamespaceRowId, PeerId, ServiceRowId};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};

    use crate::mesh::http::MeshApiClient;

    use super::*;

    #[test]
    fn replay_dedup_keeps_output_monotonic_and_detects_gaps() {
        let mut last = 0;
        assert!(accept_sequence(&mut last, 1).expect("first"));
        assert!(accept_sequence(&mut last, 2).expect("second"));
        assert!(!accept_sequence(&mut last, 1).expect("replayed first"));
        assert!(!accept_sequence(&mut last, 2).expect("replayed second"));
        assert!(accept_sequence(&mut last, 3).expect("new third"));
        assert!(matches!(
            accept_sequence(&mut last, 5),
            Err(OpsExecutionError::EvidenceGap {
                expected: 4,
                found: 5
            })
        ));
    }

    #[test]
    fn duplicate_only_replay_marks_a_reconnect_healthy() {
        let mut last = 2;
        let mut reconnect_started = Some(Instant::now());
        let mut reconnect = 3;

        assert!(
            !accept_replayed_evidence(&mut last, &mut reconnect_started, &mut reconnect, 1,)
                .expect("duplicate replay is valid")
        );
        assert_eq!(last, 2);
        assert_eq!(reconnect_started, None);
        assert_eq!(reconnect, 0);
    }

    #[tokio::test]
    async fn dropped_stream_replays_duplicates_then_renders_terminal_once() {
        let first = evidence_event(1, OperationEvidence::Created);
        let second = evidence_event(2, OperationEvidence::PullingImage);
        let terminal = evidence_event(
            3,
            OperationEvidence::Terminal {
                operation: Box::new(completed_deploy()),
            },
        );
        let (first_client, first_server) = tokio::io::duplex(16 * 1024);
        let (second_client, second_server) = tokio::io::duplex(16 * 1024);
        let first_task = tokio::spawn(serve_evidence(
            first_server,
            vec![first.clone(), second.clone()],
            StreamEnding::Drop,
        ));
        let second_task = tokio::spawn(serve_evidence(
            second_server,
            vec![first, second, terminal],
            StreamEnding::Finish,
        ));
        let mut streams = VecDeque::from([first_client, second_client]);
        let mut rendered = Vec::new();

        watch_with_attach_to(
            || {
                let stream = streams.pop_front().expect("bounded reconnect stream");
                async move {
                    MeshApiClient::default()
                        .request_sse_with_refusal::<_, (), OperationWatchEvent, OperationWatchRefusal>(
                            stream,
                            hyper::Method::GET,
                            "/operations/01ARZ3NDEKTSV4RRFFQ69G5FAA/watch",
                            None,
                            DEFAULT_MESH_SSE_IDLE_TIMEOUT,
                            MAX_MESH_SSE_FRAME_BYTES,
                        )
                        .await
                        .map_err(|error| {
                            OpsExecutionError::Remote(OperatorRemoteError::Api(error))
                        })
                }
            },
            &mut rendered,
        )
        .await
        .expect("replayed terminal operation succeeds");
        first_task.await.expect("first SSE task");
        second_task.await.expect("second SSE task");

        let rendered = String::from_utf8(rendered).expect("rendered UTF-8");
        let sequences = rendered
            .lines()
            .map(|line| {
                line.split_once('\t')
                    .expect("sequence column")
                    .0
                    .parse::<u64>()
                    .expect("numeric sequence")
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![1, 2, 3]);
        assert_eq!(rendered.matches("\tcreated\n").count(), 1);
        assert_eq!(rendered.matches("\tpulling image\n").count(), 1);
        assert_eq!(rendered.matches("\tterminal deploy completed\n").count(), 1);
    }

    #[test]
    fn placement_gathered_renders_one_line_per_bid_and_silence() {
        let bid = PlacementBid {
            machine_id: MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAC").expect("machine id"),
            machine_name: ployz_core::machine::MachineName::try_new("web-1").expect("name"),
            architecture: "x86_64".to_owned(),
            lifecycle: ployz_core::machine::MachineLifecycle::Active,
            free_disk_bytes: 10 * 1024 * 1024 * 1024,
            free_memory_bytes: 1024 * 1024 * 1024,
            load: MachineLoadBand::Idle,
            total_container_count: 3,
            service_containers: Vec::new(),
            volumes_held: [ployz_core::deploy::VolumeName::try_new("data").expect("volume")]
                .into_iter()
                .collect(),
        };
        let expected = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAD").expect("machine id");
        let anomalous = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAE").expect("machine id");
        let detail = render_placement_gathered(
            &[bid],
            &[
                SilentMachine {
                    machine_id: expected.clone(),
                    classification: SilenceClassification::ExpectedSilent {
                        handshake_age_seconds: 41 * 60,
                    },
                },
                SilentMachine {
                    machine_id: anomalous.clone(),
                    classification: SilenceClassification::AnomalousSilent {
                        reason: "bid timed out".to_owned(),
                    },
                },
            ],
        );
        assert!(detail.starts_with("placement gathered: 1 bid(s), 2 silent"));
        assert!(detail.contains("\n  bid from web-1: idle, 3 container(s), holds data"));
        assert!(detail.contains(&format!(
            "\n  no bid from {expected}: expected-silent (handshake 41m)"
        )));
        assert!(detail.contains(&format!(
            "\n  no bid from {anomalous}: anomalous-silent (bid timed out)"
        )));
    }

    #[test]
    fn placement_picked_groups_stacked_targets_and_names_machines() {
        let stacked = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAC").expect("machine id");
        let single = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAD").expect("machine id");
        let eliminated = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAE").expect("machine id");
        let names = [
            (
                stacked.clone(),
                ployz_core::machine::MachineName::try_new("web-1").expect("name"),
            ),
            (
                single.clone(),
                ployz_core::machine::MachineName::try_new("web-2").expect("name"),
            ),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        let detail = render_placement_picked(
            &[stacked.clone(), single, stacked],
            &[PlacementElimination {
                machine_id: eliminated.clone(),
                reason: PlacementEliminationReason::Draining,
            }],
            Some(&PlacementShortfall {
                requested: ployz_core::corrosion::ServiceReplicaCount::try_new(3)
                    .expect("replica count"),
                placed: 2,
            }),
            &names,
        );
        assert!(detail.starts_with("placement picked: 3 replica(s) on 2 machine(s)"));
        assert!(detail.contains("\n  target web-1 (2 replicas)"));
        assert!(detail.contains("\n  target web-2"));
        assert!(detail.contains(&format!("\n  eliminated {eliminated}: draining")));
        assert!(detail.contains("\n  shortfall: requested 3, placed 2"));
    }

    #[test]
    fn ops_list_renders_stalled_only_for_nonterminal_deploys_with_stale_heartbeats() {
        let created_at = timestamp("2026-08-05T12:00:00Z");
        let now = timestamp("2026-08-05T12:05:00Z");

        let stalled = created_deploy(created_at);
        assert_eq!(listed_kind_state(&stalled, now), ("deploy", "stalled"));

        let running_stalled = created_deploy(created_at)
            .transition_deploy(CorrosionDeployTransition::Running {
                started_at: timestamp("2026-08-05T12:00:01Z"),
            })
            .expect("running deploy");
        assert_eq!(
            listed_kind_state(&running_stalled, now),
            ("deploy", "stalled")
        );

        let beating = created_deploy(created_at)
            .refresh_heartbeat(timestamp("2026-08-05T12:04:30Z"))
            .expect("nonterminal heartbeat refresh");
        assert_eq!(listed_kind_state(&beating, now), ("deploy", "created"));

        assert_eq!(
            listed_kind_state(&completed_deploy(), timestamp("2026-08-06T12:00:00Z")),
            ("deploy", "completed")
        );
    }

    #[derive(Clone, Copy)]
    enum StreamEnding {
        Drop,
        Finish,
    }

    async fn serve_evidence(
        mut stream: DuplexStream,
        events: Vec<OperationEvidenceEvent>,
        ending: StreamEnding,
    ) {
        let mut request = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            stream
                .read_exact(&mut byte)
                .await
                .expect("read operation watch request");
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        assert!(
            String::from_utf8_lossy(&request)
                .starts_with("GET /operations/01ARZ3NDEKTSV4RRFFQ69G5FAA/watch HTTP/1.1\r\n")
        );
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("write operation watch response");
        for event in events {
            let envelope = OperationWatchEvent::Evidence { event };
            let data = serde_json::to_string(&envelope).expect("event JSON");
            let frame = format!("event: evidence\ndata: {data}\n\n");
            write_http_chunk(&mut stream, frame.as_bytes()).await;
        }
        match ending {
            StreamEnding::Drop => {}
            StreamEnding::Finish => {
                stream
                    .write_all(b"0\r\n\r\n")
                    .await
                    .expect("finish operation watch response");
            }
        }
    }

    async fn write_http_chunk(stream: &mut DuplexStream, body: &[u8]) {
        stream
            .write_all(format!("{:x}\r\n", body.len()).as_bytes())
            .await
            .expect("write chunk length");
        stream.write_all(body).await.expect("write chunk body");
        stream.write_all(b"\r\n").await.expect("finish chunk body");
    }

    fn evidence_event(sequence: u64, evidence: OperationEvidence) -> OperationEvidenceEvent {
        OperationEvidenceEvent {
            sequence: OperationEvidenceSequence::try_new(sequence).expect("positive sequence"),
            timestamp: timestamp("2026-08-05T12:34:56Z"),
            evidence,
        }
    }

    fn created_deploy(created_at: CorrosionTimestamp) -> OperationDocument {
        let service_id =
            ServiceRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAE").expect("service row id");
        OperationDocument::deploy_created(
            CorrosionDocumentVersion::V1,
            ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAB").expect("cluster id"),
            MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAC").expect("machine row id"),
            OperationInitiator::Peer {
                peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAD").expect("peer id"),
            },
            NamespaceRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAF").expect("namespace row id"),
            CorrosionDeployTargets::try_new(vec![service_id]).expect("deploy targets"),
            created_at,
        )
    }

    fn completed_deploy() -> OperationDocument {
        let service_id =
            ServiceRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAE").expect("service row id");
        created_deploy(timestamp("2026-08-05T12:34:54Z"))
            .transition_deploy(CorrosionDeployTransition::Running {
                started_at: timestamp("2026-08-05T12:34:55Z"),
            })
            .expect("running deploy")
            .transition_deploy(CorrosionDeployTransition::Terminal {
                completed_at: timestamp("2026-08-05T12:34:56Z"),
                outcome: CorrosionDeployOutcome::completed(vec![
                    CorrosionDeployServiceResult::completed(service_id),
                ])
                .expect("completed deploy outcome"),
            })
            .expect("terminal deploy")
    }

    fn timestamp(value: &str) -> CorrosionTimestamp {
        CorrosionTimestamp::try_new(value).expect("timestamp")
    }
}
