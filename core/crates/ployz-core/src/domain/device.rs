//! Devices: user identities a Machine admits at Full scope, named by their key.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::{ManagementClientLabel, ValueError};

const LABEL_PREFIX: &str = "device-";
const CODE_LEN: usize = 10;
const CHECK_LEN: usize = 20;
const CHECK_GROUP: usize = 5;
const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// The first `N` base32 digits of the SHA-256 of `bytes`.
fn digest_digits<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let digest = Sha256::digest(bytes);
    let bits = u128::from_be_bytes(
        *digest
            .as_slice()
            .first_chunk()
            .expect("a SHA-256 digest is 32 bytes"),
    );
    let mut digits = [0; N];
    for (index, digit) in digits.iter_mut().enumerate() {
        let shift = 128 - 5 * (index + 1);
        let value = usize::try_from((bits >> shift) & 31).expect("five bits");
        *digit = *BASE32.get(value).expect("five bits index 32 digits");
    }
    digits
}

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

    #[must_use]
    pub fn check_code(&self) -> CheckCode {
        CheckCode::hashing(self.as_bytes())
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

/// A Device's or passkey's name: the first 10 base32 characters of the SHA-256 of its
/// key, and the suffix of its slot label. 50 bits name a key but can't vouch for one:
/// Cloud can grind a second key with the same name, so a person approves by
/// [`CheckCode`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct DeviceCode([u8; CODE_LEN]);

impl DeviceCode {
    #[must_use]
    pub fn of(key: &DeviceKey) -> Self {
        Self::hashing(key.as_bytes())
    }

    /// The code of a key written as `bytes`.
    #[must_use]
    pub fn hashing(bytes: &[u8]) -> Self {
        Self(digest_digits(bytes))
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

/// The code a person compares before a key is trusted: the first 20 base32 characters of
/// the SHA-256 of the key, 100 bits, so Cloud can't grind a second key that matches it.
/// Its first 10 are the key's [`DeviceCode`]. A person reads it in groups of five.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct CheckCode([u8; CHECK_LEN]);

impl CheckCode {
    /// The check code of a key written as `bytes`.
    #[must_use]
    pub fn hashing(bytes: &[u8]) -> Self {
        Self(digest_digits(bytes))
    }

    /// Parse a code as a person pastes or types it: case, dashes and spaces don't matter.
    ///
    /// # Errors
    /// Returns a [`ValueError`] unless `value` holds exactly 20 base32 characters.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let mut symbols = value
            .bytes()
            .filter(|byte| *byte != b'-' && !byte.is_ascii_whitespace())
            .map(|byte| byte.to_ascii_lowercase());
        let refused = || {
            ValueError::new(
                "Device code",
                value,
                "20 letters and digits 2 to 7, as `ployz device request` prints them",
            )
        };
        let mut code = [0; CHECK_LEN];
        for slot in &mut code {
            *slot = symbols
                .next()
                .filter(|symbol| BASE32.contains(symbol))
                .ok_or_else(refused)?;
        }
        if symbols.next().is_some() {
            return Err(refused());
        }
        Ok(Self(code))
    }

    /// The name this code begins with.
    #[must_use]
    pub fn code(&self) -> DeviceCode {
        DeviceCode(
            *self
                .0
                .first_chunk()
                .expect("a check code is longer than a name"),
        )
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("base32 is ascii")
    }
}

/// Grouped for reading aloud: `mmg42-klgyq-abcde-fghij`.
impl fmt::Display for CheckCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, group) in self.0.chunks(CHECK_GROUP).enumerate() {
            if index > 0 {
                formatter.write_str("-")?;
            }
            formatter.write_str(std::str::from_utf8(group).expect("base32 is ascii"))?;
        }
        Ok(())
    }
}

impl TryFrom<String> for CheckCode {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<CheckCode> for String {
    fn from(code: CheckCode) -> Self {
        code.as_str().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_the_first_fifty_bits_of_the_key_digest() {
        let key = DeviceKey::from_bytes([7; 32]);
        let expected = digest_bits(&[7; 32], 50);
        assert_eq!(key.code().as_str(), expected);
        assert_eq!(key.label().as_str(), format!("device-{expected}"));
    }

    fn digest_bits(bytes: &[u8], count: usize) -> String {
        Sha256::digest(bytes)
            .iter()
            .flat_map(|byte| {
                (0..8)
                    .rev()
                    .map(move |shift| usize::from(byte >> shift & 1))
            })
            .take(count)
            .collect::<Vec<_>>()
            .chunks(5)
            .map(|chunk| {
                let digit = chunk.iter().fold(0, |value, bit| value << 1 | bit);
                char::from(*BASE32.get(digit).unwrap())
            })
            .collect()
    }

    #[test]
    fn check_code_is_the_first_hundred_bits_and_begins_with_the_name() {
        let key = DeviceKey::from_bytes([7; 32]);
        let digits = digest_bits(&[7; 32], 100);
        let check = key.check_code();
        assert_eq!(String::from(check), digits);
        assert_eq!(check.code(), key.code());
        let grouped = format!(
            "{}-{}-{}-{}",
            &digits[..5],
            &digits[5..10],
            &digits[10..15],
            &digits[15..]
        );
        assert_eq!(check.to_string(), grouped);
        assert_eq!(CheckCode::parse(&grouped).unwrap(), check);
        assert_eq!(
            CheckCode::parse(&format!(
                " {} \n",
                grouped.to_ascii_uppercase().replace('-', " ")
            ))
            .unwrap(),
            check
        );
    }

    #[test]
    fn a_name_alone_is_not_a_check_code() {
        let key = DeviceKey::from_bytes([3; 32]);
        assert!(CheckCode::parse(key.code().as_str()).is_err());
        let check = String::from(key.check_code());
        for invalid in [
            "",
            &check[..19],
            &format!("{check}a"),
            &format!("{}1", &check[..19]),
        ] {
            assert!(CheckCode::parse(invalid).is_err(), "{invalid}");
        }
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
