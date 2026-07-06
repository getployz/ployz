//! Wrap the cluster CA signing key with an operator recovery passphrase.
//!
//! The disposable core cannot hand a promotion candidate the CA key at promote
//! time (it is dead), so the key is pre-positioned on Reachable Machines — but
//! **encrypted** (ADR 0031): a machine compromise yields only ciphertext, and
//! `core-promote` decrypts the local copy with the operator's recovery secret.
//!
//! Wire layout is self-describing so unwrap needs only the passphrase:
//! `salt(16) || nonce(12) || ChaCha20-Poly1305(ciphertext||tag)`, with the key
//! derived from the passphrase via Argon2id over the stored salt.

use argon2::Argon2;
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use rand::RngCore;
use rand::rngs::OsRng;

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

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

    let key = derive_key(passphrase, &salt)?;
    let ciphertext = ChaCha20Poly1305::new(Key::from_slice(&key))
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| RecoverySecretError::Encrypt)?;

    let mut out = Vec::with_capacity(SALT_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt what [`wrap`] produced. Returns [`RecoverySecretError::Decrypt`] for
/// the wrong passphrase or any tampering (the AEAD tag fails).
pub fn unwrap(passphrase: &str, wrapped: &[u8]) -> Result<Vec<u8>, RecoverySecretError> {
    if wrapped.len() < SALT_LEN + NONCE_LEN {
        return Err(RecoverySecretError::Malformed);
    }
    let (salt, rest) = wrapped.split_at(SALT_LEN);
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);

    let key = derive_key(passphrase, salt)?;
    ChaCha20Poly1305::new(Key::from_slice(&key))
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| RecoverySecretError::Decrypt)
}

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; KEY_LEN], RecoverySecretError> {
    let mut key = [0u8; KEY_LEN];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
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
        let last = wrapped.len() - 1;
        wrapped[last] ^= 0xff;
        assert!(matches!(
            unwrap("secret", &wrapped),
            Err(RecoverySecretError::Decrypt)
        ));
    }

    #[test]
    fn each_wrap_is_unique_and_short_input_is_malformed() {
        // Fresh salt + nonce per wrap.
        assert_ne!(
            wrap("s", b"ca-key").expect("a"),
            wrap("s", b"ca-key").expect("b")
        );
        assert!(matches!(
            unwrap("s", b"tooshort"),
            Err(RecoverySecretError::Malformed)
        ));
    }
}
