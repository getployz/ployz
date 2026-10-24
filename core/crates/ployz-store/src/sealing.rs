//! Sealing: registry credentials, and secrets sealed before values were sealed on
//! the client, are stored only as AES-256-GCM ciphertext. Byte-compatible with
//! Cloud's TypeScript sealing in both directions: the key is SHA-256 of the injected
//! secret, the IV 12 random bytes, the tag 16 bytes, each base64. A plain value's
//! fingerprint is `v1:` and hex.

use std::fmt;
use std::io::{ErrorKind, Write as _};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ployz_core::RpcError;
use ployz_core::config::{EncryptedSecretValue, ValuePart};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::json;

use crate::error;

const FINGERPRINT_VERSION: &str = "v1";

/// The key the Store seals registry credentials with, derived from an injected
/// secret: Cloud's encryption secret in Cloud, a local key file in the hidden test
/// mode. It also opens the secrets it sealed before values were sealed on the client.
#[derive(Clone)]
pub struct SealingKey {
    cipher: [u8; 32],
}

impl SealingKey {
    /// Derive the sealing key from `secret`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for an empty secret.
    pub fn new(secret: &[u8]) -> Result<Self, RpcError> {
        if secret.is_empty() {
            return Err(error::invalid("The sealing secret is empty", json!({})));
        }
        let cipher = digest(&SHA256, secret)
            .as_ref()
            .try_into()
            .expect("SHA-256 is 32 bytes");
        Ok(Self { cipher })
    }

    /// The key in the key file at `path`, creating the file with a random secret
    /// first when there is none: the hidden test mode's key.
    ///
    /// # Errors
    /// Returns `unavailable` when the file can't be read or created.
    pub fn from_file(path: &Path) -> Result<Self, RpcError> {
        let unreadable =
            || error::unavailable("The Config Store's key file can't be read or created");
        match std::fs::read(path) {
            Ok(secret) => return Self::new(&secret),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(unreadable()),
        }
        let mut secret = [0; 32];
        SystemRandom::new()
            .fill(&mut secret)
            .expect("the system random source works");
        let secret = hex::encode(secret);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        // ponytail: a concurrent first open may read the file before it is written; retry then.
        match options.open(path) {
            Ok(mut file) => file
                .write_all(secret.as_bytes())
                .map_err(|_| unreadable())?,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => return Self::from_file(path),
            Err(_) => return Err(unreadable()),
        }
        Self::new(secret.as_bytes())
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
        let unreadable =
            || error::internal("A sealed secret could not be opened with this Store's key");
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

    // Sealed by Cloud's `encryptSecretValue` with the secret above.
    #[test]
    fn opens_what_cloud_sealed() {
        let vectors = [
            (
                "postgres://u:p@db/app",
                sealed(
                    "P06pur97O+q8KRa4",
                    "WX+lTmeP37rpdHHvD6iOEQ==",
                    "LcZhKYl1ZWoTpzn9xXoetYCxr8Iy",
                ),
            ),
            (
                "pässwörd 🔑 秘密",
                sealed(
                    "dvr2HB3T+b+uJ7Vc",
                    "/hfvTvSwVTkQcbCzaF/6PA==",
                    "w6fyakteWAS4cdfEBcv9DGk404imqw==",
                ),
            ),
        ];
        for (plaintext, sealed) in vectors {
            assert_eq!(key().open(&sealed).unwrap(), plaintext);
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
