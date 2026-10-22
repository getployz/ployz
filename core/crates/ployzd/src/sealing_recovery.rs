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
const HEADER_LEN: usize = 1 + 32;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const SEALED_KEY_LEN: usize = 32 + 16;
const BACKUP_LEN: usize = HEADER_LEN + SALT_LEN + NONCE_LEN + SEALED_KEY_LEN;

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
    seal(&key.public_key(), key.secret(), code, salt, nonce)
}

/// Opens `backup` with `code`, only if it holds the key behind `expected`.
pub(crate) fn restore(
    backup: &SealingKeyBackup,
    code: &RecoveryCode,
    expected: &SealingPublicKey,
) -> Result<SealingKey, RestoreError> {
    let bytes = backup.as_bytes();
    if bytes.len() != BACKUP_LEN {
        return Err(RestoreError::Malformed);
    }
    let (header, rest) = bytes.split_at(HEADER_LEN);
    let (&version, public_key) = header.split_first().ok_or(RestoreError::Malformed)?;
    if version != VERSION {
        return Err(RestoreError::Malformed);
    }
    if public_key != expected.as_bytes() {
        return Err(RestoreError::OtherKey);
    }
    let (salt, rest) = rest.split_at(SALT_LEN);
    let (nonce, sealed) = rest.split_at(NONCE_LEN);
    let opened = Zeroizing::new(
        cipher(code, salt)
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: header,
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
    public_key: &SealingPublicKey,
    secret: &[u8; 32],
    code: &RecoveryCode,
    salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
) -> SealingKeyBackup {
    let mut backup = Vec::with_capacity(BACKUP_LEN);
    backup.push(VERSION);
    backup.extend_from_slice(public_key.as_bytes());
    let sealed = cipher(code, &salt)
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: secret,
                aad: &backup,
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
    fn backup_of_another_key_is_refused_before_the_code_is_tried() {
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
    fn backup_whose_header_names_another_key_does_not_open() {
        let key = SealingKey::generate();
        let other = SealingKey::generate().public_key();
        let code = RecoveryCode::generate();
        let mut bytes = back_up(&key, &code).as_bytes().to_vec();
        bytes[1..HEADER_LEN].copy_from_slice(other.as_bytes());

        assert_eq!(
            restore(&SealingKeyBackup::from_bytes(bytes), &code, &other),
            Err(RestoreError::WrongCode)
        );
    }

    #[test]
    fn backup_whose_key_does_not_match_its_header_is_refused() {
        let key = SealingKey::generate();
        let other = SealingKey::generate().public_key();
        let code = RecoveryCode::generate();
        let backup = seal(&other, key.secret(), &code, [1; SALT_LEN], [2; NONCE_LEN]);

        assert_eq!(restore(&backup, &code, &other), Err(RestoreError::OtherKey));
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
        let backup = seal(
            &key.public_key(),
            key.secret(),
            &code,
            [1; SALT_LEN],
            [2; NONCE_LEN],
        );

        assert_eq!(
            String::from(backup.clone()),
            "ARO+T+rq8gTH/TNY/JwAchiB0XQngSgifsZ0839/6XttAQEBAQEBAQEBAQEBAQEBAQICAgICAgICAgICAqgnQknOS0MMQdRvIgutKtesqdM1NWvlC9drQ7pO+XTPIWVHT8ynCpV1hK3hZbM96g=="
        );
        assert_eq!(restore(&backup, &code, &key.public_key()), Ok(key));
    }
}
