use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ValueError;

/// Public half of a Cluster's Sealing Key.
#[derive(Clone, Copy, Eq, Hash, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct SealingPublicKey([u8; 32]);

impl SealingPublicKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for SealingPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&STANDARD.encode(self.0))
    }
}

impl fmt::Debug for SealingPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SealingPublicKey({self})")
    }
}

impl TryFrom<String> for SealingPublicKey {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        STANDARD
            .decode(&value)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .map(Self)
            .ok_or_else(|| ValueError::new("Sealing public key", value, "32 bytes in base64"))
    }
}

impl From<SealingPublicKey> for String {
    fn from(value: SealingPublicKey) -> Self {
        value.to_string()
    }
}

/// A Cluster's Sealing Key private half, sealed by the issuing member to one
/// joiner's WireGuard key. Only that joiner can open it.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct SealedSealingKey(Vec<u8>);

impl SealedSealingKey {
    #[must_use]
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SealedSealingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SealedSealingKey(..)")
    }
}

impl TryFrom<String> for SealedSealingKey {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        STANDARD
            .decode(&value)
            .map(Self)
            .map_err(|_| ValueError::new("sealed Sealing Key", value, "base64"))
    }
}

impl From<SealedSealingKey> for String {
    fn from(value: SealedSealingKey) -> Self {
        STANDARD.encode(value.0)
    }
}

/// Whether a Machine holds its Cluster's Sealing Key.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sealing {
    Held {
        public_key: SealingPublicKey,
    },
    /// The Machine joined without receiving the key and cannot open sealed
    /// values until a member hands it over.
    NeedsAccess,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_round_trips_as_base64() {
        let key = SealingPublicKey::from_bytes([7; 32]);
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, format!("\"{}\"", STANDARD.encode([7; 32])));
        assert_eq!(
            serde_json::from_str::<SealingPublicKey>(&json).unwrap(),
            key
        );
        assert!(serde_json::from_str::<SealingPublicKey>("\"AAAA\"").is_err());
    }

    #[test]
    fn sealing_states_are_distinguishable_on_the_wire() {
        let held = Sealing::Held {
            public_key: SealingPublicKey::from_bytes([1; 32]),
        };
        assert_eq!(
            serde_json::to_value(held).unwrap(),
            serde_json::json!({ "kind": "held", "public_key": STANDARD.encode([1; 32]) })
        );
        assert_eq!(
            serde_json::to_value(Sealing::NeedsAccess).unwrap(),
            serde_json::json!({ "kind": "needs_access" })
        );
    }

    #[test]
    fn sealed_key_debug_is_redacted() {
        let sealed = SealedSealingKey::from_bytes(vec![9; 48]);
        assert_eq!(format!("{sealed:?}"), "SealedSealingKey(..)");
    }
}
