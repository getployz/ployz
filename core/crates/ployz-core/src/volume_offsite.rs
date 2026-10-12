//! Offsite backups: full sends of a Volume's snapshots kept in an S3-compatible bucket
//! the user owns. Every request carries the bucket's credentials; the Machine keeps them
//! in memory for the request or the task it starts, and never on disk.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DockerVolumeName, SnapshotGuid, SnapshotId, SnapshotLabel, WriterEpoch};

/// Where backups live and the keys that reach them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OffsiteBucket {
    /// `http://` or `https://` base URL of the S3 API, without the bucket. Requests use
    /// path-style addressing, which every S3-compatible store accepts.
    pub endpoint: String,
    pub region: String,
    pub bucket: BucketName,
    /// Keys start with `<prefix>/<volume>/<backup>/`.
    pub prefix: OffsitePrefix,
    pub access_key: String,
    pub secret_key: SecretKey,
}

/// An S3 bucket name: 3 to 63 lowercase letters, digits, `-` and `.`, starting and
/// ending with a letter or digit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct BucketName(String);

impl BucketName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BucketName {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let edge = |byte: Option<&u8>| {
            byte.is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        };
        if !(3..=63).contains(&value.len())
            || !edge(value.as_bytes().first())
            || !edge(value.as_bytes().last())
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
            })
        {
            return Err(format!("invalid bucket name {value:?}"));
        }
        Ok(Self(value))
    }
}

impl From<BucketName> for String {
    fn from(name: BucketName) -> Self {
        name.0
    }
}

/// The key prefix Cloud gives a bucket: `/`-separated segments of letters, digits, `.`,
/// `_` and `-`, at most 256 bytes. No segment is empty, `.` or `..`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct OffsitePrefix(String);

impl OffsitePrefix {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for OffsitePrefix {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let segment = |segment: &str| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        };
        if value.len() > 256 || !value.split('/').all(segment) {
            return Err(format!("invalid offsite prefix {value:?}"));
        }
        Ok(Self(value))
    }
}

impl From<OffsitePrefix> for String {
    fn from(prefix: OffsitePrefix) -> Self {
        prefix.0
    }
}

/// The bucket's secret key. `Debug` never prints it, so a logged request stays clean.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(type = "string")]
pub struct SecretKey(String);

impl SecretKey {
    #[must_use]
    pub fn new(secret: String) -> Self {
        Self(secret)
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretKey(..)")
    }
}

/// The idempotency key of one backup, chosen by Cloud: 1 to 64 lowercase letters,
/// digits and `-`, starting with a letter or digit. It names the backup's keys.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct BackupId(String);

impl BackupId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BackupId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let mut bytes = value.bytes();
        if value.len() > 64
            || !bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(format!("invalid backup id {value:?}"));
        }
        Ok(Self(value))
    }
}

impl From<BackupId> for String {
    fn from(id: BackupId) -> Self {
        id.0
    }
}

impl fmt::Display for BackupId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What a backup's list row says about it, such as `manual` or `daily 03:00`. At most
/// 200 bytes.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct BackupNote(String);

impl BackupNote {
    pub const MAX_BYTES: usize = 200;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BackupNote {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() > Self::MAX_BYTES {
            return Err(format!(
                "a backup note must be at most {} bytes; this one is {}",
                Self::MAX_BYTES,
                value.len()
            ));
        }
        Ok(Self(value))
    }
}

impl From<BackupNote> for String {
    fn from(note: BackupNote) -> Self {
        note.0
    }
}

/// Move backup `backup` of Volume `name` in `bucket` to `want`. Resending the same
/// request reports progress and starts nothing twice; resending after `Failed` retries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeOffsiteRequest {
    pub name: DockerVolumeName,
    pub bucket: OffsiteBucket,
    pub backup: BackupId,
    pub want: WantedOffsite,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WantedOffsite {
    /// The backup holds a full send of snapshot `from`, found on this Machine's root or
    /// mirror of `name`.
    Uploaded {
        from: SnapshotGuid,
        note: BackupNote,
    },
    /// This Machine holds the backup as the replacement for restore `save`: a read-only
    /// dataset outside Docker, bounded like the Volume it came from. Also at want once
    /// that restore has swapped it in.
    Downloaded { save: SnapshotId },
    /// The downloaded backup is `name`'s root on this Writer. The old root's data stays
    /// listed as user snapshot `save` with `label`. Stop every Container holding `name`
    /// first.
    Restored {
        expect: WriterEpoch,
        save: SnapshotId,
        label: SnapshotLabel,
    },
    /// The bucket holds nothing of the backup.
    Gone,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeOffsiteReply {
    pub status: OffsiteStatus,
}

/// Where the backup is against the want. `Failed` is reported once; the next request
/// starts the work again.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OffsiteStatus {
    /// A task works toward the want; ask again.
    Running {
        bytes: u64,
    },
    Done,
    Failed {
        reason: String,
    },
}

/// List the backups of Volume `name` in `bucket`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct InspectVolumeOffsiteRequest {
    pub name: DockerVolumeName,
    pub bucket: OffsiteBucket,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeOffsiteView {
    /// Complete backups, newest first. An upload still running is not listed.
    pub backups: Vec<OffsiteBackup>,
    /// The backup this Machine holds downloaded for a restore not yet swapped in.
    pub replacement: Option<BackupId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OffsiteBackup {
    pub id: BackupId,
    /// When the snapshot it holds was taken.
    pub taken_unix_seconds: i64,
    /// Size of the stored stream, which a restore downloads.
    pub bytes: u64,
    pub note: BackupNote,
}

/// Prove the Machine can write and delete in `bucket`, for "New bucket".
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct CheckOffsiteRequest {
    pub bucket: OffsiteBucket,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OffsiteChecked {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logged_bucket_never_shows_its_secret() {
        let bucket = OffsiteBucket {
            endpoint: "https://s3.example.com".to_owned(),
            region: "auto".to_owned(),
            bucket: BucketName::try_from("backups".to_owned()).unwrap(),
            prefix: OffsitePrefix::try_from("org/proj".to_owned()).unwrap(),
            access_key: "AKIA".to_owned(),
            secret_key: SecretKey::new("hunter2-very-secret".to_owned()),
        };
        let logged = format!("{bucket:?}");
        assert!(!logged.contains("hunter2"), "{logged}");
        assert!(
            serde_json::to_string(&bucket)
                .unwrap()
                .contains("\"secret_key\":\"hunter2-very-secret\"")
        );
    }

    #[test]
    fn keys_reject_what_would_escape_their_place() {
        for prefix in ["", "/a", "a/", "a//b", "a/../b", "a/./b", "a b", "a\\b"] {
            assert!(
                OffsitePrefix::try_from(prefix.to_owned()).is_err(),
                "{prefix:?}"
            );
        }
        assert!(OffsitePrefix::try_from("org_1/proj-2.x".to_owned()).is_ok());
        for id in ["", "-a", "A1", "a/b", "a.b", &"a".repeat(65)] {
            assert!(BackupId::try_from(id.to_owned()).is_err(), "{id:?}");
        }
        assert!(BackupId::try_from("2026-10-12-0300".to_owned()).is_ok());
        for bucket in ["ab", "Backups", "-backups", "backups-", "back_ups"] {
            assert!(
                BucketName::try_from(bucket.to_owned()).is_err(),
                "{bucket:?}"
            );
        }
        assert!(BackupNote::try_from("x".repeat(201)).is_err());
    }
}
