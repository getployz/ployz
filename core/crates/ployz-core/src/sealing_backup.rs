//! A Sealing secret wrapped under a recovery code, so a Cluster that lost every holder can
//! open its sealed values again. The Cluster's identity key and the recipient key an
//! activation adds are both backed up this way.

use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit as _, Nonce,
    aead::{Aead as _, Payload},
};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore as _, TryRngCore as _};
use sha2_0_10::Sha256;
use zeroize::Zeroizing;

use crate::{RecoveryCode, SealingKeyBackup};

const VERSION: u8 = 1;
const INFO: &[u8] = b"ployz-sealing-key-backup-v1";
const HEADER: [u8; 1] = [VERSION];
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const SEALED_KEY_LEN: usize = 32 + 16;
/// The length of every backup this version writes.
pub const BACKUP_LEN: usize = HEADER.len() + SALT_LEN + NONCE_LEN + SEALED_KEY_LEN;

/// Why a backup didn't open.
#[derive(Debug, Eq, thiserror::Error, PartialEq)]
pub enum BackupError {
    #[error("the recovery backup is malformed")]
    Malformed,
    #[error("the recovery code does not open this backup")]
    WrongCode,
}

/// Wrap `secret` under `code`.
#[must_use]
pub fn back_up(secret: &[u8; 32], code: &RecoveryCode) -> SealingKeyBackup {
    let mut rng = OsRng.unwrap_err();
    let mut salt = [0; SALT_LEN];
    rng.fill_bytes(&mut salt);
    let mut nonce = [0; NONCE_LEN];
    rng.fill_bytes(&mut nonce);
    seal(secret, code, salt, nonce)
}

/// The secret `backup` holds, opened with `code`.
///
/// # Errors
/// Returns [`BackupError::Malformed`] for a backup this version can't read and
/// [`BackupError::WrongCode`] when `code` doesn't open it.
pub fn open(
    backup: &SealingKeyBackup,
    code: &RecoveryCode,
) -> Result<Zeroizing<[u8; 32]>, BackupError> {
    let bytes = backup.as_bytes();
    let Some(rest) = bytes
        .strip_prefix(&HEADER)
        .filter(|_| bytes.len() == BACKUP_LEN)
    else {
        return Err(BackupError::Malformed);
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
            .map_err(|_| BackupError::WrongCode)?,
    );
    <[u8; 32]>::try_from(opened.as_slice())
        .map(Zeroizing::new)
        .map_err(|_| BackupError::Malformed)
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
    use sha2::Digest as _;

    use super::*;

    #[test]
    fn backup_opens_with_its_code_only() {
        let code = RecoveryCode::generate();
        let backup = back_up(&[9; 32], &code);

        assert_eq!(backup.as_bytes().len(), BACKUP_LEN);
        assert_eq!(open(&backup, &code).map(|secret| *secret), Ok([9; 32]));
        assert_eq!(
            open(&backup, &RecoveryCode::generate()).map(|secret| *secret),
            Err(BackupError::WrongCode)
        );
    }

    #[test]
    #[expect(
        clippy::indexing_slicing,
        reason = "tampers with a fixed-length backup"
    )]
    fn malformed_backup_is_refused() {
        let code = RecoveryCode::generate();
        let bytes = back_up(&[4; 32], &code).as_bytes().to_vec();
        let mut other_version = bytes.clone();
        other_version[0] = 2;

        for bytes in [
            Vec::new(),
            bytes[..BACKUP_LEN - 1].to_vec(),
            [bytes.as_slice(), &[0]].concat(),
            other_version,
        ] {
            assert_eq!(
                open(&SealingKeyBackup::from_bytes(bytes), &code).map(|secret| *secret),
                Err(BackupError::Malformed)
            );
        }
    }

    #[test]
    fn backup_hides_the_key_and_the_code() {
        let secret = [5; 32];
        let code = RecoveryCode::generate();
        let backup = back_up(&secret, &code);

        for hidden in [secret.as_slice(), code.as_bytes()] {
            assert!(
                !backup
                    .as_bytes()
                    .windows(hidden.len())
                    .any(|window| window == hidden)
            );
        }
        assert_ne!(back_up(&secret, &code), backup);
    }

    #[test]
    fn backup_format_stays_readable() {
        let code = RecoveryCode::parse("ABCDE-FGHIJ-KLMNO-PQRST-UVWXYZ").unwrap();
        let backup = seal(&[7; 32], &code, [1; SALT_LEN], [2; NONCE_LEN]);

        // Cloud's RecoveryBackup and recoveryDigest tests pin the same vector.
        assert_eq!(
            String::from(backup.clone()),
            "AQEBAQEBAQEBAQEBAQEBAQECAgICAgICAgICAgKoJ0JJzktDDEHUbyILrSrXrKnTNTVr5QvXa0O6Tvl0z02w36IqI7Bc1vhSDy0Yud8="
        );
        let digest: String = sha2::Sha256::digest(backup.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            digest,
            "4ef804f83a444658d5336274cc7ad9fe8af91a47b0d3bfb903e98965439926e2"
        );
        assert_eq!(open(&backup, &code).map(|secret| *secret), Ok([7; 32]));
    }
}
