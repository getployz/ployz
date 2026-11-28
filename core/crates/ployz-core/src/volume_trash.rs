//! A Server's Trash: the data its Pool kept from each Volume copy it deleted, and the three
//! routes that list, restore and empty it. An Operator lists it; it restores or empties only
//! with a person's [`TrashApproval`] of those entries on that Server.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    DockerVolumeName, ValueError, VolumeOrigin, WriterEpoch,
    authority::{Signed, TrashApproval},
};

/// How many hourly sweeps a Trash entry waits before the sweep destroys it: 7 days of
/// hours the Server ran.
pub const TRASH_SWEEP_HOURS: u64 = 168;

/// Lowercase RFC 4648 base32, which ZFS takes in a dataset name.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const ID_LEN: usize = 26;

/// One Trash entry's name under `<pool>/ployz-trash`: 16 random bytes in base32. The Server
/// that made the entry minted it and never reuses one.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct TrashId([u8; ID_LEN]);

impl TrashId {
    /// The ID of `bytes`.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        // 128 bits fill 26 five-bit characters with two bits to spare, padded with zeros.
        let bits = u128::from_be_bytes(bytes);
        let mut id = [0; ID_LEN];
        for (at, character) in id.iter_mut().enumerate() {
            let shift = 5 * (ID_LEN - 1 - at);
            let value = if shift >= 2 {
                bits >> (shift - 2)
            } else {
                bits << (2 - shift)
            };
            *character = ALPHABET.get((value & 31) as usize).copied().unwrap_or(b'a');
        }
        Self(id)
    }

    /// # Errors
    /// Returns a [`ValueError`] for anything but 26 lowercase base32 characters.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let value = value.as_ref();
        let mut id = [0; ID_LEN];
        if value.len() != ID_LEN || !value.bytes().all(|byte| ALPHABET.contains(&byte)) {
            return Err(ValueError::new(
                "trash ID",
                value,
                "26 characters from a-z and 2-7",
            ));
        }
        id.copy_from_slice(value.as_bytes());
        Ok(Self(id))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("a trash ID is ASCII")
    }
}

impl fmt::Display for TrashId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Debug for TrashId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("TrashId")
            .field(&self.as_str())
            .finish()
    }
}

impl FromStr for TrashId {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for TrashId {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<TrashId> for String {
    fn from(id: TrashId) -> Self {
        id.as_str().to_owned()
    }
}

/// Which copy of a Volume an entry was before its delete.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TrashPlace {
    /// The Writer's root.
    Root,
    /// A Mirror or Old copy slot, with its copy.
    Slot,
    /// The copy inside a Mirror slot a full refresh replaced; the slot stayed.
    Copy,
}

impl TrashPlace {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Slot => "slot",
            Self::Copy => "copy",
        }
    }
}

impl FromStr for TrashPlace {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "root" => Ok(Self::Root),
            "slot" => Ok(Self::Slot),
            "copy" => Ok(Self::Copy),
            _ => Err(ValueError::new("trash place", value, "root, slot or copy")),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListTrashRequest {}

/// What this Server's Trash holds.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct TrashList {
    #[serde(default)]
    pub entries: Vec<TrashEntry>,
    /// Entries whose marks are missing or unreadable. Neither the sweep nor an Empty
    /// destroys them; only root on the Server can.
    #[serde(default)]
    pub unreadable: Vec<UnreadableTrash>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct TrashEntry {
    pub id: TrashId,
    pub place: TrashPlace,
    pub name: DockerVolumeName,
    /// The born origin of its data; `None` is Legacy, or an origin this Server can't read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<VolumeOrigin>,
    /// Unix seconds by this Server's clock when the delete moved it here.
    #[ts(type = "number")]
    pub trashed_at: i64,
    /// Unix seconds when the sweep is expected to destroy it: an estimate from the hours
    /// it has left, which only advance while the Server runs.
    #[ts(type = "number")]
    pub expires_at: i64,
    #[ts(type = "number")]
    pub used_bytes: u64,
    /// A restore pinned it: it waits to become the Volume's data, and `expires_at` counts
    /// from the restore's last attempt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restoring: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct UnreadableTrash {
    pub id: TrashId,
    #[ts(type = "number")]
    pub used_bytes: u64,
    pub reason: String,
}

/// A deleted root comes back in two calls: the first pins it, so it waits in the Trash for
/// the restore, and the second, once every Server showed it holds no copy, makes it the
/// Volume's data. A mirror or old copy lands as an old copy in the first.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RestoreTrashRequest {
    pub id: TrashId,
    /// Make the pinned root the Volume's data, at the epoch its pin holds.
    #[serde(default)]
    pub promote: bool,
    /// A person's approval of restoring `id` on this Machine, which an Operator's restore
    /// needs and a Full caller's doesn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Signed<TrashApproval>>,
}

/// Where a restore of `id` stands, under the entry's Docker Volume name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct TrashRestored {
    pub id: TrashId,
    pub name: DockerVolumeName,
    pub place: TrashPlace,
    pub state: TrashRestore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrashRestore {
    /// The root waits in the Trash for a promote at `epoch`.
    Pinned { epoch: WriterEpoch },
    /// The mirror or old copy is back as an old copy.
    Landed,
    /// The root is the Volume's data at `epoch`. It says only that the restore committed:
    /// the root may have changed since.
    Restored { epoch: WriterEpoch },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EmptyTrashRequest {
    pub ids: Vec<TrashId>,
    /// Emptying the whole Trash: entries a restore pinned stay. Named alone, they go.
    #[serde(default)]
    pub bulk: bool,
    /// A person's approval of emptying exactly `ids` on this Machine, which an Operator's
    /// empty needs and a Full caller's doesn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Signed<TrashApproval>>,
}

/// Each named entry destroyed, or why not. A named entry the Trash doesn't hold is
/// `missing`, not a failure.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct TrashEmptied {
    #[serde(default)]
    pub emptied: Vec<TrashId>,
    #[serde(default)]
    pub missing: Vec<TrashId>,
    #[serde(default)]
    pub failed: Vec<TrashFailure>,
    /// Entries a bulk empty kept because a restore pinned them.
    #[serde(default)]
    pub restoring: Vec<TrashId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct TrashFailure {
    pub id: TrashId,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::TrashId;

    #[test]
    fn an_id_is_its_bytes_in_lowercase_base32_and_reads_back() {
        let id = TrashId::from_bytes([0xff; 16]);
        assert_eq!(id.as_str(), "77777777777777777777777774");
        assert_eq!(TrashId::parse(id.as_str()).unwrap(), id);
        assert_eq!(TrashId::from_bytes([0; 16]).as_str(), "a".repeat(26));
        for refused in ["", "A".repeat(26).as_str(), "a".repeat(25).as_str(), "../x"] {
            assert!(TrashId::parse(refused).is_err(), "{refused}");
        }
    }
}
