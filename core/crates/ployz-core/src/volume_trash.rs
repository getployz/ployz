//! A Server's Trash: the data its Pool kept from each Volume copy it deleted, and the three
//! routes that list, restore and empty it.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::{DockerVolumeName, ValueError, VolumeOrigin};

/// How many hourly sweeps a Trash entry waits before the sweep destroys it: 7 days of
/// hours the Server ran.
pub const TRASH_SWEEP_HOURS: u64 = 168;

/// Lowercase RFC 4648 base32, which ZFS takes in a dataset name.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const ID_LEN: usize = 26;

/// One Trash entry's name under `<pool>/ployz-trash`: 16 random bytes in base32. The Server
/// that made the entry minted it and never reuses one.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrashList {
    #[serde(default)]
    pub entries: Vec<TrashEntry>,
    /// Entries whose marks are missing or unreadable. Neither the sweep nor an Empty
    /// destroys them; only root on the Server can.
    #[serde(default)]
    pub unreadable: Vec<UnreadableTrash>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrashEntry {
    pub id: TrashId,
    pub place: TrashPlace,
    pub name: DockerVolumeName,
    /// The born origin of its data; `None` is Legacy, or an origin this Server can't read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<VolumeOrigin>,
    /// Unix seconds by this Server's clock when the delete moved it here.
    pub trashed_at: i64,
    /// Unix seconds when the sweep is expected to destroy it: an estimate from the hours
    /// it has left, which only advance while the Server runs.
    pub expires_at: i64,
    pub used_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnreadableTrash {
    pub id: TrashId,
    pub used_bytes: u64,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RestoreTrashRequest {
    pub id: TrashId,
}

/// The entry back where it was taken from, under its Docker Volume name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrashRestored {
    pub id: TrashId,
    pub name: DockerVolumeName,
    pub place: TrashPlace,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EmptyTrashRequest {
    pub ids: Vec<TrashId>,
}

/// Each named entry destroyed, or why not. A named entry the Trash doesn't hold is
/// `missing`, not a failure.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrashEmptied {
    #[serde(default)]
    pub emptied: Vec<TrashId>,
    #[serde(default)]
    pub missing: Vec<TrashId>,
    #[serde(default)]
    pub failed: Vec<TrashFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
