//! Lease, position, markers and switch verbs shared by a Volume run and the Machines it drives.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{CopyRole, DockerVolumeName, MachineName, ManagementAddress, RpcError, RpcErrorCode};

/// Lease number of one Volume run, decided on the Machines (max over their records plus one).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct Lease(u64);

impl Lease {
    #[must_use]
    pub const fn new(lease: u64) -> Self {
        Self(lease)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Lease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Position of a request in its run: step, round and verb within the round, ordered in that order.
///
/// `06-freeze` is `(6, 0, 0)`; round verbs are `(4, round, sub)` with one `sub` per verb.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
pub struct Pos {
    pub seq: u16,
    pub round: u32,
    pub sub: u8,
}

impl Pos {
    /// Position of `02-lease`, the step that writes every member's record.
    pub const ADOPT_LEASE: Self = Self::step(2);

    #[must_use]
    pub const fn step(seq: u16) -> Self {
        Self {
            seq,
            round: 0,
            sub: 0,
        }
    }
}

impl fmt::Display for Pos {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.seq, self.round, self.sub)
    }
}

/// Whether a run is mid-switch on this copy. Adopting a newer lease keeps it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Cycle {
    Open,
    Closed,
}

/// One Machine's admission record for one Volume name.
///
/// Stored as the ZFS user property `ployz:lease.<name>` on the managed root (an uppercase
/// letter in `<name>` written as `:` and its lowercase), value
/// `<lease>:<seq>.<round>.<sub>:<open|closed>`, so it outlives every copy of the Volume.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct LeaseRecord {
    pub lease: Lease,
    pub pos: Pos,
    pub cycle: Cycle,
}

impl fmt::Display for LeaseRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cycle = match self.cycle {
            Cycle::Open => "open",
            Cycle::Closed => "closed",
        };
        write!(formatter, "{}:{}:{cycle}", self.lease, self.pos)
    }
}

/// A stored lease record that does not read as `<lease>:<seq>.<round>.<sub>:<open|closed>`.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid lease record {0:?}; expected <lease>:<seq>.<round>.<sub>:<open|closed>")]
pub struct LeaseRecordParseError(pub String);

impl FromStr for LeaseRecord {
    type Err = LeaseRecordParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || LeaseRecordParseError(value.to_owned());
        let mut fields = value.split(':');
        let (Some(lease), Some(pos), Some(cycle), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(invalid());
        };
        let mut parts = pos.split('.');
        let (Some(seq), Some(round), Some(sub), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid());
        };
        Ok(Self {
            lease: Lease(lease.parse().map_err(|_| invalid())?),
            pos: Pos {
                seq: seq.parse().map_err(|_| invalid())?,
                round: round.parse().map_err(|_| invalid())?,
                sub: sub.parse().map_err(|_| invalid())?,
            },
            cycle: match cycle {
                "open" => Cycle::Open,
                "closed" => Cycle::Closed,
                _ => return Err(invalid()),
            },
        })
    }
}

/// What every switch request carries. The Machine compares `not_after` with its own clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Switch {
    pub lease: Lease,
    pub pos: Pos,
    /// Step deadline: step start plus the request timeout, as Unix seconds.
    pub not_after_unix_seconds: i64,
}

/// What the fence rule decided for a request, under the dataset lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FenceDecision {
    /// Same lease, later position: run the effect and record the position.
    Admit,
    /// Same lease and position: answer from the marker, or finish an incomplete effect.
    Replay,
    /// Newer lease, or no record yet: write the record, then run the effect.
    Adopt,
    RefuseStaleLease,
    RefuseStaleStep,
    RefuseExpired,
}

/// ZFS `guid` of a snapshot; stable across send and receive.
///
/// A decimal string on the wire: guids span all of u64, and a JSON number above 2^53 loses
/// digits in JavaScript.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct SnapshotGuid(u64);

impl SnapshotGuid {
    #[must_use]
    pub const fn new(guid: u64) -> Self {
        Self(guid)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SnapshotGuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<String> for SnapshotGuid {
    type Error = std::num::ParseIntError;

    fn try_from(guid: String) -> Result<Self, Self::Error> {
        guid.parse().map(Self)
    }
}

impl From<SnapshotGuid> for String {
    fn from(guid: SnapshotGuid) -> Self {
        guid.to_string()
    }
}

/// Name of a run's snapshot without the `@`: `w-<lease>-<round>` taken warm, `f-<lease>` taken final.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
pub struct SnapshotName(String);

impl SnapshotName {
    #[must_use]
    pub fn warm(lease: Lease, round: u32) -> Self {
        Self(format!("w-{lease}-{round}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A snapshot name that is not `w-<lease>-<round>` or `f-<lease>`.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid run snapshot name {0:?}; expected w-<lease>-<round> or f-<lease>")]
pub struct SnapshotNameParseError(pub String);

impl FromStr for SnapshotName {
    type Err = SnapshotNameParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || SnapshotNameParseError(value.to_owned());
        let valid = match value.split_once('-') {
            Some(("w", rest)) => rest
                .split_once('-')
                .is_some_and(|(lease, index)| is_number(lease) && is_number(index)),
            Some(("f", lease)) => is_number(lease),
            _ => false,
        };
        valid.then(|| Self(value.to_owned())).ok_or_else(invalid)
    }
}

fn is_number(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

impl TryFrom<String> for SnapshotName {
    type Error = SnapshotNameParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<SnapshotName> for String {
    fn from(name: SnapshotName) -> Self {
        name.0
    }
}

impl fmt::Display for SnapshotName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One snapshot of a copy, by name, GUID and creation time.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Snapshot {
    pub name: SnapshotName,
    pub guid: SnapshotGuid,
    pub created_unix_seconds: i64,
}

/// `ployz:writer` on a Volume root: the phase of the copy that holds the writes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum WriterMarker {
    Idle,
    Stopping,
    Frozen { guid: SnapshotGuid },
    Thawing,
    Handed { guid: SnapshotGuid },
}

impl fmt::Display for WriterMarker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => formatter.write_str("idle"),
            Self::Stopping => formatter.write_str("stopping"),
            Self::Frozen { guid } => write!(formatter, "frozen:{guid}"),
            Self::Thawing => formatter.write_str("thawing"),
            Self::Handed { guid } => write!(formatter, "handed:{guid}"),
        }
    }
}

/// `ployz:mirror` on a slot: the phase of a read-only copy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum MirrorMarker {
    Idle,
    Final { guid: SnapshotGuid },
    HandedIn { guid: SnapshotGuid },
    Promoting,
}

impl fmt::Display for MirrorMarker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => formatter.write_str("idle"),
            Self::Final { guid } => write!(formatter, "final:{guid}"),
            Self::HandedIn { guid } => write!(formatter, "handed_in:{guid}"),
            Self::Promoting => formatter.write_str("promoting"),
        }
    }
}

/// A stored marker that is not one of the known phases.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid {property} marker {value:?}")]
pub struct MarkerParseError {
    pub property: &'static str,
    pub value: String,
}

fn marker_parts(value: &str) -> (&str, Option<&str>) {
    value
        .split_once(':')
        .map_or((value, None), |(phase, guid)| (phase, Some(guid)))
}

fn parse_guid(guid: Option<&str>) -> Option<SnapshotGuid> {
    guid.and_then(|guid| guid.parse().ok()).map(SnapshotGuid)
}

impl FromStr for WriterMarker {
    type Err = MarkerParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || MarkerParseError {
            property: "ployz:writer",
            value: value.to_owned(),
        };
        Ok(match marker_parts(value) {
            ("idle", None) => Self::Idle,
            ("stopping", None) => Self::Stopping,
            ("thawing", None) => Self::Thawing,
            ("frozen", guid) => Self::Frozen {
                guid: parse_guid(guid).ok_or_else(invalid)?,
            },
            ("handed", guid) => Self::Handed {
                guid: parse_guid(guid).ok_or_else(invalid)?,
            },
            _ => return Err(invalid()),
        })
    }
}

impl FromStr for MirrorMarker {
    type Err = MarkerParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || MarkerParseError {
            property: "ployz:mirror",
            value: value.to_owned(),
        };
        Ok(match marker_parts(value) {
            ("idle", None) => Self::Idle,
            ("promoting", None) => Self::Promoting,
            ("final", guid) => Self::Final {
                guid: parse_guid(guid).ok_or_else(invalid)?,
            },
            ("handed_in", guid) => Self::HandedIn {
                guid: parse_guid(guid).ok_or_else(invalid)?,
            },
            _ => return Err(invalid()),
        })
    }
}

/// The copy of a Volume this Machine holds, by its place in the dataset layout.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VolumeCopy {
    /// `<pool>/ployz/<name>`: the writer's dataset, mounted for Containers.
    Root {
        writer: WriterMarker,
        readonly: bool,
        newest: Option<Snapshot>,
    },
    /// `<pool>/ployz-mirror/<name>`: a read-only copy Docker never sees. `newest` is of
    /// its `fs` child; `resume_token` is set while a receive into `fs` is interrupted.
    Slot {
        mirror: MirrorMarker,
        readonly: bool,
        newest: Option<Snapshot>,
        #[serde(default)]
        resume_token: Option<String>,
    },
}

/// Live answer to `InspectVolumeCopy`: what this Machine holds and what it last admitted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeCopyView {
    pub copy: Option<VolumeCopy>,
    pub lease: Option<LeaseRecord>,
}

/// Ask a Machine what it holds for one Volume name. Carries no lease.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct InspectVolumeCopyRequest {
    pub name: DockerVolumeName,
}

/// `02-lease`: record `<lease>:2.0.0` on this Machine, keeping an open cycle open.
///
/// Carries no position: the step is always [`Pos::ADOPT_LEASE`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct AdoptLeaseRequest {
    pub lease: Lease,
    pub not_after_unix_seconds: i64,
    pub name: DockerVolumeName,
}

impl AdoptLeaseRequest {
    /// The request the fence admits, at [`Pos::ADOPT_LEASE`].
    #[must_use]
    pub const fn switch(&self) -> Switch {
        Switch {
            lease: self.lease,
            pos: Pos::ADOPT_LEASE,
            not_after_unix_seconds: self.not_after_unix_seconds,
        }
    }
}

/// `03-declare`: create the slot parent for a mirror bounded by `refquota_bytes`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DeclareMirrorRequest {
    pub switch: Switch,
    pub name: DockerVolumeName,
    pub refquota_bytes: u64,
}

/// A leased verb that needs only the Volume name: BeginRound, Prune, Destroy, Forget.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MirrorRequest {
    pub switch: Switch,
    pub name: DockerVolumeName,
}

/// A source switch that stops or restarts the Container holding this Volume.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SourceContainerRequest {
    pub switch: Switch,
    pub name: DockerVolumeName,
    pub container_id: crate::ContainerId,
}

/// Irreversibly hand the frozen source snapshot to the target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct HandOverRequest {
    pub switch: Switch,
    pub name: DockerVolumeName,
    pub guid: SnapshotGuid,
}

/// A target verb that leaves Docker holding the Volume as `resolved_spec` declares it:
/// Promote, Restore, and StartHandedContainer, which also creates the Service Container
/// under the creation key `handoff-<guid>` and starts it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ServiceVolumeRequest {
    pub switch: Switch,
    pub name: DockerVolumeName,
    pub namespace: crate::Namespace,
    pub resolved_spec: crate::ResolvedServiceSpec,
}

/// Commit on the writer: drop run snapshots older than the mirror's newest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct CommitRequest {
    pub switch: Switch,
    pub name: DockerVolumeName,
    pub mirror_newest: SnapshotGuid,
}

/// Warm on the writer: take `w-<lease>-<round>` unless the newest warm snapshot already
/// captures every write.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct WarmRequest {
    pub switch: Switch,
    pub name: DockerVolumeName,
}

/// StartReceive on the mirror: pull `target` from the writer Machine at `from`, as a full
/// stream, an increment over `base`, or a resume of an interrupted stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StartReceiveRequest {
    pub switch: Switch,
    pub name: DockerVolumeName,
    pub from: ManagementAddress,
    pub base: Option<SnapshotGuid>,
    pub target: SnapshotName,
    pub resume_token: Option<String>,
}

/// Ask a mirror Machine how the receive of `round` is going. Carries no lease.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct InspectReceiveRequest {
    pub name: DockerVolumeName,
    pub round: u32,
}

/// What the last admitted receive left on the slot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReceiveStatus {
    /// No receive of the asked round was admitted on this slot.
    Idle,
    Running {
        target: SnapshotName,
    },
    Done {
        newest: Snapshot,
    },
    /// The stream broke; StartReceive again with `token` at the same position.
    Resumable {
        target: SnapshotName,
        token: String,
    },
    Failed {
        target: SnapshotName,
        reason: String,
    },
}

/// Answer to `InspectReceive`: the round the slot last admitted and its state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ReceiveView {
    pub round: Option<u32>,
    pub status: ReceiveStatus,
}

/// Which stream a mirror asks its writer Machine for over the mesh.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SendSource {
    Full {
        target: SnapshotName,
    },
    Incremental {
        base: SnapshotGuid,
        target: SnapshotName,
    },
    Resume {
        token: String,
        target: SnapshotName,
    },
}

/// `GET /volume-send/<name>?<query>` on the writer Machine's management address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendStream {
    pub name: DockerVolumeName,
    pub source: SendSource,
}

impl SendStream {
    pub const PATH_PREFIX: &'static str = "/volume-send/";

    #[must_use]
    pub fn path_and_query(&self) -> String {
        let query = match &self.source {
            SendSource::Full { target } => format!("target={target}"),
            SendSource::Incremental { base, target } => format!("target={target}&base={base}"),
            SendSource::Resume { token, target } => format!("target={target}&token={token}"),
        };
        format!("{}{}?{query}", Self::PATH_PREFIX, self.name)
    }

    /// Reads a request path and query; `None` when it names no stream this Machine can send.
    ///
    /// Values are a Volume name, a snapshot name, a GUID or a ZFS resume token, none of
    /// which contain `&` or `=`, so the query needs no decoding.
    #[must_use]
    pub fn parse(path: &str, query: Option<&str>) -> Option<Self> {
        let name = DockerVolumeName::parse(path.strip_prefix(Self::PATH_PREFIX)?).ok()?;
        let mut target = None;
        let mut base = None;
        let mut token = None;
        for pair in query?.split('&') {
            match pair.split_once('=')? {
                ("target", value) => target = Some(value.parse::<SnapshotName>().ok()?),
                ("base", value) => base = Some(SnapshotGuid(value.parse().ok()?)),
                ("token", value) if !value.is_empty() => token = Some(value.to_owned()),
                _ => return None,
            }
        }
        let source = match (target, base, token) {
            (Some(target), None, None) => SendSource::Full { target },
            (Some(target), Some(base), None) => SendSource::Incremental { base, target },
            (Some(target), None, Some(token)) => SendSource::Resume { token, target },
            _ => return None,
        };
        Some(Self { name, source })
    }
}

/// The managed root under a Pool: `<pool>/ployz/<name>` holds a Volume's writer.
pub const DATASET_ROOT: &str = "ployz";
/// The mirror root under a Pool: `<pool>/ployz-mirror/<name>` holds a Volume's slot.
pub const MIRROR_ROOT: &str = "ployz-mirror";

/// Every admitted switch verb answers the same shape, so a replay answers as the original did.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SwitchReply {
    pub decision: FenceDecision,
    /// The record after admission.
    pub lease: LeaseRecord,
    pub copy: Option<VolumeCopy>,
}

/// Why a Machine refused a switch request, in `RpcError.details`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SwitchError {
    StaleLease,
    StaleStep,
    /// `not_after` passed on the Machine's clock; `skew_seconds` is how long ago.
    Expired {
        skew_seconds: i64,
    },
    Precondition,
    Busy,
    VolumeSwitching,
    NoWriter,
    NoCapacity,
}

impl SwitchError {
    /// The switch reason carried by an RPC error, if it is one of the known names.
    #[must_use]
    pub fn from_details(details: &serde_json::Value) -> Option<Self> {
        Self::deserialize(details).ok()
    }

    /// An RPC error that carries this reason in `details`.
    #[must_use]
    pub fn rpc_error(self, message: impl Into<String>) -> RpcError {
        RpcError {
            code: self.rpc_code(),
            message: message.into(),
            details: serde_json::to_value(self).expect("a switch reason is JSON serializable"),
            cause: Vec::new(),
        }
    }

    const fn rpc_code(self) -> RpcErrorCode {
        match self {
            Self::StaleLease
            | Self::StaleStep
            | Self::Expired { .. }
            | Self::Precondition
            | Self::Busy
            | Self::VolumeSwitching
            | Self::NoWriter => RpcErrorCode::Conflict,
            Self::NoCapacity => RpcErrorCode::Unavailable,
        }
    }
}

/// A copy of a Volume on another Machine, as this Machine's replicated observation names it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownCopy {
    pub machine: MachineName,
    pub role: CopyRole,
}

/// Whether a plain Deploy may mount a Volume on this Machine. `held` is what the Machine
/// holds and last admitted; `elsewhere` is every copy its replicated observation knows on
/// other Machines.
///
/// A Machine holding nothing mounts only a Volume no Machine holds and it never admitted a
/// run for: a fresh Volume. Its own root mounts only while idle, closed and writable.
///
/// # Errors
/// `NoWriter` when the data lives elsewhere or was departed; `VolumeSwitching` when a run
/// holds the root.
pub fn admit_plain_mount(
    held: &VolumeCopyView,
    elsewhere: &[KnownCopy],
) -> Result<(), SwitchError> {
    match &held.copy {
        None if elsewhere.is_empty() && held.lease.is_none() => Ok(()),
        None | Some(VolumeCopy::Slot { .. }) => Err(SwitchError::NoWriter),
        Some(VolumeCopy::Root {
            writer: WriterMarker::Idle,
            readonly: false,
            ..
        }) if held
            .lease
            .is_none_or(|record| record.cycle == Cycle::Closed) =>
        {
            Ok(())
        }
        Some(VolumeCopy::Root { .. }) => Err(SwitchError::VolumeSwitching),
    }
}

/// The refusal a plain Deploy of Volume `name` reads when this Machine cannot mount its
/// writer: the Machine that holds the writer, or else every copy and the restore that would
/// make one a writer. `name` is the Volume as its owner named it, not its Docker name.
#[must_use]
pub fn no_writer_message(name: &str, copies: &[KnownCopy]) -> String {
    if let Some(writer) = copies.iter().find(|copy| copy.role == CopyRole::Writer) {
        return format!(
            "Volume {name}'s writer is on {}, so this Machine cannot mount it",
            writer.machine
        );
    }
    let Some(first) = copies.first() else {
        return format!(
            "Volume {name} has no writer: no Machine holds a copy and this Machine recorded a run for it; restore it from a backup or remove it before deploying"
        );
    };
    let listed = copies
        .iter()
        .map(|copy| {
            let role = if copy.role == CopyRole::Switching {
                "switching"
            } else {
                "copy"
            };
            format!("{} ({role})", copy.machine)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Volume {name} has no writer; it is held as {listed}. Make one the writer: ployz volume restore {name} --from {}",
        first.machine
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(machine: &str) -> MachineName {
        MachineName::parse(machine).unwrap()
    }

    #[test]
    fn plain_mount_admission_table() {
        let record = |cycle| LeaseRecord {
            lease: Lease(3),
            pos: Pos::ADOPT_LEASE,
            cycle,
        };
        let root = |writer, readonly| {
            Some(VolumeCopy::Root {
                writer,
                readonly,
                newest: None,
            })
        };
        let slot = Some(VolumeCopy::Slot {
            mirror: MirrorMarker::Idle,
            readonly: true,
            newest: None,
            resume_token: None,
        });
        let known = |role| {
            vec![KnownCopy {
                machine: name("fsn-2"),
                role,
            }]
        };
        let rows = [
            (None, None, vec![], Ok(())),
            (
                None,
                None,
                known(CopyRole::Slot),
                Err(SwitchError::NoWriter),
            ),
            (
                None,
                None,
                known(CopyRole::Writer),
                Err(SwitchError::NoWriter),
            ),
            (
                None,
                Some(record(Cycle::Closed)),
                vec![],
                Err(SwitchError::NoWriter),
            ),
            (slot.clone(), None, vec![], Err(SwitchError::NoWriter)),
            (root(WriterMarker::Idle, false), None, vec![], Ok(())),
            (
                root(WriterMarker::Idle, false),
                Some(record(Cycle::Closed)),
                known(CopyRole::Slot),
                Ok(()),
            ),
            (
                root(WriterMarker::Idle, false),
                Some(record(Cycle::Open)),
                vec![],
                Err(SwitchError::VolumeSwitching),
            ),
            (
                root(WriterMarker::Idle, true),
                None,
                vec![],
                Err(SwitchError::VolumeSwitching),
            ),
            (
                root(WriterMarker::Stopping, false),
                None,
                vec![],
                Err(SwitchError::VolumeSwitching),
            ),
            (
                root(
                    WriterMarker::Handed {
                        guid: SnapshotGuid(1),
                    },
                    true,
                ),
                Some(record(Cycle::Closed)),
                known(CopyRole::Slot),
                Err(SwitchError::VolumeSwitching),
            ),
        ];
        for (copy, lease, elsewhere, expected) in rows {
            let held = VolumeCopyView { copy, lease };
            assert_eq!(
                admit_plain_mount(&held, &elsewhere),
                expected,
                "held {held:?}, elsewhere {elsewhere:?}"
            );
        }
    }

    #[test]
    fn no_writer_message_names_every_copy_and_the_restore_line() {
        let volume = "data";
        let copies = [
            KnownCopy {
                machine: name("fsn-2"),
                role: CopyRole::Slot,
            },
            KnownCopy {
                machine: name("hel-1"),
                role: CopyRole::Switching,
            },
        ];
        let message = no_writer_message(volume, &copies);
        assert_eq!(
            message,
            "Volume data has no writer; it is held as fsn-2 (copy), hel-1 (switching). Make one the writer: ployz volume restore data --from fsn-2"
        );
        assert!(no_writer_message(volume, &[]).contains("no Machine holds a copy"));
    }

    #[test]
    fn no_writer_message_names_a_writer_elsewhere_without_a_restore_line() {
        let volume = "data";
        let copies = [
            KnownCopy {
                machine: name("fsn-2"),
                role: CopyRole::Slot,
            },
            KnownCopy {
                machine: name("hel-1"),
                role: CopyRole::Writer,
            },
        ];
        assert_eq!(
            no_writer_message(volume, &copies),
            "Volume data's writer is on hel-1, so this Machine cannot mount it"
        );
    }

    #[test]
    fn lease_record_round_trips_through_its_property_value() {
        let record = LeaseRecord {
            lease: Lease(7),
            pos: Pos {
                seq: 4,
                round: 2,
                sub: 5,
            },
            cycle: Cycle::Open,
        };
        assert_eq!(record.to_string(), "7:4.2.5:open");
        assert_eq!("7:4.2.5:open".parse::<LeaseRecord>().unwrap(), record);
        assert_eq!(
            "7:2.0.0:closed".parse::<LeaseRecord>().unwrap().cycle,
            Cycle::Closed
        );
        for invalid in [
            "",
            "7",
            "7:2.0.0",
            "7:2.0:closed",
            "7:2.0.0:ajar",
            "x:2.0.0:open",
        ] {
            assert_eq!(
                invalid.parse::<LeaseRecord>(),
                Err(LeaseRecordParseError(invalid.to_owned()))
            );
        }
    }

    #[test]
    fn positions_order_by_step_then_round_then_sub() {
        let later_round = Pos {
            seq: 4,
            round: 2,
            sub: 1,
        };
        let earlier_round_later_sub = Pos {
            seq: 4,
            round: 1,
            sub: 5,
        };
        assert!(earlier_round_later_sub < later_round);
        assert!(Pos::step(5) > later_round);
        assert!(Pos::ADOPT_LEASE < Pos::step(3));
    }

    #[test]
    fn markers_round_trip_through_their_property_values() {
        for (text, marker) in [
            ("idle", WriterMarker::Idle),
            ("stopping", WriterMarker::Stopping),
            (
                "frozen:42",
                WriterMarker::Frozen {
                    guid: SnapshotGuid(42),
                },
            ),
            ("thawing", WriterMarker::Thawing),
            (
                "handed:42",
                WriterMarker::Handed {
                    guid: SnapshotGuid(42),
                },
            ),
        ] {
            assert_eq!(marker.to_string(), text);
            assert_eq!(text.parse::<WriterMarker>().unwrap(), marker);
        }
        for (text, marker) in [
            ("idle", MirrorMarker::Idle),
            (
                "final:9",
                MirrorMarker::Final {
                    guid: SnapshotGuid(9),
                },
            ),
            (
                "handed_in:9",
                MirrorMarker::HandedIn {
                    guid: SnapshotGuid(9),
                },
            ),
            ("promoting", MirrorMarker::Promoting),
        ] {
            assert_eq!(marker.to_string(), text);
            assert_eq!(text.parse::<MirrorMarker>().unwrap(), marker);
        }
        for invalid in ["", "frozen", "frozen:x", "idle:1", "handed", "-"] {
            assert!(invalid.parse::<WriterMarker>().is_err(), "{invalid:?}");
        }
        assert!("final".parse::<MirrorMarker>().is_err());
        assert!("thawing".parse::<MirrorMarker>().is_err());
    }

    fn through_javascript(value: serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match value {
            Value::Number(number) => Value::from(number.as_f64().unwrap()),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(through_javascript).collect())
            }
            Value::Object(fields) => Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, through_javascript(value)))
                    .collect(),
            ),
            other @ (Value::Null | Value::Bool(_) | Value::String(_)) => other,
        }
    }

    #[test]
    fn a_guid_above_two_to_the_53_survives_javascript() {
        let guid = SnapshotGuid(5_695_289_101_028_938_467);
        let json = serde_json::to_string(&guid).unwrap();
        let js = through_javascript(serde_json::from_str(&json).unwrap());
        let back: SnapshotGuid = serde_json::from_value(js).unwrap();
        assert_eq!(back, guid);
    }

    #[test]
    fn switch_errors_carry_their_reason_in_details() {
        let error = SwitchError::Expired { skew_seconds: 61 }.rpc_error("too late");
        assert_eq!(error.code, RpcErrorCode::Conflict);
        assert_eq!(
            error.details,
            serde_json::json!({ "reason": "expired", "skew_seconds": 61 })
        );
        assert_eq!(
            SwitchError::from_details(&error.details),
            Some(SwitchError::Expired { skew_seconds: 61 })
        );
        assert_eq!(
            SwitchError::from_details(&SwitchError::StaleLease.rpc_error("").details),
            Some(SwitchError::StaleLease)
        );
        assert_eq!(
            SwitchError::from_details(&serde_json::json!({ "reason": "unknown" })),
            None
        );
    }
}
