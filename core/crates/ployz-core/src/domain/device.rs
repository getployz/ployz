//! Devices: user identities a Machine admits at Full scope, named by their key.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::{ManagementClientLabel, ValueError};

const LABEL_PREFIX: &str = "device-";
const CODE_LEN: usize = 10;
const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// A Device's management public key. The Device holds the secret; a Machine stores
/// only this key, in the slot its [`DeviceCode`] names.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct DeviceKey([u8; 32]);

impl DeviceKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub fn code(&self) -> DeviceCode {
        DeviceCode::of(self)
    }

    /// The only slot label a Machine admits this key under at Full scope.
    #[must_use]
    pub fn label(&self) -> ManagementClientLabel {
        self.code().label()
    }

    /// Parse the 64-digit lowercase hex form.
    ///
    /// # Errors
    /// Returns a [`ValueError`] when `value` is not 64 lowercase hex digits.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        if !crate::value::is_lower_hex(value, 64) {
            return Err(ValueError::new(
                "Device key",
                value,
                "64 lowercase hex digits",
            ));
        }
        let mut bytes = [0; 32];
        for (byte, pair) in bytes.iter_mut().zip(value.as_bytes().chunks(2)) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).expect("ascii hex"), 16)
                .expect("checked lowercase hex");
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for DeviceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

impl fmt::Debug for DeviceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "DeviceKey({})", self.code())
    }
}

impl TryFrom<String> for DeviceKey {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<DeviceKey> for String {
    fn from(key: DeviceKey) -> Self {
        key.to_string()
    }
}

/// A Device's name: the first 10 base32 characters of the SHA-256 of its key. The
/// approver checks a key fetched through Cloud against the code the Device printed,
/// so Cloud can't substitute a key without finding a 50-bit collision.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct DeviceCode([u8; CODE_LEN]);

impl DeviceCode {
    #[must_use]
    pub fn of(key: &DeviceKey) -> Self {
        let digest = Sha256::digest(key.as_bytes());
        let bits = u64::from_be_bytes(
            *digest
                .as_slice()
                .first_chunk()
                .expect("a SHA-256 digest is 32 bytes"),
        );
        let mut code = [0; CODE_LEN];
        for (index, char) in code.iter_mut().enumerate() {
            let shift = 64 - 5 * (index + 1);
            let digit = usize::try_from((bits >> shift) & 31).expect("five bits");
            *char = *BASE32.get(digit).expect("five bits index 32 digits");
        }
        Self(code)
    }

    /// Parse a code as a person types it: case and surrounding space don't matter.
    ///
    /// # Errors
    /// Returns a [`ValueError`] unless `value` is 10 base32 characters.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let normalized = value.trim().to_ascii_lowercase();
        let code: [u8; CODE_LEN] = normalized
            .as_bytes()
            .try_into()
            .ok()
            .filter(|code: &[u8; CODE_LEN]| code.iter().all(|byte| BASE32.contains(byte)))
            .ok_or_else(|| ValueError::new("Device code", value, "10 base32 characters"))?;
        Ok(Self(code))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("base32 is ascii")
    }

    #[must_use]
    pub fn label(&self) -> ManagementClientLabel {
        ManagementClientLabel::parse(format!("{LABEL_PREFIX}{self}"))
            .expect("device- and base32 form a valid label")
    }

    /// The code a `device-` label names, or `None` for any other label.
    #[must_use]
    pub fn named_by(label: &ManagementClientLabel) -> Option<Self> {
        label
            .as_str()
            .strip_prefix(LABEL_PREFIX)
            .and_then(|code| Self::parse(code).ok())
    }

    /// Whether `label` is in the Device namespace, which only a Full caller may write.
    #[must_use]
    pub fn reserves(label: &ManagementClientLabel) -> bool {
        label.as_str().starts_with(LABEL_PREFIX)
    }
}

impl fmt::Display for DeviceCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for DeviceCode {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<DeviceCode> for String {
    fn from(code: DeviceCode) -> Self {
        code.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_the_first_fifty_bits_of_the_key_digest() {
        let key = DeviceKey::from_bytes([7; 32]);
        let digest = Sha256::digest([7; 32]);
        let bits: Vec<usize> = digest
            .iter()
            .flat_map(|byte| {
                (0..8)
                    .rev()
                    .map(move |shift| usize::from(byte >> shift & 1))
            })
            .take(50)
            .collect();
        let expected: String = bits
            .chunks(5)
            .map(|chunk| {
                let digit = chunk.iter().fold(0, |value, bit| value << 1 | bit);
                char::from(*BASE32.get(digit).unwrap())
            })
            .collect();
        assert_eq!(key.code().as_str(), expected);
        assert_eq!(key.label().as_str(), format!("device-{expected}"));
    }

    #[test]
    fn code_parses_as_typed_and_refuses_anything_else() {
        let code = DeviceKey::from_bytes([1; 32]).code();
        let shouted = format!("  {}\n", code.as_str().to_ascii_uppercase());
        assert_eq!(DeviceCode::parse(&shouted).unwrap(), code);
        for invalid in ["", "abcdefghi", "abcdefghijk", "abcdefghi1", "abcdefghi-"] {
            assert!(DeviceCode::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn only_a_canonical_device_label_names_a_code() {
        let code = DeviceKey::from_bytes([2; 32]).code();
        assert_eq!(DeviceCode::named_by(&code.label()), Some(code));
        for other in ["cloud", "cli-abc", "device-", "device-abc"] {
            let label = ManagementClientLabel::parse(other).unwrap();
            assert_eq!(DeviceCode::named_by(&label), None, "{other}");
        }
        assert!(DeviceCode::reserves(
            &ManagementClientLabel::parse("device-abc").unwrap()
        ));
        assert!(!DeviceCode::reserves(
            &ManagementClientLabel::parse("cli-abc").unwrap()
        ));
    }

    #[test]
    fn key_round_trips_as_hex() {
        let key = DeviceKey::from_bytes([0xab; 32]);
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, format!("\"{}\"", "ab".repeat(32)));
        assert_eq!(serde_json::from_str::<DeviceKey>(&json).unwrap(), key);
        assert!(serde_json::from_str::<DeviceKey>("\"AB\"").is_err());
    }
}
