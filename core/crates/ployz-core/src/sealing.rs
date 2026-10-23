use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use zeroize::Zeroizing;

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

/// The Sealing Key wrapped under a recovery code. Cloud stores it; only the
/// code opens it.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct SealingKeyBackup(Vec<u8>);

impl SealingKeyBackup {
    #[must_use]
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SealingKeyBackup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SealingKeyBackup(..)")
    }
}

impl TryFrom<String> for SealingKeyBackup {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        STANDARD
            .decode(&value)
            .map(Self)
            .map_err(|_| ValueError::new("Sealing Key backup", value, "base64"))
    }
}

impl From<SealingKeyBackup> for String {
    fn from(value: SealingKeyBackup) -> Self {
        STANDARD.encode(value.0)
    }
}

/// What a founder of an Organization's next Cluster recovers instead of
/// generating a Sealing Key: the public key its sealed values need, and the
/// backup that holds it when Cloud has one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RecoverySeed {
    pub expected_public_key: SealingPublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<SealingKeyBackup>,
}

const RECOVERY_CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const RECOVERY_CODE_LEN: usize = 26;
const RECOVERY_CODE_GROUPS: [usize; 5] = [5, 5, 5, 5, 6];

/// The secret that opens a [`SealingKeyBackup`]: 26 base32 symbols, 130 bits.
#[derive(Clone, Eq, PartialEq)]
pub struct RecoveryCode(Zeroizing<[u8; RECOVERY_CODE_LEN]>);

#[derive(Debug, thiserror::Error)]
#[error("recovery code must be 26 letters and digits 2 to 7")]
pub struct RecoveryCodeError;

impl RecoveryCode {
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn generate() -> Self {
        use rand_core::{OsRng, RngCore as _, TryRngCore as _};

        let mut code = Zeroizing::new([0; RECOVERY_CODE_LEN]);
        OsRng.unwrap_err().fill_bytes(code.as_mut_slice());
        for symbol in code.iter_mut() {
            let index = *symbol & 31;
            *symbol = if index < 26 {
                b'A' + index
            } else {
                b'2' + index - 26
            };
        }
        Self(code)
    }

    /// Accepts any case and ignores dashes and whitespace.
    pub fn parse(input: &str) -> Result<Self, RecoveryCodeError> {
        let mut symbols = input
            .bytes()
            .filter(|byte| *byte != b'-' && !byte.is_ascii_whitespace())
            .map(|byte| byte.to_ascii_uppercase());
        let mut code = Zeroizing::new([0; RECOVERY_CODE_LEN]);
        for slot in code.iter_mut() {
            *slot = symbols
                .next()
                .filter(|symbol| RECOVERY_CODE_ALPHABET.contains(symbol))
                .ok_or(RecoveryCodeError)?;
        }
        if symbols.next().is_some() {
            return Err(RecoveryCodeError);
        }
        Ok(Self(code))
    }

    /// The canonical symbols, without dashes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// The code as a person copies it, in dash-separated groups.
    #[must_use]
    pub fn reveal(&self) -> Zeroizing<String> {
        let mut grouped = Zeroizing::new(String::with_capacity(
            RECOVERY_CODE_LEN + RECOVERY_CODE_GROUPS.len() - 1,
        ));
        let mut rest = self.as_bytes();
        for (index, len) in RECOVERY_CODE_GROUPS.into_iter().enumerate() {
            if index > 0 {
                grouped.push('-');
            }
            let (group, tail) = rest.split_at(len);
            grouped.extend(group.iter().map(|&symbol| char::from(symbol)));
            rest = tail;
        }
        grouped
    }
}

impl fmt::Debug for RecoveryCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RecoveryCode(..)")
    }
}

impl Serialize for RecoveryCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.reveal())
    }
}

impl<'de> Deserialize<'de> for RecoveryCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Zeroizing::new(String::deserialize(deserializer)?);
        Self::parse(&raw).map_err(serde::de::Error::custom)
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
    /// The Cluster lost its key: no Machine may generate another until the one it
    /// published, `public_key`, is recovered.
    AwaitsRecovery {
        public_key: SealingPublicKey,
    },
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
        let awaits = Sealing::AwaitsRecovery {
            public_key: SealingPublicKey::from_bytes([2; 32]),
        };
        assert_eq!(
            serde_json::to_value(awaits).unwrap(),
            serde_json::json!({ "kind": "awaits_recovery", "public_key": STANDARD.encode([2; 32]) })
        );
    }

    #[test]
    fn sealed_key_debug_is_redacted() {
        let sealed = SealedSealingKey::from_bytes(vec![9; 48]);
        assert_eq!(format!("{sealed:?}"), "SealedSealingKey(..)");
    }

    #[test]
    fn recovery_code_reads_back_from_its_grouped_form() {
        let code = RecoveryCode::generate();
        let grouped = code.reveal();
        assert_eq!(grouped.len(), 30);
        assert_eq!(
            grouped.split('-').map(str::len).collect::<Vec<_>>(),
            [5, 5, 5, 5, 6]
        );
        assert_eq!(RecoveryCode::parse(&grouped).unwrap(), code);
        assert_eq!(
            RecoveryCode::parse(&format!(" {} \n", grouped.to_lowercase())).unwrap(),
            code
        );
        assert_ne!(RecoveryCode::generate(), code);
    }

    #[test]
    fn recovery_code_refuses_wrong_length_or_symbols() {
        let valid = "ABCDE-FGHIJ-KLMNO-PQRST-UVWXYZ";
        assert!(RecoveryCode::parse(valid).is_ok());
        assert!(RecoveryCode::parse("ABCDE-FGHIJ-KLMNO-PQRST-UVWXY").is_err());
        assert!(RecoveryCode::parse("ABCDE-FGHIJ-KLMNO-PQRST-UVWXYZ2").is_err());
        assert!(RecoveryCode::parse("ABCDE-FGHIJ-KLMNO-PQRST-UVWXY1").is_err());
        assert!(RecoveryCode::parse("").is_err());
        let error = RecoveryCode::parse("ABCDE-FGHIJ-KLMNO-PQRST-UVWXY0").unwrap_err();
        assert!(!error.to_string().contains("ABCDE"));
    }

    #[test]
    fn recovery_code_and_backup_round_trip_without_leaking() {
        let code = RecoveryCode::generate();
        assert_eq!(format!("{code:?}"), "RecoveryCode(..)");
        assert_eq!(
            serde_json::to_value(&code).unwrap(),
            serde_json::json!(code.reveal().as_str())
        );
        assert_eq!(
            serde_json::from_value::<RecoveryCode>(serde_json::to_value(&code).unwrap()).unwrap(),
            code
        );
        let refused = serde_json::from_str::<RecoveryCode>("\"ABCDE-FGHIJ-KLMNO-PQRST-UVWXY0\"")
            .unwrap_err()
            .to_string();
        assert!(!refused.contains("ABCDE"), "{refused}");
        let backup = SealingKeyBackup::from_bytes(vec![3; 77]);
        assert_eq!(format!("{backup:?}"), "SealingKeyBackup(..)");
        let json = serde_json::to_string(&backup).unwrap();
        assert_eq!(
            serde_json::from_str::<SealingKeyBackup>(&json).unwrap(),
            backup
        );
    }
}
