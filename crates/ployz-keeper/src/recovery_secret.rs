//! Wrap the cluster CA signing key with an operator recovery passphrase.
//!
//! The disposable core cannot hand a promotion candidate the CA key at promote
//! time (it is dead), so the key is pre-positioned on Reachable Machines — but
//! **encrypted** (ADR 0031): a machine compromise yields only ciphertext, and
//! `core-promote` decrypts the local copy with the operator's recovery secret.
//!
//! Wire layout is self-describing so unwrap needs only the passphrase:
//! `version(1) || salt(16) || nonce(12) || ChaCha20-Poly1305(ciphertext||tag)`,
//! with the key derived from the passphrase via Argon2id over the stored salt
//! using the parameters pinned for that version.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use rand::RngCore;
use rand::rngs::OsRng;
use zeroize::Zeroizing;

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// Format version, stored as the first byte so the KDF parameters are pinned
/// *per wrapped blob*. Without this, a future change to the crate's default
/// Argon2 cost would re-derive a different key and leave every already-wrapped
/// CA key permanently undecryptable — the "lose it and there's no promotion"
/// failure, silently. New versions add a match arm; old blobs keep decrypting.
const RECOVERY_FORMAT_V1: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum RecoverySecretError {
    #[error("recovery-secret key derivation failed")]
    KeyDerivation,
    #[error("recovery-secret encryption failed")]
    Encrypt,
    #[error("wrong recovery secret, or the wrapped key is corrupt")]
    Decrypt,
    #[error("wrapped recovery material is malformed")]
    Malformed,
}

/// Encrypt `plaintext` (the CA key PEM) under `passphrase`. Each call uses a
/// fresh random salt and nonce, so wrapping the same key twice yields different
/// output.
pub fn wrap(passphrase: &str, plaintext: &[u8]) -> Result<Vec<u8>, RecoverySecretError> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);

    let key = derive_key_v1(passphrase, &salt)?;
    let ciphertext = ChaCha20Poly1305::new(Key::from_slice(&key[..]))
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| RecoverySecretError::Encrypt)?;

    let mut out = Vec::with_capacity(1 + SALT_LEN + NONCE_LEN + ciphertext.len());
    out.push(RECOVERY_FORMAT_V1);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt what [`wrap`] produced. Returns [`RecoverySecretError::Decrypt`] for
/// the wrong passphrase or any tampering (the AEAD tag fails), and
/// [`RecoverySecretError::Malformed`] for a truncated blob or an unknown format
/// version.
pub fn unwrap(passphrase: &str, wrapped: &[u8]) -> Result<Vec<u8>, RecoverySecretError> {
    let Some((&version, body)) = wrapped.split_first() else {
        return Err(RecoverySecretError::Malformed);
    };
    match version {
        RECOVERY_FORMAT_V1 => unwrap_v1(passphrase, body),
        _ => Err(RecoverySecretError::Malformed),
    }
}

fn unwrap_v1(passphrase: &str, body: &[u8]) -> Result<Vec<u8>, RecoverySecretError> {
    if body.len() < SALT_LEN + NONCE_LEN {
        return Err(RecoverySecretError::Malformed);
    }
    let (salt, rest) = body.split_at(SALT_LEN);
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);

    let key = derive_key_v1(passphrase, salt)?;
    ChaCha20Poly1305::new(Key::from_slice(&key[..]))
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| RecoverySecretError::Decrypt)
}

/// Argon2id with **pinned** parameters (19 MiB, 3 passes, 1 lane) — never
/// `Argon2::default()`, whose values could shift under us and break already-
/// wrapped blobs. The derived key is zeroized on drop.
fn derive_key_v1(
    passphrase: &str,
    salt: &[u8],
) -> Result<Zeroizing<[u8; KEY_LEN]>, RecoverySecretError> {
    let params =
        Params::new(19_456, 3, 1, Some(KEY_LEN)).map_err(|_| RecoverySecretError::KeyDerivation)?;
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut key[..])
        .map_err(|_| RecoverySecretError::KeyDerivation)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_then_unwrap_round_trips() {
        let secret = b"-----BEGIN PRIVATE KEY-----\ncluster-ca\n-----END PRIVATE KEY-----";
        let wrapped = wrap("correct horse battery staple", secret).expect("wrap");
        let recovered = unwrap("correct horse battery staple", &wrapped).expect("unwrap");
        assert_eq!(recovered, secret);
    }

    #[test]
    fn the_wrong_passphrase_does_not_decrypt() {
        let wrapped = wrap("right", b"ca-key").expect("wrap");
        assert!(matches!(
            unwrap("wrong", &wrapped),
            Err(RecoverySecretError::Decrypt)
        ));
    }

    #[test]
    fn tampering_is_rejected() {
        let mut wrapped = wrap("secret", b"ca-key").expect("wrap");
        *wrapped.last_mut().expect("non-empty") ^= 0xff;
        assert!(matches!(
            unwrap("secret", &wrapped),
            Err(RecoverySecretError::Decrypt)
        ));
    }

    #[test]
    fn each_wrap_is_unique_and_malformed_input_is_rejected() {
        // Fresh salt + nonce per wrap.
        assert_ne!(
            wrap("s", b"ca-key").expect("a"),
            wrap("s", b"ca-key").expect("b")
        );
        // Empty, a v1 blob too short to hold salt+nonce, and an unknown version.
        assert!(matches!(
            unwrap("s", b""),
            Err(RecoverySecretError::Malformed)
        ));
        assert!(matches!(
            unwrap("s", &[RECOVERY_FORMAT_V1, 0, 0]),
            Err(RecoverySecretError::Malformed)
        ));
        assert!(matches!(
            unwrap("s", &[0xff, 1, 2, 3]),
            Err(RecoverySecretError::Malformed)
        ));
    }

    #[test]
    fn wrapped_blobs_carry_the_format_version() {
        assert_eq!(
            wrap("s", b"ca-key").expect("wrap").first().copied(),
            Some(RECOVERY_FORMAT_V1)
        );
    }
}
