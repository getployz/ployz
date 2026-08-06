use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ployz_core::corrosion::{
    ContainerDocument, CorrosionDeployServiceResult, CorrosionDeployServiceResultKind,
    CorrosionDeployState, CorrosionOperation, CorrosionOperationState, CorrosionTimestamp,
    NamespaceDocument, OperationDocument, ServiceDocument,
};
use ployz_core::ids::{
    ContainerId, CorrosionUlid, MachineRowId, NamespaceRowId, OperationRowId, ServiceRowId,
};
use ployz_core::{OperationEvidence, OperationEvidenceEvent, OperationEvidenceSequence};
use serde::{Deserialize, Serialize};
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, watch};

const EVIDENCE_FORMAT_VERSION: u32 = 1;
const MAX_EVIDENCE_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const DEFAULT_MAX_EVIDENCE_LINE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TornTailPolicy {
    Refuse,
    Repair,
}

/// The immutable owner identity at the head of every operation evidence file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvidenceIdentity {
    format_version: u32,
    operation_id: OperationRowId,
    driver_machine_id: MachineRowId,
}

impl EvidenceIdentity {
    #[must_use]
    pub(super) const fn new(operation_id: OperationRowId, driver_machine_id: MachineRowId) -> Self {
        Self {
            format_version: EVIDENCE_FORMAT_VERSION,
            operation_id,
            driver_machine_id,
        }
    }

    pub(super) fn operation_id(&self) -> &OperationRowId {
        &self.operation_id
    }
}

/// Exact non-secret promotion intent needed to finish one prepared deploy after restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreparedPromotion {
    pub(super) namespace_id: NamespaceRowId,
    pub(super) exact_namespace_document: String,
    pub(super) service_id: ServiceRowId,
    pub(super) service_document: ServiceDocument,
    pub(super) container_id: ContainerId,
    pub(super) container_document: ContainerDocument,
    pub(super) success_result: CorrosionDeployServiceResult,
}

impl PreparedPromotion {
    fn is_consistent_with(&self, identity: &EvidenceIdentity) -> bool {
        let Ok(namespace_document) =
            serde_json::from_str::<NamespaceDocument>(&self.exact_namespace_document)
        else {
            return false;
        };
        namespace_document.cluster_id == self.service_document.cluster_id
            && namespace_document.cluster_id == self.container_document.cluster_id
            && self.service_document.namespace_id == self.namespace_id
            && self.service_document.active_deploy == identity.operation_id
            && self.service_document.operation_id == identity.operation_id
            && self.container_document.namespace_id == self.namespace_id
            && self.container_document.service_id == self.service_id
            && self.container_document.deploy == identity.operation_id
            && self.container_document.machine_id == identity.driver_machine_id
            && self.container_document.cluster_id == self.service_document.cluster_id
            && self.success_result.service_id == self.service_id
            && matches!(
                &self.success_result.result,
                CorrosionDeployServiceResultKind::Completed
            )
    }
}

/// Restart-safe durable progress after exact promotion intent has been recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DurablePromotionProgress {
    PromotionPrepared {
        prepared: PreparedPromotion,
    },
    RowsCommitted {
        prepared: PreparedPromotion,
    },
    ClaimWon {
        prepared: PreparedPromotion,
    },
    ClaimLost {
        prepared: PreparedPromotion,
        winner: ServiceRowId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OperationEvidenceRecovery {
    pub(super) terminal: Option<OperationDocument>,
    pub(super) promotion: Option<DurablePromotionProgress>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum EvidenceFrame {
    Identity {
        #[serde(flatten)]
        identity: EvidenceIdentity,
    },
    Event {
        event: DurableEvidenceEvent,
    },
    PromotionPrepared {
        event: DurableEvidenceEvent,
        prepared: Box<PreparedPromotion>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableEvidenceEvent {
    sequence: OperationEvidenceSequence,
    timestamp: CorrosionTimestamp,
    evidence: OperationEvidence,
}

impl From<OperationEvidenceEvent> for DurableEvidenceEvent {
    fn from(event: OperationEvidenceEvent) -> Self {
        Self {
            sequence: event.sequence,
            timestamp: event.timestamp,
            evidence: event.evidence,
        }
    }
}

impl From<DurableEvidenceEvent> for OperationEvidenceEvent {
    fn from(event: DurableEvidenceEvent) -> Self {
        Self {
            sequence: event.sequence,
            timestamp: event.timestamp,
            evidence: event.evidence,
        }
    }
}

#[derive(Debug)]
struct WriterState {
    file: File,
    next_sequence: u64,
    durable_offset: u64,
    phase: EvidencePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidencePhase {
    Created,
    Executing,
    PromotionPrepared,
    RowsCommitted,
    ClaimDecided,
    Terminal,
}

/// One open, single-writer durable operation evidence file.
#[derive(Clone, Debug)]
pub(super) struct OperationEvidenceLog {
    path: PathBuf,
    identity: EvidenceIdentity,
    max_line_bytes: usize,
    state: Arc<Mutex<WriterState>>,
    durable_events: watch::Sender<u64>,
}

impl OperationEvidenceLog {
    /// Appends one full frame and exposes it to followers only after `sync_data` succeeds.
    pub(super) async fn append(
        &self,
        timestamp: CorrosionTimestamp,
        evidence: OperationEvidence,
    ) -> Result<OperationEvidenceEvent, OperationEvidenceError> {
        let mut state = self.state.lock().await;
        if matches!(&evidence, OperationEvidence::PromotionPrepared) {
            return Err(OperationEvidenceError::PreparedPayloadRequired);
        }
        if let OperationEvidence::Terminal { operation } = &evidence {
            if operation.machine_id != self.identity.driver_machine_id {
                return Err(OperationEvidenceError::TerminalDriverMismatch);
            }
            if !operation_is_terminal(operation) {
                return Err(OperationEvidenceError::TerminalDocumentNonterminal);
            }
        }
        let next_phase = advance_phase(state.phase, &evidence, 0)?;
        let sequence = OperationEvidenceSequence::try_new(state.next_sequence)
            .map_err(|_| OperationEvidenceError::SequenceExhausted)?;
        let event = OperationEvidenceEvent {
            sequence,
            timestamp,
            evidence,
        };
        append_frame(
            &mut state,
            &EvidenceFrame::Event {
                event: event.clone().into(),
            },
            self.max_line_bytes,
        )
        .await?;
        state.phase = next_phase;
        self.durable_events.send_replace(sequence.get());
        Ok(event)
    }

    /// Stores exact promotion intent privately while exposing only typed redaction-safe progress.
    pub(super) async fn append_promotion_prepared(
        &self,
        timestamp: CorrosionTimestamp,
        prepared: PreparedPromotion,
    ) -> Result<OperationEvidenceEvent, OperationEvidenceError> {
        let mut state = self.state.lock().await;
        if !prepared.is_consistent_with(&self.identity) {
            return Err(OperationEvidenceError::InvalidPreparedIntent { line: 0 });
        }
        let next_phase = advance_phase(state.phase, &OperationEvidence::PromotionPrepared, 0)?;
        let sequence = OperationEvidenceSequence::try_new(state.next_sequence)
            .map_err(|_| OperationEvidenceError::SequenceExhausted)?;
        let event = OperationEvidenceEvent {
            sequence,
            timestamp,
            evidence: OperationEvidence::PromotionPrepared,
        };
        append_frame(
            &mut state,
            &EvidenceFrame::PromotionPrepared {
                event: event.clone().into(),
                prepared: Box::new(prepared),
            },
            self.max_line_bytes,
        )
        .await?;
        state.phase = next_phase;
        self.durable_events.send_replace(sequence.get());
        Ok(event)
    }

    pub(super) async fn recovery_evidence(
        &self,
    ) -> Result<OperationEvidenceRecovery, OperationEvidenceError> {
        let state = self.state.lock().await;
        let loaded = load_file(
            &self.path,
            self.max_line_bytes,
            Some(&self.identity),
            TornTailPolicy::Refuse,
        )
        .await?;
        if loaded.durable_offset != state.durable_offset {
            return Err(OperationEvidenceError::DurableOffsetChanged);
        }
        Ok(loaded.recovery())
    }

    pub(super) async fn replay_after(
        &self,
        sequence: u64,
    ) -> Result<Vec<OperationEvidenceEvent>, OperationEvidenceError> {
        let state = self.state.lock().await;
        let loaded = load_file(
            &self.path,
            self.max_line_bytes,
            Some(&self.identity),
            TornTailPolicy::Refuse,
        )
        .await?;
        if loaded.durable_offset != state.durable_offset {
            return Err(OperationEvidenceError::DurableOffsetChanged);
        }
        Ok(loaded
            .events
            .into_iter()
            .filter(|event| event.sequence.get() > sequence)
            .collect())
    }

    /// Subscribes before the final replay read, closing the replay/follow lost-wakeup race.
    pub(super) async fn follow(&self) -> Result<OperationEvidenceFollower, OperationEvidenceError> {
        let changes = self.durable_events.subscribe();
        let pending = self.replay_after(0).await?.into();
        Ok(OperationEvidenceFollower {
            log: self.clone(),
            changes,
            pending,
            last_sequence: 0,
            terminal_delivered: false,
        })
    }
}

/// Replay followed by durable wakeups from one operation log.
pub(super) struct OperationEvidenceFollower {
    log: OperationEvidenceLog,
    changes: watch::Receiver<u64>,
    pending: VecDeque<OperationEvidenceEvent>,
    last_sequence: u64,
    terminal_delivered: bool,
}

impl OperationEvidenceFollower {
    pub(super) async fn next(
        &mut self,
    ) -> Result<Option<OperationEvidenceEvent>, OperationEvidenceError> {
        loop {
            if self.terminal_delivered {
                return Ok(None);
            }
            if let Some(event) = self.pending.pop_front() {
                self.last_sequence = event.sequence.get();
                self.terminal_delivered =
                    matches!(&event.evidence, OperationEvidence::Terminal { .. });
                return Ok(Some(event));
            }
            if self.changes.changed().await.is_err() {
                return Ok(None);
            }
            self.pending = self.log.replay_after(self.last_sequence).await?.into();
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct OperationEvidenceDirectory {
    root: PathBuf,
    max_line_bytes: usize,
}

impl OperationEvidenceDirectory {
    #[must_use]
    pub(super) const fn new(root: PathBuf, max_line_bytes: usize) -> Self {
        Self {
            root,
            max_line_bytes,
        }
    }

    pub(super) fn path_for(&self, operation_id: &OperationRowId) -> PathBuf {
        self.root.join(format!("{operation_id}.jsonl"))
    }

    /// Reads a closed operation without creating another append-capable handle or wake channel.
    pub(super) async fn replay_closed(
        &self,
        identity: &EvidenceIdentity,
    ) -> Result<Vec<OperationEvidenceEvent>, OperationEvidenceError> {
        Ok(load_file(
            &self.path_for(identity.operation_id()),
            self.max_line_bytes,
            Some(identity),
            TornTailPolicy::Refuse,
        )
        .await?
        .events)
    }

    /// Creates the identity and Created event, syncs the file, then fsyncs its parent directory.
    /// Callers may insert the visible operation row only after this method returns.
    pub(super) async fn create(
        &self,
        identity: EvidenceIdentity,
        created_at: CorrosionTimestamp,
    ) -> Result<OperationEvidenceLog, OperationEvidenceError> {
        tokio::fs::create_dir_all(&self.root).await?;
        let path = self.path_for(identity.operation_id());
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|source| OperationEvidenceError::Create {
                path: path.clone(),
                source,
            })?;
        let header = encode_frame(
            &EvidenceFrame::Identity {
                identity: identity.clone(),
            },
            self.max_line_bytes,
        )?;
        let created = OperationEvidenceEvent {
            sequence: OperationEvidenceSequence::try_new(1)
                .map_err(|_| OperationEvidenceError::SequenceExhausted)?,
            timestamp: created_at,
            evidence: OperationEvidence::Created,
        };
        let created_line = encode_frame(
            &EvidenceFrame::Event {
                event: created.clone().into(),
            },
            self.max_line_bytes,
        )?;
        file.write_all(&header).await?;
        file.write_all(&created_line).await?;
        file.flush().await?;
        file.sync_data().await?;
        sync_directory(&self.root).await?;
        let durable_offset = (header.len() + created_line.len()) as u64;
        let (durable_events, _) = watch::channel(created.sequence.get());
        Ok(OperationEvidenceLog {
            path,
            identity,
            max_line_bytes: self.max_line_bytes,
            state: Arc::new(Mutex::new(WriterState {
                file,
                next_sequence: 2,
                durable_offset,
                phase: EvidencePhase::Created,
            })),
            durable_events,
        })
    }

    /// Opens a canonical log, truncating and syncing only an incomplete final frame.
    pub(super) async fn open(
        &self,
        identity: EvidenceIdentity,
    ) -> Result<OperationEvidenceLog, OperationEvidenceError> {
        let path = self.path_for(identity.operation_id());
        let loaded = load_file(
            &path,
            self.max_line_bytes,
            Some(&identity),
            TornTailPolicy::Repair,
        )
        .await?;
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .await?;
        let last_sequence = loaded.events.last().map_or(0, |event| event.sequence.get());
        let next_sequence = last_sequence
            .checked_add(1)
            .ok_or(OperationEvidenceError::SequenceExhausted)?;
        let (durable_events, _) = watch::channel(last_sequence);
        Ok(OperationEvidenceLog {
            path,
            identity,
            max_line_bytes: self.max_line_bytes,
            state: Arc::new(Mutex::new(WriterState {
                file,
                next_sequence,
                durable_offset: loaded.durable_offset,
                phase: loaded.phase,
            })),
            durable_events,
        })
    }

    /// Moves unusable evidence aside without overwriting earlier quarantines and fsyncs the rename.
    pub(super) async fn quarantine(
        &self,
        operation_id: &OperationRowId,
    ) -> Result<Option<PathBuf>, OperationEvidenceError> {
        let source = self.path_for(operation_id);
        if !tokio::fs::try_exists(&source).await? {
            return Ok(None);
        }
        loop {
            let suffix = CorrosionUlid::generate();
            let target = self
                .root
                .join(format!("{operation_id}.quarantine.{suffix}.jsonl"));
            if tokio::fs::try_exists(&target).await? {
                continue;
            }
            match tokio::fs::hard_link(&source, &target).await {
                Ok(()) => {
                    tokio::fs::remove_file(&source).await?;
                    sync_directory(&self.root).await?;
                    return Ok(Some(target));
                }
                Err(source) if source.kind() == ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(OperationEvidenceError::Io(source)),
            }
        }
    }
}

struct LoadedEvidence {
    events: Vec<OperationEvidenceEvent>,
    prepared: Option<PreparedPromotion>,
    durable_offset: u64,
    phase: EvidencePhase,
}

async fn load_file(
    path: &Path,
    max_line_bytes: usize,
    expected_identity: Option<&EvidenceIdentity>,
    torn_tail_policy: TornTailPolicy,
) -> Result<LoadedEvidence, OperationEvidenceError> {
    let metadata = tokio::fs::metadata(path).await.map_err(|source| {
        if source.kind() == ErrorKind::NotFound {
            OperationEvidenceError::Missing
        } else {
            OperationEvidenceError::Io(source)
        }
    })?;
    if metadata.len() > MAX_EVIDENCE_FILE_BYTES {
        return Err(OperationEvidenceError::FileTooLarge {
            limit: MAX_EVIDENCE_FILE_BYTES,
        });
    }
    let bytes = tokio::fs::read(path).await?;
    let has_torn_tail = !bytes.is_empty() && !bytes.ends_with(b"\n");
    if has_torn_tail && torn_tail_policy == TornTailPolicy::Refuse {
        return Err(OperationEvidenceError::TornTail);
    }
    let durable_len = if has_torn_tail {
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1)
    } else {
        bytes.len()
    };
    let Some(durable_bytes) = bytes.get(..durable_len) else {
        return Err(OperationEvidenceError::TornTail);
    };
    let loaded = decode_loaded(durable_bytes, max_line_bytes, expected_identity)?;
    if has_torn_tail {
        let file = OpenOptions::new().write(true).open(path).await?;
        file.set_len(durable_len as u64).await?;
        file.sync_data().await?;
    }
    Ok(loaded)
}

fn decode_loaded(
    bytes: &[u8],
    max_line_bytes: usize,
    expected_identity: Option<&EvidenceIdentity>,
) -> Result<LoadedEvidence, OperationEvidenceError> {
    let mut lines = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty());
    let Some(header) = lines.next() else {
        return Err(OperationEvidenceError::Malformed {
            line: 1,
            detail: "empty file".to_owned(),
        });
    };
    if header.len() > max_line_bytes {
        return Err(OperationEvidenceError::LineTooLarge {
            line: 1,
            limit: max_line_bytes,
        });
    }
    let EvidenceFrame::Identity { identity } = decode_frame(header, 1)? else {
        return Err(OperationEvidenceError::MissingIdentity);
    };
    if identity.format_version != EVIDENCE_FORMAT_VERSION {
        return Err(OperationEvidenceError::UnsupportedVersion {
            version: identity.format_version,
        });
    }
    if expected_identity.is_some_and(|expected| expected != &identity) {
        let Some(expected) = expected_identity.cloned() else {
            return Err(OperationEvidenceError::MissingIdentity);
        };
        return Err(OperationEvidenceError::IdentityMismatch {
            expected,
            found: identity,
        });
    }
    let mut events = Vec::new();
    let mut prepared = None;
    let mut phase = None;
    for (index, line) in lines.enumerate() {
        let line_number = index + 2;
        if line.len() > max_line_bytes {
            return Err(OperationEvidenceError::LineTooLarge {
                line: line_number,
                limit: max_line_bytes,
            });
        }
        let (event, frame_prepared): (OperationEvidenceEvent, Option<PreparedPromotion>) =
            match decode_frame(line, line_number)? {
                EvidenceFrame::Event { event }
                    if matches!(&event.evidence, OperationEvidence::PromotionPrepared) =>
                {
                    return Err(OperationEvidenceError::InvalidPreparedEvent { line: line_number });
                }
                EvidenceFrame::Event { event } => (event.into(), None),
                EvidenceFrame::PromotionPrepared { event, prepared } => {
                    if !matches!(event.evidence, OperationEvidence::PromotionPrepared) {
                        return Err(OperationEvidenceError::InvalidPreparedEvent {
                            line: line_number,
                        });
                    }
                    if !prepared.is_consistent_with(&identity) {
                        return Err(OperationEvidenceError::InvalidPreparedIntent {
                            line: line_number,
                        });
                    }
                    (event.into(), Some(*prepared))
                }
                EvidenceFrame::Identity { .. } => {
                    return Err(OperationEvidenceError::RepeatedIdentity { line: line_number });
                }
            };
        let expected = events.len() as u64 + 1;
        if event.sequence.get() != expected {
            return Err(OperationEvidenceError::NonContiguousSequence {
                expected,
                found: event.sequence.get(),
            });
        }
        if let OperationEvidence::Terminal { operation } = &event.evidence {
            if operation.machine_id != identity.driver_machine_id {
                return Err(OperationEvidenceError::TerminalDriverMismatch);
            }
            if !operation_is_terminal(operation) {
                return Err(OperationEvidenceError::TerminalDocumentNonterminal);
            }
        }
        phase = Some(match phase {
            None if matches!(&event.evidence, OperationEvidence::Created) => EvidencePhase::Created,
            None => return Err(OperationEvidenceError::MissingCreated),
            Some(phase) => advance_phase(phase, &event.evidence, line_number)?,
        });
        if let Some(frame_prepared) = frame_prepared
            && prepared.replace(frame_prepared).is_some()
        {
            return Err(OperationEvidenceError::RepeatedPreparedPromotion { line: line_number });
        }
        events.push(event);
    }
    let Some(phase) = phase else {
        return Err(OperationEvidenceError::MissingCreated);
    };
    Ok(LoadedEvidence {
        phase,
        events,
        prepared,
        durable_offset: bytes.len() as u64,
    })
}

impl LoadedEvidence {
    fn recovery(&self) -> OperationEvidenceRecovery {
        let terminal = self
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.evidence {
                OperationEvidence::Terminal { operation } => Some((**operation).clone()),
                OperationEvidence::Created
                | OperationEvidence::OpClaimWon
                | OperationEvidence::OpClaimLost { .. }
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
                | OperationEvidence::ClaimWon
                | OperationEvidence::ClaimLost { .. }
                | OperationEvidence::Drained
                | OperationEvidence::IncumbentRemoved { .. } => None,
            });
        let promotion = self.prepared.clone().and_then(|prepared| match self.phase {
            EvidencePhase::PromotionPrepared => {
                Some(DurablePromotionProgress::PromotionPrepared { prepared })
            }
            EvidencePhase::RowsCommitted => {
                Some(DurablePromotionProgress::RowsCommitted { prepared })
            }
            EvidencePhase::ClaimDecided => self.events.iter().rev().find_map(|event| match &event
                .evidence
            {
                OperationEvidence::ClaimWon => Some(DurablePromotionProgress::ClaimWon {
                    prepared: prepared.clone(),
                }),
                OperationEvidence::ClaimLost { winner } => {
                    Some(DurablePromotionProgress::ClaimLost {
                        prepared: prepared.clone(),
                        winner: winner.clone(),
                    })
                }
                OperationEvidence::Created
                | OperationEvidence::OpClaimWon
                | OperationEvidence::OpClaimLost { .. }
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
                | OperationEvidence::Drained
                | OperationEvidence::IncumbentRemoved { .. }
                | OperationEvidence::Terminal { .. } => None,
            }),
            EvidencePhase::Created | EvidencePhase::Executing | EvidencePhase::Terminal => None,
        });
        OperationEvidenceRecovery {
            terminal,
            promotion,
        }
    }
}

fn advance_phase(
    phase: EvidencePhase,
    evidence: &OperationEvidence,
    line: usize,
) -> Result<EvidencePhase, OperationEvidenceError> {
    match (phase, evidence) {
        (
            EvidencePhase::Created | EvidencePhase::Executing,
            OperationEvidence::OpClaimWon
            | OperationEvidence::OpClaimLost { .. }
            | OperationEvidence::DebrisSwept { .. }
            | OperationEvidence::PullingImage
            | OperationEvidence::ImageResolved
            | OperationEvidence::ContainerCreated { .. }
            | OperationEvidence::ContainerStarted { .. }
            | OperationEvidence::HealthGateSkipped
            | OperationEvidence::IncumbentStopped { .. }
            | OperationEvidence::IncumbentRestarted { .. },
        ) => Ok(EvidencePhase::Executing),
        (
            EvidencePhase::RowsCommitted | EvidencePhase::ClaimDecided,
            OperationEvidence::Drained | OperationEvidence::IncumbentRemoved { .. },
        ) => Ok(phase),
        (
            EvidencePhase::Created | EvidencePhase::Executing,
            OperationEvidence::PromotionPrepared,
        ) => Ok(EvidencePhase::PromotionPrepared),
        (EvidencePhase::PromotionPrepared, OperationEvidence::RowsCommitted) => {
            Ok(EvidencePhase::RowsCommitted)
        }
        (
            EvidencePhase::RowsCommitted,
            OperationEvidence::ClaimWon | OperationEvidence::ClaimLost { .. },
        ) => Ok(EvidencePhase::ClaimDecided),
        (
            EvidencePhase::Created
            | EvidencePhase::Executing
            | EvidencePhase::PromotionPrepared
            | EvidencePhase::RowsCommitted
            | EvidencePhase::ClaimDecided,
            OperationEvidence::Terminal { .. },
        ) => Ok(EvidencePhase::Terminal),
        (
            EvidencePhase::Created
            | EvidencePhase::Executing
            | EvidencePhase::PromotionPrepared
            | EvidencePhase::RowsCommitted
            | EvidencePhase::ClaimDecided
            | EvidencePhase::Terminal,
            OperationEvidence::Created
            | OperationEvidence::OpClaimWon
            | OperationEvidence::OpClaimLost { .. }
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
            | OperationEvidence::ClaimWon
            | OperationEvidence::ClaimLost { .. }
            | OperationEvidence::Drained
            | OperationEvidence::IncumbentRemoved { .. }
            | OperationEvidence::Terminal { .. },
        ) => Err(OperationEvidenceError::InvalidEventOrder { line }),
    }
}

fn encode_frame(
    frame: &EvidenceFrame,
    max_line_bytes: usize,
) -> Result<Vec<u8>, OperationEvidenceError> {
    let mut line = serde_json::to_vec(frame)
        .map_err(|source| OperationEvidenceError::Encode(source.to_string()))?;
    line.push(b'\n');
    if line.len() > max_line_bytes {
        return Err(OperationEvidenceError::LineTooLarge {
            line: 0,
            limit: max_line_bytes,
        });
    }
    Ok(line)
}

async fn append_frame(
    state: &mut WriterState,
    frame: &EvidenceFrame,
    max_line_bytes: usize,
) -> Result<(), OperationEvidenceError> {
    let line = encode_frame(frame, max_line_bytes)?;
    let next_offset = state.durable_offset.checked_add(line.len() as u64).ok_or(
        OperationEvidenceError::FileTooLarge {
            limit: MAX_EVIDENCE_FILE_BYTES,
        },
    )?;
    if next_offset > MAX_EVIDENCE_FILE_BYTES {
        return Err(OperationEvidenceError::FileTooLarge {
            limit: MAX_EVIDENCE_FILE_BYTES,
        });
    }
    let next_sequence = state
        .next_sequence
        .checked_add(1)
        .ok_or(OperationEvidenceError::SequenceExhausted)?;
    state.file.write_all(&line).await?;
    state.file.flush().await?;
    state.file.sync_data().await?;
    state.durable_offset = next_offset;
    state.next_sequence = next_sequence;
    Ok(())
}

fn operation_is_terminal(operation: &OperationDocument) -> bool {
    match operation.operation() {
        CorrosionOperation::Deploy { state, .. } => {
            matches!(state, CorrosionDeployState::Terminal { .. })
        }
        CorrosionOperation::Build { state, .. }
        | CorrosionOperation::MachineAdd { state, .. }
        | CorrosionOperation::MachineRemove { state, .. }
        | CorrosionOperation::Recovery { state, .. } => matches!(
            state,
            CorrosionOperationState::Succeeded { .. } | CorrosionOperationState::Failed { .. }
        ),
    }
}

fn decode_frame(line: &[u8], line_number: usize) -> Result<EvidenceFrame, OperationEvidenceError> {
    serde_json::from_slice(line).map_err(|source| OperationEvidenceError::Malformed {
        line: line_number,
        detail: source.to_string(),
    })
}

async fn sync_directory(path: &Path) -> Result<(), OperationEvidenceError> {
    let directory = File::open(path).await?;
    directory.sync_all().await?;
    Ok(())
}

#[cfg(test)]
pub(super) fn prepared_promotion_fixture() -> PreparedPromotion {
    let namespace_id = NamespaceRowId::try_new("01J00000000000000000000013").expect("namespace");
    let service_id = ServiceRowId::try_new("01J00000000000000000000014").expect("service");
    let operation_id = OperationRowId::try_new("01J00000000000000000000011").expect("operation");
    let service_document = serde_json::from_value(serde_json::json!({
        "v": 1,
        "cluster_id": "01J00000000000000000000010",
        "written_by": {
            "kind": "peer",
            "peer_id": "01J00000000000000000000015"
        },
        "written_at": "2026-08-05T10:00:00Z",
        "namespace_id": namespace_id,
        "name": "api",
        "image": "ghcr.io/acme/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "env_fingerprints": {},
        "mode": "replicated",
        "replicas": 1,
        "pinned_machines": ["01J00000000000000000000012"],
        "active_deploy": operation_id,
        "previous_image": null,
        "deployed_at": "2026-08-05T10:00:02Z",
        "operation_id": operation_id
    }))
    .expect("service document");
    let container_document = serde_json::from_value(serde_json::json!({
        "v": 1,
        "cluster_id": "01J00000000000000000000010",
        "machine_id": "01J00000000000000000000012",
        "service_id": service_id,
        "namespace_id": namespace_id,
        "ip": "10.210.20.2",
        "deploy": operation_id
    }))
    .expect("container document");
    PreparedPromotion {
        namespace_id,
        exact_namespace_document: serde_json::json!({
            "v": 1,
            "cluster_id": "01J00000000000000000000010",
            "written_by": {
                "kind": "peer",
                "peer_id": "01J00000000000000000000015"
            },
            "written_at": "2026-08-05T10:00:00Z",
            "name": "production"
        })
        .to_string(),
        service_id: service_id.clone(),
        service_document,
        container_id: ContainerId::try_new("docker-container-1").expect("container id"),
        container_document,
        success_result: CorrosionDeployServiceResult::completed(service_id),
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum OperationEvidenceError {
    #[error("operation evidence file is missing")]
    Missing,
    #[error("operation evidence file has an incomplete final frame")]
    TornTail,
    #[error("operation evidence file is missing its identity header")]
    MissingIdentity,
    #[error("operation evidence file is missing its initial Created event")]
    MissingCreated,
    #[error("operation evidence identity uses unsupported version {version}")]
    UnsupportedVersion { version: u32 },
    #[error("operation evidence identity does not match its operation row")]
    IdentityMismatch {
        expected: EvidenceIdentity,
        found: EvidenceIdentity,
    },
    #[error("operation evidence line {line} is malformed: {detail}")]
    Malformed { line: usize, detail: String },
    #[error("operation evidence line {line} exceeds its {limit}-byte bound")]
    LineTooLarge { line: usize, limit: usize },
    #[error("operation evidence file exceeds its {limit}-byte bound")]
    FileTooLarge { limit: u64 },
    #[error("operation evidence repeats an identity header at line {line}")]
    RepeatedIdentity { line: usize },
    #[error("operation evidence repeats promotion intent at line {line}")]
    RepeatedPreparedPromotion { line: usize },
    #[error("operation evidence PromotionPrepared requires exact private intent")]
    PreparedPayloadRequired,
    #[error("operation evidence promotion frame has the wrong public event at line {line}")]
    InvalidPreparedEvent { line: usize },
    #[error("operation evidence promotion intent is internally inconsistent at line {line}")]
    InvalidPreparedIntent { line: usize },
    #[error("operation evidence terminal document is nonterminal")]
    TerminalDocumentNonterminal,
    #[error("operation evidence terminal document belongs to another driver")]
    TerminalDriverMismatch,
    #[error("operation evidence event is invalid for its durable phase at line {line}")]
    InvalidEventOrder { line: usize },
    #[error("operation evidence sequence is not contiguous: expected {expected}, found {found}")]
    NonContiguousSequence { expected: u64, found: u64 },
    #[error("operation evidence sequence space is exhausted")]
    SequenceExhausted,
    #[error("operation evidence changed outside its single writer")]
    DurableOffsetChanged,
    #[error("failed to encode operation evidence: {0}")]
    Encode(String),
    #[error("failed to create operation evidence at {path}: {source}")]
    Create {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use ployz_core::OperationEvidence;
    use ployz_core::corrosion::{
        CorrosionBasicOperation, CorrosionBasicTransition, CorrosionDeployServiceFailure,
        CorrosionDeployServiceResult, CorrosionDocumentVersion, CorrosionTimestamp,
        OperationDocument, OperationInitiator,
    };
    use ployz_core::ids::{ClusterId, MachineRowId, OperationRowId, PeerId, ServiceRowId};
    use tokio::io::AsyncWriteExt;

    use super::{EvidenceIdentity, OperationEvidenceDirectory};

    fn operation_id() -> OperationRowId {
        OperationRowId::try_new("01J00000000000000000000011").expect("operation id")
    }

    fn machine_id() -> MachineRowId {
        MachineRowId::try_new("01J00000000000000000000012").expect("machine id")
    }

    fn at(value: &str) -> CorrosionTimestamp {
        CorrosionTimestamp::try_new(value).expect("timestamp")
    }

    fn created_operation(owner: MachineRowId) -> OperationDocument {
        OperationDocument::basic_created(
            CorrosionDocumentVersion::V1,
            ClusterId::try_new("01J00000000000000000000010").expect("cluster"),
            owner,
            OperationInitiator::Peer {
                peer_id: PeerId::try_new("01J00000000000000000000015").expect("peer"),
            },
            CorrosionBasicOperation::Build {
                service_id: ServiceRowId::try_new("01J00000000000000000000014").expect("service"),
            },
            at("2026-08-05T10:00:00Z"),
        )
    }

    fn terminal_operation(owner: MachineRowId) -> OperationDocument {
        created_operation(owner)
            .transition_basic(CorrosionBasicTransition::Succeeded {
                completed_at: at("2026-08-05T10:00:02Z"),
            })
            .expect("terminal")
    }

    #[tokio::test]
    async fn create_durably_records_identity_and_created_before_returning() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let log = directory
            .create(
                EvidenceIdentity::new(operation_id(), machine_id()),
                at("2026-08-05T10:00:00Z"),
            )
            .await
            .expect("create evidence");

        let replay = log.replay_after(0).await.expect("replay");
        let [created] = replay.as_slice() else {
            panic!("one Created event expected");
        };
        assert_eq!(created.sequence.get(), 1);
        assert_eq!(created.evidence, OperationEvidence::Created);
    }

    #[tokio::test]
    async fn append_assigns_contiguous_sequences_and_follow_does_not_miss_a_wakeup() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let log = directory
            .create(
                EvidenceIdentity::new(operation_id(), machine_id()),
                at("2026-08-05T10:00:00Z"),
            )
            .await
            .expect("create evidence");
        let mut follower = log.follow().await.expect("follow");

        log.append(at("2026-08-05T10:00:01Z"), OperationEvidence::PullingImage)
            .await
            .expect("append");

        let replay = follower.next().await.expect("next").expect("event");
        assert_eq!(replay.sequence.get(), 1);
        let next = follower.next().await.expect("next").expect("event");
        assert_eq!(next.sequence.get(), 2);
        assert_eq!(next.evidence, OperationEvidence::PullingImage);
    }

    #[tokio::test]
    async fn reopen_repairs_only_a_torn_final_frame() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let identity = EvidenceIdentity::new(operation_id(), machine_id());
        drop(
            directory
                .create(identity.clone(), at("2026-08-05T10:00:00Z"))
                .await
                .expect("create evidence"),
        );
        let path = directory.path_for(&operation_id());
        append_bytes(&path, br#"{"record":"event"#).await;

        let log = directory.open(identity).await.expect("repair and open");
        log.append(at("2026-08-05T10:00:02Z"), OperationEvidence::ImageResolved)
            .await
            .expect("append after repair");
        let replay = log.replay_after(0).await.expect("replay");
        let [_, resolved] = replay.as_slice() else {
            panic!("Created and ImageResolved expected");
        };
        assert_eq!(resolved.sequence.get(), 2);
    }

    #[tokio::test]
    async fn malformed_complete_prefix_is_preserved_when_the_tail_is_torn() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let identity = EvidenceIdentity::new(operation_id(), machine_id());
        drop(
            directory
                .create(identity.clone(), at("2026-08-05T10:00:00Z"))
                .await
                .expect("create evidence"),
        );
        let path = directory.path_for(&operation_id());
        let valid = tokio::fs::read(&path).await.expect("valid bytes");
        let newline = valid
            .iter()
            .position(|byte| *byte == b'\n')
            .expect("identity newline");
        let Some(identity_line) = valid.get(..=newline) else {
            panic!("identity prefix");
        };
        let mut malformed = identity_line.to_vec();
        malformed.extend_from_slice(b"{\"broken\":true}\n{\"torn\"");
        tokio::fs::write(&path, &malformed)
            .await
            .expect("write malformed evidence");

        assert!(directory.open(identity).await.is_err());
        assert_eq!(tokio::fs::read(path).await.expect("preserved"), malformed);
    }

    #[tokio::test]
    async fn prepared_promotion_round_trips_exact_private_intent() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let identity = EvidenceIdentity::new(operation_id(), machine_id());
        let log = directory
            .create(identity.clone(), at("2026-08-05T10:00:00Z"))
            .await
            .expect("create evidence");
        let prepared = super::prepared_promotion_fixture();
        log.append_promotion_prepared(at("2026-08-05T10:00:01Z"), prepared.clone())
            .await
            .expect("append prepared");
        drop(log);

        let reopened = directory.open(identity).await.expect("reopen");
        assert_eq!(reopened.identity.driver_machine_id, machine_id());
        assert!(matches!(
            reopened.recovery_evidence().await.expect("recovery").promotion,
            Some(super::DurablePromotionProgress::PromotionPrepared { prepared: found })
                if found == prepared
        ));
    }

    #[tokio::test]
    async fn recovery_preserves_the_exact_durable_promotion_phase() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let log = directory
            .create(
                EvidenceIdentity::new(operation_id(), machine_id()),
                at("2026-08-05T10:00:00Z"),
            )
            .await
            .expect("create evidence");
        let prepared = super::prepared_promotion_fixture();
        log.append_promotion_prepared(at("2026-08-05T10:00:01Z"), prepared.clone())
            .await
            .expect("prepared");
        assert!(matches!(
            log.recovery_evidence()
                .await
                .expect("prepared recovery")
                .promotion,
            Some(super::DurablePromotionProgress::PromotionPrepared { .. })
        ));
        log.append(at("2026-08-05T10:00:02Z"), OperationEvidence::RowsCommitted)
            .await
            .expect("rows committed");
        assert!(matches!(
            log.recovery_evidence()
                .await
                .expect("rows recovery")
                .promotion,
            Some(super::DurablePromotionProgress::RowsCommitted { .. })
        ));
        let winner = ServiceRowId::try_new("01J00000000000000000000016").expect("winner");
        log.append(
            at("2026-08-05T10:00:03Z"),
            OperationEvidence::ClaimLost {
                winner: winner.clone(),
            },
        )
        .await
        .expect("claim lost");

        assert!(matches!(
            log.recovery_evidence().await.expect("recovery").promotion,
            Some(super::DurablePromotionProgress::ClaimLost {
                prepared: found,
                winner: found_winner,
            }) if found == prepared && found_winner == winner
        ));
    }

    #[tokio::test]
    async fn recovery_preserves_durable_claim_win() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let log = directory
            .create(
                EvidenceIdentity::new(operation_id(), machine_id()),
                at("2026-08-05T10:00:00Z"),
            )
            .await
            .expect("create evidence");
        log.append_promotion_prepared(
            at("2026-08-05T10:00:01Z"),
            super::prepared_promotion_fixture(),
        )
        .await
        .expect("prepared");
        log.append(at("2026-08-05T10:00:02Z"), OperationEvidence::RowsCommitted)
            .await
            .expect("rows committed");
        log.append(at("2026-08-05T10:00:03Z"), OperationEvidence::ClaimWon)
            .await
            .expect("claim won");
        assert!(matches!(
            log.recovery_evidence().await.expect("recovery").promotion,
            Some(super::DurablePromotionProgress::ClaimWon { .. })
        ));
    }

    #[tokio::test]
    async fn prepared_promotion_is_validated_before_any_write() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let log = directory
            .create(
                EvidenceIdentity::new(operation_id(), machine_id()),
                at("2026-08-05T10:00:00Z"),
            )
            .await
            .expect("create evidence");
        let before = tokio::fs::read(directory.path_for(&operation_id()))
            .await
            .expect("before");

        let mut malformed_namespace = super::prepared_promotion_fixture();
        malformed_namespace.exact_namespace_document = "not json".to_owned();
        let mut foreign_namespace = super::prepared_promotion_fixture();
        foreign_namespace.exact_namespace_document = foreign_namespace
            .exact_namespace_document
            .replace("01J00000000000000000000010", "01J00000000000000000000016");
        let mut failed_result = super::prepared_promotion_fixture();
        failed_result.success_result = CorrosionDeployServiceResult::failed(
            failed_result.service_id.clone(),
            CorrosionDeployServiceFailure::ContainerStartFailed {
                message: "failed".to_owned(),
            },
        );

        for invalid in [malformed_namespace, foreign_namespace, failed_result] {
            assert!(matches!(
                log.append_promotion_prepared(at("2026-08-05T10:00:01Z"), invalid)
                    .await,
                Err(super::OperationEvidenceError::InvalidPreparedIntent { .. })
            ));
        }
        assert_eq!(
            tokio::fs::read(directory.path_for(&operation_id()))
                .await
                .expect("after"),
            before
        );
    }

    #[tokio::test]
    async fn terminal_append_requires_terminal_state_and_the_identity_driver() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let log = directory
            .create(
                EvidenceIdentity::new(operation_id(), machine_id()),
                at("2026-08-05T10:00:00Z"),
            )
            .await
            .expect("create evidence");
        assert!(matches!(
            log.append(
                at("2026-08-05T10:00:01Z"),
                OperationEvidence::Terminal {
                    operation: Box::new(created_operation(machine_id())),
                },
            )
            .await,
            Err(super::OperationEvidenceError::TerminalDocumentNonterminal)
        ));
        assert!(matches!(
            log.append(
                at("2026-08-05T10:00:01Z"),
                OperationEvidence::Terminal {
                    operation: Box::new(terminal_operation(
                        MachineRowId::try_new("01J00000000000000000000016").expect("other driver"),
                    )),
                },
            )
            .await,
            Err(super::OperationEvidenceError::TerminalDriverMismatch)
        ));
    }

    #[tokio::test]
    async fn follower_returns_terminal_once_then_closes_immediately() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let log = directory
            .create(
                EvidenceIdentity::new(operation_id(), machine_id()),
                at("2026-08-05T10:00:00Z"),
            )
            .await
            .expect("create evidence");
        log.append(
            at("2026-08-05T10:00:02Z"),
            OperationEvidence::Terminal {
                operation: Box::new(terminal_operation(machine_id())),
            },
        )
        .await
        .expect("terminal");
        let mut follower = log.follow().await.expect("follow");
        assert!(follower.next().await.expect("created").is_some());
        assert!(matches!(
            follower.next().await.expect("terminal"),
            Some(event) if matches!(event.evidence, OperationEvidence::Terminal { .. })
        ));
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(50), follower.next())
                .await
                .expect("immediate close")
                .expect("follow result"),
            None
        );
    }

    #[tokio::test]
    async fn file_limit_is_checked_before_bytes_are_written() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let log = directory
            .create(
                EvidenceIdentity::new(operation_id(), machine_id()),
                at("2026-08-05T10:00:00Z"),
            )
            .await
            .expect("create evidence");
        let path = directory.path_for(&operation_id());
        let before = tokio::fs::metadata(&path).await.expect("before").len();
        log.state.lock().await.durable_offset = super::MAX_EVIDENCE_FILE_BYTES;
        assert!(matches!(
            log.append(at("2026-08-05T10:00:01Z"), OperationEvidence::PullingImage)
                .await,
            Err(super::OperationEvidenceError::FileTooLarge { .. })
        ));
        assert_eq!(
            tokio::fs::metadata(path).await.expect("after").len(),
            before
        );
    }

    #[tokio::test]
    async fn quarantine_never_overwrites_and_removes_the_canonical_name() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        drop(
            directory
                .create(
                    EvidenceIdentity::new(operation_id(), machine_id()),
                    at("2026-08-05T10:00:00Z"),
                )
                .await
                .expect("create evidence"),
        );

        let quarantined = directory
            .quarantine(&operation_id())
            .await
            .expect("quarantine")
            .expect("existing file");
        assert!(tokio::fs::try_exists(&quarantined).await.expect("target"));
        assert!(
            !tokio::fs::try_exists(directory.path_for(&operation_id()))
                .await
                .expect("canonical")
        );
    }

    #[tokio::test]
    async fn durable_progress_refuses_out_of_order_finalizer_events() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024);
        let log = directory
            .create(
                EvidenceIdentity::new(operation_id(), machine_id()),
                at("2026-08-05T10:00:00Z"),
            )
            .await
            .expect("create evidence");

        assert!(matches!(
            log.append(at("2026-08-05T10:00:01Z"), OperationEvidence::RowsCommitted,)
                .await,
            Err(super::OperationEvidenceError::InvalidEventOrder { .. })
        ));
    }

    async fn append_bytes(path: &Path, bytes: &[u8]) {
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .await
            .expect("open");
        file.write_all(bytes).await.expect("write");
        file.flush().await.expect("flush");
        file.sync_data().await.expect("sync");
    }
}
