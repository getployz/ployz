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
        /// What this Machine recorded the slot as. Older daemons omit it, and it stays
        /// omitted: an unreported role is unknown, not a Mirror.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        role: Option<SlotRole>,
    },
}

/// What a slot holds its copy for, as its own Machine recorded it: a pull makes it a
/// Mirror, and a cut or a departure leaves the root behind as an old copy. A slot no step
/// marked reads as a Mirror.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SlotRole {
    #[default]
    Mirror,
    OldCopy,
}

impl SlotRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mirror => "mirror",
            Self::OldCopy => "old_copy",
        }
    }
}

/// A slot role this Machine did not write.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid slot role {0:?}; expected mirror or old_copy")]
pub struct SlotRoleParseError(pub String);

impl FromStr for SlotRole {
    type Err = SlotRoleParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "mirror" => Ok(Self::Mirror),
            "old_copy" => Ok(Self::OldCopy),
            _ => Err(SlotRoleParseError(value.to_owned())),
        }
    }
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

    /// [`Self::path_and_query`] fit to print or store: a resume token reads `token=…`,
    /// so it never lands in a run's message, the CLI or a log.
    #[must_use]
    pub fn shown(&self) -> String {
        match &self.source {
            SendSource::Resume { target, .. } => {
                format!("{}{}?target={target}&token=…", Self::PATH_PREFIX, self.name)
            }
            SendSource::Full { .. } | SendSource::Incremental { .. } => self.path_and_query(),
        }
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

/// Who drives a Deploy, and so what a refusal can offer as the way back.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    /// Ployz Cloud, whose Volume Runs make a copy the writer again. An Intent that
    /// doesn't say is Cloud's.
    #[default]
    Cloud,
    /// The CLI alone, with no Cloud: `ployz volume restore` runs from the CLI, and Moves
    /// can't be stopped.
    Cli,
}

impl Operator {
    /// Whether this is the default, Ployz Cloud, which serialized Intents leave out.
    #[must_use]
    pub const fn is_cloud(&self) -> bool {
        matches!(self, Self::Cloud)
    }
}

/// What a Machine saw of Volume `name` when it could not mount a writer: the Server that
/// holds the writer, the Move holding it, or the copies left. No command line: a Machine
/// does not know who drives the Deploy, so it cannot name the way back. `name` is the
/// Volume as its owner named it, not its Docker name.
#[must_use]
pub fn no_writer_observation(name: &str, copies: &[KnownCopy]) -> String {
    match NoWriterCase::of(copies) {
        NoWriterCase::WriterElsewhere(writer) => format!(
            "Volume {name}'s writer is on {}, so this Server cannot mount it",
            writer.machine
        ),
        NoWriterCase::MidMove => format!(
            "Volume {name} is mid-Move; it is held as {}",
            listed(copies)
        ),
        NoWriterCase::NoCopy => {
            format!("Volume {name} was deployed, but no Server in this Cluster holds a copy of it")
        }
        NoWriterCase::OneCopy(only) => {
            format!(
                "Volume {name} has no writer; {} holds its only copy",
                only.machine
            )
        }
        NoWriterCase::SeveralCopies => {
            format!(
                "Volume {name} has no writer; it is held as {}",
                listed(copies)
            )
        }
    }
}

/// The refusal a plain Deploy of Volume `name` reads when no Server can mount its writer:
/// what was observed, then the way back `operator` can take. Moves are Cloud's: the CLI alone
/// is told that stopping one needs Cloud. With no Move under way, the CLI alone is told to
/// restore, and that `ployz volume rm` starts over empty.
#[must_use]
pub fn no_writer_message(name: &str, copies: &[KnownCopy], operator: Operator) -> String {
    let observed = no_writer_observation(name, copies);
    match (NoWriterCase::of(copies), operator) {
        (NoWriterCase::WriterElsewhere(_), _) => observed,
        (NoWriterCase::MidMove, Operator::Cloud) => {
            format!("{observed}; ployz volume release {name} stops it")
        }
        (NoWriterCase::MidMove, Operator::Cli) => {
            format!("{observed}; stopping it needs Ployz Cloud")
        }
        (NoWriterCase::NoCopy, Operator::Cloud) => format!(
            "{observed}. If a Server you removed still holds its data, add that Server back, then run ployz volume restore {name} --from <server>"
        ),
        (NoWriterCase::NoCopy, Operator::Cli) => format!(
            "{observed}. If a Server you removed still holds its data, add that Server back, then run ployz volume restore {name} --from <server>. {}",
            start_over(name)
        ),
        (NoWriterCase::OneCopy(only), Operator::Cloud) => format!(
            "{observed}. Make it the writer: ployz volume restore {name} --from {}",
            only.machine
        ),
        (NoWriterCase::OneCopy(only), Operator::Cli) => format!(
            "{observed}. Make it the writer: ployz volume restore {name} --from {}. {}",
            only.machine,
            start_over(name)
        ),
        // Copies carry no age, so the operator picks which one to keep. They carry no slot
        // role either, so no removal line is printed: run late, it could delete a live mirror.
        (NoWriterCase::SeveralCopies, Operator::Cloud) => format!(
            "{observed}. Restore needs a single copy: keep the newest, delete each other one from the Volume's page in Ployz Cloud, which shows whether it is a mirror or an old copy, then run ployz volume restore {name} --from <server you kept>"
        ),
        (NoWriterCase::SeveralCopies, Operator::Cli) => format!(
            "{observed}. Restore needs a single copy: keep the newest, remove each other Server from the Cluster with ployz server rm, then run ployz volume restore {name} --from <server you kept>. {}",
            start_over(name)
        ),
    }
}

/// The standalone way out that loses the data.
fn start_over(name: &str) -> String {
    format!(
        "To start over with an empty Volume instead, run ployz volume rm {name}, which deletes its data, then add it again and deploy"
    )
}

/// Why a Volume has no writer this Server can mount, read off the copies known of it.
enum NoWriterCase<'a> {
    WriterElsewhere(&'a KnownCopy),
    MidMove,
    NoCopy,
    OneCopy(&'a KnownCopy),
    SeveralCopies,
}

impl<'a> NoWriterCase<'a> {
    fn of(copies: &'a [KnownCopy]) -> Self {
        if let Some(writer) = copies.iter().find(|copy| copy.role == CopyRole::Writer) {
            return Self::WriterElsewhere(writer);
        }
        if copies.iter().any(|copy| copy.role == CopyRole::Switching) {
            return Self::MidMove;
        }
        match copies {
            [] => Self::NoCopy,
            [only] => Self::OneCopy(only),
            _ => Self::SeveralCopies,
        }
    }
}

fn listed(copies: &[KnownCopy]) -> String {
    copies
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
        .join(", ")
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
            role: Some(SlotRole::Mirror),
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
            no_writer_message("data", &[copy("fsn-2", CopyRole::Slot)], Operator::Cloud),
            "Volume data has no writer; fsn-2 holds its only copy. Make it the writer: ployz volume restore data --from fsn-2"
        );
        assert_eq!(
            no_writer_message(
                "data",
                &[
                    copy("fsn-2", CopyRole::Slot),
                    copy("hel-1", CopyRole::Slot),
                    copy("nbg-3", CopyRole::Slot),
                ],
                Operator::Cloud
            ),
            "Volume data has no writer; it is held as fsn-2 (copy), hel-1 (copy), nbg-3 (copy). Restore needs a single copy: keep the newest, delete each other one from the Volume's page in Ployz Cloud, which shows whether it is a mirror or an old copy, then run ployz volume restore data --from <server you kept>"
        );
        let several = no_writer_message(
            "data",
            &[copy("fsn-2", CopyRole::Slot), copy("hel-1", CopyRole::Slot)],
            Operator::Cloud,
        );
        assert!(!several.contains("mirror rm"), "{several}");
        assert_eq!(
            no_writer_message("data", &[], Operator::Cloud),
            "Volume data was deployed, but no Server in this Cluster holds a copy of it. If a Server you removed still holds its data, add that Server back, then run ployz volume restore data --from <server>"
        );
    }

    #[test]
    fn no_writer_message_without_cloud_offers_restore_and_starting_empty() {
        assert_eq!(
            no_writer_message("data", &[copy("fsn-2", CopyRole::Slot)], Operator::Cli),
            "Volume data has no writer; fsn-2 holds its only copy. Make it the writer: ployz volume restore data --from fsn-2. To start over with an empty Volume instead, run ployz volume rm data, which deletes its data, then add it again and deploy"
        );
        assert_eq!(
            no_writer_message("data", &[], Operator::Cli),
            "Volume data was deployed, but no Server in this Cluster holds a copy of it. If a Server you removed still holds its data, add that Server back, then run ployz volume restore data --from <server>. To start over with an empty Volume instead, run ployz volume rm data, which deletes its data, then add it again and deploy"
        );
        assert_eq!(
            no_writer_message(
                "data",
                &[copy("fsn-2", CopyRole::Slot), copy("hel-1", CopyRole::Slot)],
                Operator::Cli
            ),
            "Volume data has no writer; it is held as fsn-2 (copy), hel-1 (copy). Restore needs a single copy: keep the newest, remove each other Server from the Cluster with ployz server rm, then run ployz volume restore data --from <server you kept>. To start over with an empty Volume instead, run ployz volume rm data, which deletes its data, then add it again and deploy"
        );
        assert_eq!(
            no_writer_message("data", &[copy("hel-1", CopyRole::Switching)], Operator::Cli),
            "Volume data is mid-Move; it is held as hel-1 (switching); stopping it needs Ployz Cloud"
        );
        for copies in [
            vec![],
            vec![copy("fsn-2", CopyRole::Slot)],
            vec![copy("fsn-2", CopyRole::Slot), copy("hel-1", CopyRole::Slot)],
            vec![copy("hel-1", CopyRole::Switching)],
        ] {
            let message = no_writer_message("data", &copies, Operator::Cli);
            for cloud_only in ["volume release", "Volume's page"] {
                assert!(!message.contains(cloud_only), "{copies:?}: {message}");
            }
        }
        let writer_elsewhere = [copy("hel-1", CopyRole::Writer)];
        assert_eq!(
            no_writer_message("data", &writer_elsewhere, Operator::Cli),
            no_writer_message("data", &writer_elsewhere, Operator::Cloud)
        );
    }

    #[test]
    fn no_writer_observation_reports_what_the_machine_saw_without_a_command() {
        let cases = [
            vec![],
            vec![copy("fsn-2", CopyRole::Slot)],
            vec![copy("fsn-2", CopyRole::Slot), copy("hel-1", CopyRole::Slot)],
            vec![copy("hel-1", CopyRole::Switching)],
            vec![copy("hel-1", CopyRole::Writer)],
        ];
        for copies in cases {
            let observed = no_writer_observation("data", &copies);
            assert!(!observed.contains("ployz "), "{observed}");
            assert!(
                no_writer_message("data", &copies, Operator::Cloud).starts_with(&observed),
                "{observed}"
            );
            assert!(
                no_writer_message("data", &copies, Operator::Cli).starts_with(&observed),
                "{observed}"
            );
        }
        assert_eq!(
            no_writer_observation("data", &[copy("fsn-2", CopyRole::Slot)]),
            "Volume data has no writer; fsn-2 holds its only copy"
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
            let message = no_writer_message("data", &copies, Operator::Cloud);
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
                ],
                Operator::Cloud
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
