//! Sealing: secret variable values are stored only as AES-256-GCM ciphertext, and
//! compared by an HMAC fingerprint. Byte-compatible with Cloud's TypeScript sealing
//! in both directions: the key is SHA-256 of the injected secret, the IV 12 random
//! bytes, the tag 16 bytes, each base64; fingerprints are `v1:` and hex.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ployz_core::RpcError;
use ployz_core::config::{EncryptedSecretValue, ValuePart};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::digest::{SHA256, digest};
use ring::hmac;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::json;

use crate::error;

const FINGERPRINT_VERSION: &str = "v1";
const FINGERPRINT_KEY_PURPOSE: &[u8] = b"ployz:variable-value-fingerprint:v1";
const SEALED_FINGERPRINT_PURPOSE: &[u8] = b"ployz:sealed-variable-value:v1";

/// The key the Store seals secrets with, derived from an injected secret: Cloud's
/// encryption secret in Cloud, a local key file in the hidden test mode.
#[derive(Clone)]
pub struct SealingKey {
    cipher: [u8; 32],
    fingerprint: hmac::Key,
}

impl SealingKey {
    /// Derive the sealing and fingerprint keys from `secret`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for an empty secret.
    pub fn new(secret: &[u8]) -> Result<Self, RpcError> {
        if secret.is_empty() {
            return Err(error::invalid("The sealing secret is empty", json!({})));
        }
        let hash = |parts: &[&[u8]]| {
            let mut context = ring::digest::Context::new(&SHA256);
            for part in parts {
                context.update(part);
            }
            context.finish()
        };
        let cipher = hash(&[secret]).as_ref().try_into().expect("SHA-256 is 32 bytes");
        let fingerprint = hash(&[FINGERPRINT_KEY_PURPOSE, b"\0", secret]);
        Ok(Self {
            cipher,
            fingerprint: hmac::Key::new(hmac::HMAC_SHA256, fingerprint.as_ref()),
        })
    }

    fn aead(&self) -> LessSafeKey {
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &self.cipher).expect("a 32-byte key"))
    }

    /// Seal `plaintext` under a fresh random IV.
    pub(crate) fn seal(&self, plaintext: &str) -> EncryptedSecretValue {
        let mut iv = [0; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut iv)
            .expect("the system random source works");
        let mut sealed = plaintext.as_bytes().to_vec();
        let tag = self
            .aead()
            .seal_in_place_separate_tag(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut sealed)
            .expect("AES-GCM seals any plaintext this size");
        EncryptedSecretValue {
            version: 1,
            iv: STANDARD.encode(iv),
            tag: STANDARD.encode(tag.as_ref()),
            ciphertext: STANDARD.encode(sealed),
        }
    }

    /// Open a sealed value.
    ///
    /// # Errors
    /// Returns `internal` when it was sealed under another key or was altered.
    pub(crate) fn open(&self, sealed: &EncryptedSecretValue) -> Result<String, RpcError> {
        let unreadable = || error::internal("A sealed secret could not be opened with this Store's key");
        let decode = |text: &str| STANDARD.decode(text).map_err(|_| unreadable());
        let iv: [u8; NONCE_LEN] = decode(&sealed.iv)?.try_into().map_err(|_| unreadable())?;
        let tag = decode(&sealed.tag)?;
        if sealed.version != 1 || tag.len() != AES_256_GCM.tag_len() {
            return Err(unreadable());
        }
        let mut buffer = decode(&sealed.ciphertext)?;
        buffer.extend(tag);
        let plaintext = self
            .aead()
            .open_in_place(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut buffer)
            .map_err(|_| unreadable())?;
        String::from_utf8(plaintext.to_vec()).map_err(|_| unreadable())
    }

    /// The fingerprint a secret is compared by, without revealing it.
    pub(crate) fn fingerprint(&self, plaintext: &str) -> String {
        let mut context = hmac::Context::with_key(&self.fingerprint);
        context.update(SEALED_FINGERPRINT_PURPOSE);
        context.update(b"\0");
        context.update(plaintext.as_bytes());
        format!("{FINGERPRINT_VERSION}:{}", hex::encode(context.sign()))
    }
}

impl fmt::Debug for SealingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SealingKey(..)")
    }
}

/// The fingerprint a plain or template value is compared by.
pub(crate) fn plain_fingerprint(parts: &[ValuePart]) -> String {
    let parts = serde_json::to_string(parts).expect("template parts are JSON");
    let document = serde_json::to_string(&json!({ "kind": "plain", "value": parts }))
        .expect("a fingerprint document is JSON");
    format!(
        "{FINGERPRINT_VERSION}:{}",
        hex::encode(digest(&SHA256, document.as_bytes()))
    )
}

#[cfg(test)]
mod tests {
    use ployz_core::config::ValuePartOwner;

    use super::*;

    fn key() -> SealingKey {
        SealingKey::new(b"test-encryption-secret").unwrap()
    }

    fn sealed(iv: &str, tag: &str, ciphertext: &str) -> EncryptedSecretValue {
        EncryptedSecretValue {
            version: 1,
            iv: iv.into(),
            tag: tag.into(),
            ciphertext: ciphertext.into(),
        }
    }

    // Sealed by Cloud's `encryptSecretValue` and fingerprinted by
    // `getSealedVariableValueFingerprint`, with the secret above.
    #[test]
    fn opens_what_cloud_sealed_and_fingerprints_alike() {
        let vectors = [
            (
                "postgres://u:p@db/app",
                sealed(
                    "P06pur97O+q8KRa4",
                    "WX+lTmeP37rpdHHvD6iOEQ==",
                    "LcZhKYl1ZWoTpzn9xXoetYCxr8Iy",
                ),
                "v1:84deeccda72e54030ebff34a05c985585fa26b1f6e313d5b826f7c1ad916bfdc",
            ),
            (
                "pässwörd 🔑 秘密",
                sealed(
                    "dvr2HB3T+b+uJ7Vc",
                    "/hfvTvSwVTkQcbCzaF/6PA==",
                    "w6fyakteWAS4cdfEBcv9DGk404imqw==",
                ),
                "v1:a5a478b6d3b77dab18f838ae270d88f36135d92dbe6a96423554f13f89db664a",
            ),
        ];
        for (plaintext, sealed, fingerprint) in vectors {
            assert_eq!(key().open(&sealed).unwrap(), plaintext);
            assert_eq!(key().fingerprint(plaintext), fingerprint);
        }
    }

    #[test]
    fn refuses_a_corrupt_tag_or_another_key() {
        let value = key().seal("pässwörd 🔑");
        assert_eq!(key().open(&value).unwrap(), "pässwörd 🔑");
        assert_ne!(key().seal("pässwörd 🔑"), value, "each seal has its own IV");
        let mut corrupt = value.clone();
        corrupt.tag = STANDARD.encode([0_u8; 16]);
        assert_eq!(
            key().open(&corrupt).unwrap_err().code,
            ployz_core::RpcErrorCode::Internal
        );
        assert!(SealingKey::new(b"other").unwrap().open(&value).is_err());
        assert!(SealingKey::new(b"").is_err());
        assert_eq!(format!("{:?}", key()), "SealingKey(..)");
    }

    // `getPlainVariableValueFingerprint(JSON.stringify(parts))` for the same parts.
    #[test]
    fn plain_fingerprints_match_cloud() {
        let parts = [
            ValuePart::Text {
                value: "x=é ".into(),
            },
            ValuePart::Ref {
                owner: ValuePartOwner::Service {
                    lineage_id: "6f1c2b1e-8d4a-4c1e-9a53-0b7e2f9d1a11".into(),
                },
                key: "URL".into(),
            },
            ValuePart::Ref {
                owner: ValuePartOwner::Self_,
                key: "K".into(),
            },
        ];
        assert_eq!(
            plain_fingerprint(&parts),
            "v1:1178fad2c71fa518e32b95bdfa55300479c9a61cacf8aa29bac99e2115fe5fb3"
        );
    }
}
