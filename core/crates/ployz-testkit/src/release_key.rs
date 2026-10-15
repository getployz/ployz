//! A fixed minisign key that signs fake releases the way the release workflow signs real ones.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer, SigningKey};
use ployz_core::{MachineVersion, release_signature_comment};

const KEY_ID: [u8; 8] = *b"ployztst";

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

/// The test key in `minisign.pub` form.
#[must_use]
pub fn release_public_key() -> String {
    let mut key = b"Ed".to_vec();
    key.extend_from_slice(&KEY_ID);
    key.extend_from_slice(signing_key().verifying_key().as_bytes());
    format!(
        "untrusted comment: ployz test release key\n{}\n",
        STANDARD.encode(key)
    )
}

/// A legacy (pure Ed25519) minisign signature over `checksums` for `version`.
#[must_use]
pub fn sign_release(checksums: &[u8], version: &MachineVersion) -> String {
    sign(checksums, &release_signature_comment(version))
}

/// A legacy minisign signature over `data` carrying any trusted comment.
#[must_use]
pub fn sign(data: &[u8], trusted_comment: &str) -> String {
    let key = signing_key();
    let signature = key.sign(data).to_bytes();
    let mut line = b"Ed".to_vec();
    line.extend_from_slice(&KEY_ID);
    line.extend_from_slice(&signature);
    let mut global = signature.to_vec();
    global.extend_from_slice(trusted_comment.as_bytes());
    format!(
        "untrusted comment: ployz test signature\n{}\ntrusted comment: {trusted_comment}\n{}\n",
        STANDARD.encode(line),
        STANDARD.encode(key.sign(&global).to_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signatures_verify_under_the_test_key_only() {
        let version = MachineVersion::parse("1.2.3").unwrap();
        let signature = sign_release(b"checksums", &version);
        ployz_core::verify_release_checksums(
            &release_public_key(),
            b"checksums",
            &signature,
            &version,
        )
        .unwrap();
        assert!(
            ployz_core::verify_release_checksums(
                ployz_core::RELEASE_PUBLIC_KEY,
                b"checksums",
                &signature,
                &version,
            )
            .is_err()
        );
    }
}
