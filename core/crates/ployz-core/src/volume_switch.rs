//! Snapshots, markers and copies shared by a Volume run and the Machines it drives.

use std::{fmt, str::FromStr, time::Duration};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{CopyRole, DockerVolumeName, MachineName, RpcError, RpcErrorCode, WriterEpoch};

/// How long a Machine waits on one call to its volume plugin, such as the call that
/// mounts a handed-over Volume and starts its Container.
pub const VOLUME_PLUGIN_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// What a storage departure changed. The daemon forgets each `demoted` name in Docker.
/// A `stuck` Volume's copy would not stop, and a `failed` Volume's demote went wrong;
/// either way the departure fails naming it, and a retry judges it again.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StorageDeparture {
    pub demoted: Vec<DockerVolumeName>,
    pub stuck: Vec<DockerVolumeName>,
    pub failed: Vec<DepartureFailure>,
}

/// A Volume a storage departure could not demote, and why.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DepartureFailure {
    pub name: DockerVolumeName,
    pub reason: String,
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

/// ZFS `guid` of a slot parent dataset. A slot is created fresh and destroyed whole, so the
/// guid names one incarnation of the slot: a slot pulled again after a Forget gets a new one
/// even when it lands the same snapshot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct SlotId(u64);

impl SlotId {
    #[must_use]
    pub const fn new(guid: u64) -> Self {
        Self(guid)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SlotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<String> for SlotId {
    type Error = std::num::ParseIntError;

    fn try_from(guid: String) -> Result<Self, Self::Error> {
        guid.parse().map(Self)
    }
}

impl From<SlotId> for String {
    fn from(guid: SlotId) -> Self {
        guid.to_string()
    }
}

/// Name of a copy's snapshot without the `@`: `w-<run>-<round>` taken warm, `f-<run>` taken
/// final, or `dep-<unix seconds>` taken when the Machine departed, the restore point of a
/// copy no run ever snapshotted.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
pub struct SnapshotName(String);

impl SnapshotName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A snapshot name that is not `w-<run>-<round>`, `f-<run>` or `dep-<unix seconds>`.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid snapshot name {0:?}; expected w-<run>-<round>, f-<run> or dep-<unix seconds>")]
pub struct SnapshotNameParseError(pub String);

impl FromStr for SnapshotName {
    type Err = SnapshotNameParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || SnapshotNameParseError(value.to_owned());
        let valid = match value.split_once('-') {
            Some(("w", rest)) => rest
                .split_once('-')
                .is_some_and(|(run, index)| is_number(run) && is_number(index)),
            Some(("f", run)) => is_number(run),
            Some(("dep", seconds)) => is_number(seconds),
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

/// The copy of a Volume this Machine holds, by its place in the dataset layout.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VolumeCopy {
    /// `<pool>/ployz/<name>`: the writer's dataset, mounted for Containers.
    Root {
        readonly: bool,
        newest: Option<Snapshot>,
        /// The run of writes this root belongs to; 0 until `SetVolumeCopy` numbers it.
        #[serde(default)]
        epoch: WriterEpoch,
        /// A root `Volume.Seed` landed that Docker has not adopted yet: it carries both
        /// `ployz:seed` and `ployz:promote`. Older daemons never report it.
        #[serde(default)]
        seed: bool,
    },
    /// `<pool>/ployz-mirror/<name>`: a read-only copy Docker never sees. `newest` is of
    /// its `fs` child; `resume_token` is set while a receive into `fs` is interrupted.
    Slot {
        /// The slot parent's guid, which changes when the slot is destroyed and created again.
        slot: SlotId,
        readonly: bool,
        newest: Option<Snapshot>,
        #[serde(default)]
        resume_token: Option<String>,
        /// The `SetVolumeCopy` pull in flight, from its start until its prune is done.
        #[serde(default)]
        pull: Option<Pull>,
    },
}

/// A `SetVolumeCopy` pull into a slot: which snapshot it fetches and, once the receive
/// has stopped without landing it, why.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Pull {
    pub target: SnapshotName,
    #[serde(default)]
    pub failure: Option<String>,
}

/// Live answer to `InspectVolumeCopy`: what this Machine holds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeCopyView {
    pub copy: Option<VolumeCopy>,
}

/// Ask a Machine what it holds for one Volume name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct InspectVolumeCopyRequest {
    pub name: DockerVolumeName,
}

/// SeedVolume on the writer: a new Volume `into` holding the restore point `guid` of
/// `name`, received locally from the writer's own snapshot. Resent until it converges: it
/// answers the copy of `into`, a slot while it copies and a root once it lands.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SeedVolumeRequest {
    pub name: DockerVolumeName,
    pub into: DockerVolumeName,
    pub guid: SnapshotGuid,
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

/// Why a Machine refused a switch request, in `RpcError.details`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SwitchError {
    Precondition,
    Busy,
    VolumeSwitching,
    NoWriter,
    /// The restore point a seed asked for is no longer on the writer.
    RestorePointGone,
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
            Self::Precondition
            | Self::Busy
            | Self::VolumeSwitching
            | Self::NoWriter
            | Self::RestorePointGone => RpcErrorCode::Conflict,
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

/// Whether a plain Deploy may mount a Volume on this Machine. `held` is the copy this
/// Machine holds; `elsewhere` is every copy its replicated observation knows on other
/// Machines.
///
/// A Machine holding nothing mounts a fresh Volume while no other Machine holds a copy.
/// Its own root mounts while writable.
///
/// # Errors
/// `NoWriter` when the data lives elsewhere or only in this Machine's slot;
/// `VolumeSwitching` when a run holds the root read-only.
pub fn admit_plain_mount(
    held: Option<&VolumeCopy>,
    elsewhere: &[KnownCopy],
) -> Result<(), SwitchError> {
    match held {
        None if elsewhere.is_empty() => Ok(()),
        None | Some(VolumeCopy::Slot { .. }) => Err(SwitchError::NoWriter),
        Some(VolumeCopy::Root {
            readonly: false, ..
        }) => Ok(()),
        Some(VolumeCopy::Root { .. }) => Err(SwitchError::VolumeSwitching),
    }
}

/// The refusal a plain Deploy of Volume `name` reads when this Server cannot mount its
/// writer: the Server that holds the writer, the Move a run holds it in, or else how
/// Restore makes a copy the writer. `name` is the Volume as its owner named it, not its
/// Docker name.
#[must_use]
pub fn no_writer_message(name: &str, copies: &[KnownCopy]) -> String {
    if let Some(writer) = copies.iter().find(|copy| copy.role == CopyRole::Writer) {
        return format!(
            "Volume {name}'s writer is on {}, so this Server cannot mount it",
            writer.machine
        );
    }
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
    if copies.iter().any(|copy| copy.role == CopyRole::Switching) {
        return format!(
            "Volume {name} is mid-Move; it is held as {listed}; ployz volume release {name} stops it"
        );
    }
    match copies {
        [] => format!("Volume {name} has no writer and no Server holds a copy of it"),
        [only] => format!(
            "Volume {name} has no writer; {} holds its only copy. Make it the writer: ployz volume restore {name} --from {}",
            only.machine, only.machine
        ),
        // Copies carry no age, so the operator picks which one to keep.
        _ => {
            let removals = copies
                .iter()
                .map(|copy| {
                    format!(
                        "ployz volume mirror rm {name}-{} --confirm {name}",
                        copy.machine
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Volume {name} has no writer; it is held as {listed}. Restore needs a single copy: keep the newest, remove each other one ({removals}), then run ployz volume restore {name} --from <server you kept>"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(machine: &str) -> MachineName {
        MachineName::parse(machine).unwrap()
    }

    #[test]
    fn plain_mount_admission_table() {
        let root = |readonly| VolumeCopy::Root {
            readonly,
            newest: None,
            epoch: WriterEpoch::default(),
            seed: false,
        };
        let slot = VolumeCopy::Slot {
            slot: SlotId::new(1),
            readonly: true,
            newest: None,
            resume_token: None,
            pull: None,
        };
        let known = |role| {
            vec![KnownCopy {
                machine: name("fsn-2"),
                role,
            }]
        };
        let rows = [
            ("nothing anywhere", None, vec![], Ok(())),
            (
                "a slot elsewhere",
                None,
                known(CopyRole::Slot),
                Err(SwitchError::NoWriter),
            ),
            (
                "a writer elsewhere",
                None,
                known(CopyRole::Writer),
                Err(SwitchError::NoWriter),
            ),
            (
                "a slot here",
                Some(slot),
                vec![],
                Err(SwitchError::NoWriter),
            ),
            ("a writable root", Some(root(false)), vec![], Ok(())),
            (
                "a writable root beside a slot elsewhere",
                Some(root(false)),
                known(CopyRole::Slot),
                Ok(()),
            ),
            (
                "a read-only root",
                Some(root(true)),
                vec![],
                Err(SwitchError::VolumeSwitching),
            ),
        ];
        for (case, held, elsewhere, expected) in rows {
            assert_eq!(
                admit_plain_mount(held.as_ref(), &elsewhere),
                expected,
                "{case}"
            );
        }
    }

    fn copy(machine: &str, role: CopyRole) -> KnownCopy {
        KnownCopy {
            machine: name(machine),
            role,
        }
    }

    #[test]
    fn no_writer_message_restores_a_single_copy_and_mirrors_need_removing_first() {
        assert_eq!(
            no_writer_message("data", &[copy("fsn-2", CopyRole::Slot)]),
            "Volume data has no writer; fsn-2 holds its only copy. Make it the writer: ployz volume restore data --from fsn-2"
        );
        assert_eq!(
            no_writer_message(
                "data",
                &[
                    copy("fsn-2", CopyRole::Slot),
                    copy("hel-1", CopyRole::Slot),
                    copy("nbg-3", CopyRole::Slot),
                ]
            ),
            "Volume data has no writer; it is held as fsn-2 (copy), hel-1 (copy), nbg-3 (copy). Restore needs a single copy: keep the newest, remove each other one (ployz volume mirror rm data-fsn-2 --confirm data, ployz volume mirror rm data-hel-1 --confirm data, ployz volume mirror rm data-nbg-3 --confirm data), then run ployz volume restore data --from <server you kept>"
        );
        assert_eq!(
            no_writer_message("data", &[]),
            "Volume data has no writer and no Server holds a copy of it"
        );
    }

    #[test]
    fn no_writer_message_sends_a_mid_move_volume_to_its_move_not_to_restore() {
        for copies in [
            vec![copy("hel-1", CopyRole::Switching)],
            vec![
                copy("fsn-2", CopyRole::Slot),
                copy("hel-1", CopyRole::Switching),
            ],
        ] {
            let message = no_writer_message("data", &copies);
            assert!(
                message.starts_with("Volume data is mid-Move; it is held as "),
                "{message}"
            );
            assert!(message.contains("hel-1 (switching)"), "{message}");
            assert!(
                message.ends_with("; ployz volume release data stops it"),
                "{message}"
            );
            assert!(!message.contains("restore"), "{message}");
        }
    }

    #[test]
    fn no_writer_message_names_a_writer_elsewhere_without_a_restore_line() {
        assert_eq!(
            no_writer_message(
                "data",
                &[
                    copy("fsn-2", CopyRole::Slot),
                    copy("hel-1", CopyRole::Writer)
                ]
            ),
            "Volume data's writer is on hel-1, so this Server cannot mount it"
        );
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
    fn snapshot_names_are_run_snapshots_or_a_departure_restore_point() {
        for valid in ["w-3-1", "f-3", "dep-1690000000"] {
            assert_eq!(valid.parse::<SnapshotName>().unwrap().as_str(), valid);
        }
        for invalid in ["w-3", "f-", "dep-", "dep-x", "d-1", "manual"] {
            assert!(invalid.parse::<SnapshotName>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn switch_errors_carry_their_reason_in_details() {
        let error = SwitchError::NoWriter.rpc_error("nothing to mount");
        assert_eq!(error.code, RpcErrorCode::Conflict);
        assert_eq!(error.details, serde_json::json!({ "reason": "no_writer" }));
        assert_eq!(
            SwitchError::from_details(&error.details),
            Some(SwitchError::NoWriter)
        );
        assert_eq!(
            SwitchError::from_details(&SwitchError::NoCapacity.rpc_error("").details),
            Some(SwitchError::NoCapacity)
        );
        assert_eq!(
            SwitchError::from_details(&serde_json::json!({ "reason": "unknown" })),
            None
        );
    }
}
