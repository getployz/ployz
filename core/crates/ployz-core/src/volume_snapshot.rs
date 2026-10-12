//! User snapshots of a Writer root: taken, kept, deleted and restored on request. They
//! live beside the run's snapshots under their own names, so run retention never sees
//! them, and a send between Machines never carries them.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DockerVolumeName, RpcError, RpcErrorCode};

/// Which run of writes a Writer root belongs to. Cloud numbers it; the Machine stores it
/// on the root and compares it by equality only. A root without one reads as 0.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS,
)]
#[serde(transparent)]
pub struct WriterEpoch(u32);

impl WriterEpoch {
    #[must_use]
    pub const fn new(epoch: u32) -> Self {
        Self(epoch)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for WriterEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A user snapshot's id, minted by the sender so a resend names the same snapshot.
///
/// Invariant: 1 to 64 of `a-z`, `0-9` and `-`. On ZFS it is the snapshot `u-<id>`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct SnapshotId(String);

impl SnapshotId {
    pub const MAX_LEN: usize = 64;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid snapshot id {0:?}; use 1 to 64 of a-z, 0-9 and -")]
pub struct SnapshotIdParseError(pub String);

impl FromStr for SnapshotId {
    type Err = SnapshotIdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid = (1..=Self::MAX_LEN).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        valid
            .then(|| Self(value.to_owned()))
            .ok_or_else(|| SnapshotIdParseError(value.to_owned()))
    }
}

impl TryFrom<String> for SnapshotId {
    type Error = SnapshotIdParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<SnapshotId> for String {
    fn from(id: SnapshotId) -> Self {
        id.0
    }
}

impl fmt::Display for SnapshotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Why a snapshot was taken. Cloud schedules and prunes by kind; the Machine only stores it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotKind {
    Manual,
    Hourly,
    Daily,
    BeforeDeploy,
    AfterDeploy,
    BeforeRestore,
}

impl SnapshotKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Hourly => "hourly",
            Self::Daily => "daily",
            Self::BeforeDeploy => "before_deploy",
            Self::AfterDeploy => "after_deploy",
            Self::BeforeRestore => "before_restore",
        }
    }
}

impl FromStr for SnapshotKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "manual" => Self::Manual,
            "hourly" => Self::Hourly,
            "daily" => Self::Daily,
            "before_deploy" => Self::BeforeDeploy,
            "after_deploy" => Self::AfterDeploy,
            "before_restore" => Self::BeforeRestore,
            other => return Err(format!("unknown snapshot kind {other:?}")),
        })
    }
}

/// Free text shown beside the kind, such as `#41 · e81b7d0`.
///
/// Invariant: at most 256 bytes, no control characters.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct SnapshotNote(String);

impl SnapshotNote {
    pub const MAX_LEN: usize = 256;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SnapshotNote {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() > Self::MAX_LEN || value.chars().any(char::is_control) {
            return Err(format!(
                "invalid snapshot note {value:?}; use at most {} bytes and no control characters",
                Self::MAX_LEN
            ));
        }
        Ok(Self(value))
    }
}

impl From<SnapshotNote> for String {
    fn from(note: SnapshotNote) -> Self {
        note.0
    }
}

/// What a snapshot is, as the list shows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SnapshotLabel {
    pub kind: SnapshotKind,
    #[serde(default)]
    pub note: SnapshotNote,
}

/// Move this Machine's user snapshots of `name` to `want`, provided its Writer root is
/// numbered `expect`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeSnapshotRequest {
    pub name: DockerVolumeName,
    pub expect: WriterEpoch,
    pub want: WantedSnapshot,
}

/// The snapshot state the sender wants. Each is at want by id, so a resend is a no-op.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WantedSnapshot {
    /// A snapshot `id` of the root as it is now.
    Taken {
        id: SnapshotId,
        label: SnapshotLabel,
        #[serde(default)]
        kept: bool,
    },
    /// Snapshot `id` marked kept or not. Cloud's pruning skips kept snapshots.
    Kept { id: SnapshotId, kept: bool },
    /// No snapshot `id`.
    Deleted { id: SnapshotId },
    /// The root holds the data of snapshot `from`, and the data it held before is kept
    /// as snapshot `save`. Refuses while a Container holding the Volume runs.
    Restored {
        from: SnapshotId,
        save: SnapshotId,
        label: SnapshotLabel,
    },
}

/// Ask a Machine for the user snapshots of one Volume.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ListVolumeSnapshotsRequest {
    pub name: DockerVolumeName,
}

/// The user snapshots this Machine holds for one Volume, newest first. `epoch` is the
/// Writer root's, or absent when this Machine holds no Writer root.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeSnapshots {
    pub epoch: Option<WriterEpoch>,
    pub snapshots: Vec<UserSnapshot>,
}

/// One user snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct UserSnapshot {
    pub id: SnapshotId,
    pub label: SnapshotLabel,
    pub kept: bool,
    pub created_unix_seconds: i64,
    /// Bytes written between the snapshot before this one and this one.
    pub written_bytes: u64,
    /// Bytes of data this snapshot refers to.
    pub referenced_bytes: u64,
    /// Bytes written to the live Volume since this snapshot: what a restore to it
    /// replaces. For a snapshot taken after the live data's last restore point, the
    /// bytes written since that restore, a lower bound.
    pub loses_bytes: u64,
}

/// Why a Machine refused a `SetVolumeSnapshot`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotRefusal {
    /// The Writer root's epoch is not `expect`.
    Mismatch,
    /// This Machine holds no idle Writer root for the Volume.
    NoWriter,
    /// A Container holding the Volume runs, so the root cannot be swapped.
    Busy,
    /// The snapshot the want names does not exist.
    NotFound,
}

/// `RpcError.details` of a refused `SetVolumeSnapshot`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeSnapshotRefused {
    pub reason: SnapshotRefusal,
}

impl SetVolumeSnapshotRefused {
    #[must_use]
    pub fn from_details(details: &serde_json::Value) -> Option<Self> {
        Self::deserialize(details).ok()
    }

    #[must_use]
    pub fn rpc_error(self, message: impl Into<String>) -> RpcError {
        let code = match self.reason {
            SnapshotRefusal::NotFound => RpcErrorCode::NotFound,
            SnapshotRefusal::Mismatch | SnapshotRefusal::NoWriter | SnapshotRefusal::Busy => {
                RpcErrorCode::Conflict
            }
        };
        RpcError {
            code,
            message: message.into(),
            details: serde_json::to_value(self).expect("a snapshot refusal is JSON serializable"),
            cause: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_id_accepts_only_its_charset_and_length() {
        assert!("0193-abc".parse::<SnapshotId>().is_ok());
        assert!("".parse::<SnapshotId>().is_err());
        assert!("Upper".parse::<SnapshotId>().is_err());
        assert!("a@b".parse::<SnapshotId>().is_err());
        assert!("a".repeat(65).parse::<SnapshotId>().is_err());
    }

    #[test]
    fn note_refuses_control_characters_and_long_text() {
        assert!(SnapshotNote::try_from("#41 · e81b7d0".to_owned()).is_ok());
        assert!(SnapshotNote::try_from("a\tb".to_owned()).is_err());
        assert!(SnapshotNote::try_from("a".repeat(257)).is_err());
    }

    #[test]
    fn kind_round_trips_through_its_stored_text() {
        for kind in [
            SnapshotKind::Manual,
            SnapshotKind::Hourly,
            SnapshotKind::Daily,
            SnapshotKind::BeforeDeploy,
            SnapshotKind::AfterDeploy,
            SnapshotKind::BeforeRestore,
        ] {
            assert_eq!(kind.as_str().parse::<SnapshotKind>(), Ok(kind));
            assert_eq!(
                serde_json::to_value(kind).expect("kind serializes"),
                kind.as_str()
            );
        }
    }
}
