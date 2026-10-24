//! The Sealing Key wrapped under a recovery code, so a Cluster that lost every
//! holder can open its sealed values again.

use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit as _, Nonce,
    aead::{Aead as _, Payload},
};
use hkdf::Hkdf;
use ployz_core::{RecoveryCode, SealingKeyBackup, SealingPublicKey};
use rand_core::{OsRng, RngCore as _, TryRngCore as _};
use sha2_0_10::Sha256;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::sealing_key::SealingKey;

const VERSION: u8 = 1;
const INFO: &[u8] = b"ployz-sealing-key-backup-v1";
const HEADER: [u8; 1] = [VERSION];
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const SEALED_KEY_LEN: usize = 32 + 16;
const BACKUP_LEN: usize = HEADER.len() + SALT_LEN + NONCE_LEN + SEALED_KEY_LEN;

#[derive(Debug, Eq, Error, PartialEq)]
pub(crate) enum RestoreError {
    #[error("the recovery backup is malformed")]
    Malformed,
    #[error("the recovery backup belongs to a different Sealing Key")]
    OtherKey,
    #[error("the recovery code does not open this backup")]
    WrongCode,
}

pub(crate) fn back_up(key: &SealingKey, code: &RecoveryCode) -> SealingKeyBackup {
    let mut rng = OsRng.unwrap_err();
    let mut salt = [0; SALT_LEN];
    rng.fill_bytes(&mut salt);
    let mut nonce = [0; NONCE_LEN];
    rng.fill_bytes(&mut nonce);
    seal(key.secret(), code, salt, nonce)
}

/// Opens `backup` with `code`, only if it holds the key behind `expected`.
pub(crate) fn restore(
    backup: &SealingKeyBackup,
    code: &RecoveryCode,
    expected: &SealingPublicKey,
) -> Result<SealingKey, RestoreError> {
    let bytes = backup.as_bytes();
    let Some(rest) = bytes
        .strip_prefix(&HEADER)
        .filter(|_| bytes.len() == BACKUP_LEN)
    else {
        return Err(RestoreError::Malformed);
    };
    let (salt, rest) = rest.split_at(SALT_LEN);
    let (nonce, sealed) = rest.split_at(NONCE_LEN);
    let opened = Zeroizing::new(
        cipher(code, salt)
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: &HEADER,
                },
            )
            .map_err(|_| RestoreError::WrongCode)?,
    );
    let key = <[u8; 32]>::try_from(opened.as_slice())
        .map(SealingKey::from_secret)
        .map_err(|_| RestoreError::Malformed)?;
    if key.public_key() == *expected {
        Ok(key)
    } else {
        Err(RestoreError::OtherKey)
    }
}

fn seal(
    secret: &[u8; 32],
    code: &RecoveryCode,
    salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
) -> SealingKeyBackup {
    let mut backup = Vec::with_capacity(BACKUP_LEN);
    backup.extend_from_slice(&HEADER);
    let sealed = cipher(code, &salt)
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: secret,
                aad: &HEADER,
            },
        )
        .expect("ChaCha20-Poly1305 seals a 32-byte key");
    backup.extend_from_slice(&salt);
    backup.extend_from_slice(&nonce);
    backup.extend(sealed);
    SealingKeyBackup::from_bytes(backup)
}

fn cipher(code: &RecoveryCode, salt: &[u8]) -> ChaCha20Poly1305 {
    let mut key = Zeroizing::new([0; 32]);
    Hkdf::<Sha256>::new(Some(salt), code.as_bytes())
        .expand(INFO, key.as_mut_slice())
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    ChaCha20Poly1305::new(Key::from_slice(key.as_slice()))
}

#[cfg(test)]
mod tests {
    use sha2_0_10::Digest as _;

    use super::*;

    #[test]
    fn backup_restores_with_its_code() {
        let key = SealingKey::generate();
        let code = RecoveryCode::generate();
        let backup = back_up(&key, &code);

        assert_eq!(backup.as_bytes().len(), BACKUP_LEN);
        assert_eq!(restore(&backup, &code, &key.public_key()), Ok(key));
    }

    #[test]
    fn backup_refuses_another_code() {
        let key = SealingKey::generate();
        let backup = back_up(&key, &RecoveryCode::generate());

        assert_eq!(
            restore(&backup, &RecoveryCode::generate(), &key.public_key()),
            Err(RestoreError::WrongCode)
        );
    }

    #[test]
    fn backup_of_another_key_is_refused() {
        let code = RecoveryCode::generate();
        let backup = back_up(&SealingKey::generate(), &code);

        assert_eq!(
            restore(&backup, &code, &SealingKey::generate().public_key()),
            Err(RestoreError::OtherKey)
        );
    }

    #[test]
    #[expect(
        clippy::indexing_slicing,
        reason = "tampers with a fixed-length backup"
    )]
    fn malformed_backup_is_refused() {
        let key = SealingKey::generate();
        let code = RecoveryCode::generate();
        let bytes = back_up(&key, &code).as_bytes().to_vec();
        let mut other_version = bytes.clone();
        other_version[0] = 2;

        for bytes in [
            Vec::new(),
            bytes[..BACKUP_LEN - 1].to_vec(),
            [bytes.as_slice(), &[0]].concat(),
            other_version,
        ] {
            assert_eq!(
                restore(
                    &SealingKeyBackup::from_bytes(bytes),
                    &code,
                    &key.public_key()
                ),
                Err(RestoreError::Malformed)
            );
        }
    }

    #[test]
    fn backup_hides_the_key_and_the_code() {
        let key = SealingKey::generate();
        let code = RecoveryCode::generate();
        let backup = back_up(&key, &code);

        for secret in [key.secret().as_slice(), code.as_bytes()] {
            assert!(
                !backup
                    .as_bytes()
                    .windows(secret.len())
                    .any(|window| window == secret)
            );
        }
        assert_ne!(back_up(&key, &code), backup);
    }

    #[test]
    fn backup_format_stays_readable() {
        let key = SealingKey::from_secret([7; 32]);
        let code = RecoveryCode::parse("ABCDE-FGHIJ-KLMNO-PQRST-UVWXYZ").unwrap();
        let backup = seal(key.secret(), &code, [1; SALT_LEN], [2; NONCE_LEN]);

        // Cloud's RecoveryBackup and recoveryDigest tests pin the same vector.
        assert_eq!(
            String::from(backup.clone()),
            "AQEBAQEBAQEBAQEBAQEBAQECAgICAgICAgICAgKoJ0JJzktDDEHUbyILrSrXrKnTNTVr5QvXa0O6Tvl0z02w36IqI7Bc1vhSDy0Yud8="
        );
        assert_eq!(
            hex::encode(Sha256::digest(backup.as_bytes())),
            "4ef804f83a444658d5336274cc7ad9fe8af91a47b0d3bfb903e98965439926e2"
        );
        assert_eq!(restore(&backup, &code, &key.public_key()), Ok(key));
    }
}
