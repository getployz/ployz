//! Every string that keeps one use of a key, a digest or a signature apart from another: the
//! HPKE `info` a value is sealed under, the label a key is expanded under, and the prefix a
//! statement is signed or hashed behind.
//!
//! Two uses of one key that shared a string could open or verify each other's bytes, so each
//! string here is its own, and none is the start of another. Each is also on the wire or in a
//! stored row, so a change to one strands what was sealed or signed under it: add a string
//! for new bytes, never edit one.

/// HPKE `info` for env values sealed to a Cluster's Sealing Key. The first field of such a
/// value's associated data too.
pub const SEALED_VALUE: &[u8] = b"ployz-sealed-v1";

/// HPKE `info` for values Ployz itself seals to a Cluster's Sealing Key for one
/// [`PlatformPurpose`](crate::PlatformPurpose), apart from any Service's env. The first field
/// of such a value's associated data too.
pub const PLATFORM_SEALED: &[u8] = b"ployz-platform-sealed-v1";

/// HPKE `info` for a registry login a client seals to a Cluster's Sealing Key for one
/// registry host. The first field of such a login's associated data too.
pub const REGISTRY_LOGIN: &[u8] = b"ployz-registry-login-v1";

/// What every build-secrets seal is bound under, followed by the grant's audience.
pub const BUILD_SECRETS: &[u8] = b"ployz build secrets v1";

/// HPKE `info` for a Sealing Key sealed to a joining Machine's WireGuard key.
pub const SEALING_KEY: &[u8] = b"ployz-sealing-key-v1";

/// HPKE `info` for a Sealing Key one Machine shares with another.
pub const SEALING_KEY_SHARE: &[u8] = b"ployz-sealing-key-share-v1";

/// HKDF label of the Cluster's build attest key, derived from its Sealing Key.
pub const BUILD_SECRETS_ATTEST: &[u8] = b"ployz build-secrets attest v1";

/// HKDF label of the key a Sealing secret's recovery backup is wrapped under.
pub const SEALING_KEY_BACKUP: &[u8] = b"ployz-sealing-key-backup-v1";

/// HKDF label of the key a joiner's confirmation is a MAC under.
pub const JOIN_CONFIRMATION: &[u8] = b"ployz-join-confirmation-v1";

/// HKDF label of the key the restrictions an admitting Machine hands a confirmed joiner are
/// a MAC under.
pub const JOIN_RESTRICTIONS: &[u8] = b"ployz-join-restrictions-v1";

/// HKDF label of the key a verified build is a MAC under.
pub const VERIFIED_BUILD_KEY: &[u8] = b"ployz-verified-build-key-v1";

/// What a verified build's MAC input starts with.
pub const VERIFIED_BUILD: &[u8] = b"ployz-verified-build/v1\n";

/// HKDF label of the key a Free Build's receipt is a MAC under.
pub const FREE_IMAGE_KEY: &[u8] = b"ployz-free-image-key-v1";

/// What a Free Build's receipt MAC input starts with.
pub const FREE_IMAGE: &[u8] = b"ployz-free-image/v1\n";

/// Digest prefix of the code that names a joining Machine.
pub const JOIN_CODE: &[u8] = b"ployz-join-v1\n";

/// Digest prefix of the Founding Claim a Cloud pairing secret names.
pub const FOUNDING_CLAIM: &[u8] = b"ployz founding claim v1\0";

/// Digest prefix of a WireGuard public key's fingerprint.
pub const WIREGUARD_KEY_DIGEST: &[u8] = b"ployz-wireguard-key-v1\n";

/// Signature prefix of a Machine's observability push.
pub const PUSH_SIGNATURE: &[u8] = b"ployz-push-v1\0";

/// Signature prefix of the token a Machine's Collector sends Cloud's OTLP ingest, followed by
/// the token's payload segment as sent.
pub const OTLP_TOKEN_SIGNATURE: &[u8] = b"ployz-otlp-v1\0";

/// Signature prefix of a waiting host's check-in with Cloud, which releases before Fleet Tokens
/// were removed still send. Reserved, so no new signature can pass for one.
pub const CHECKIN_SIGNATURE: &[u8] = b"ployz-checkin-v1\0";

/// Digest prefix of a build's cache scope.
pub const BUILD_CACHE: &[u8] = b"ployz build cache v1\0";

/// TLS exporter label, and signature prefix, of a management client's proof of its transport
/// key.
pub const MANAGEMENT_PROOF: &[u8] = b"ployz management client proof v1";

/// Signature prefix of a statement a Device or passkey signs.
pub const AUTHORITY_SIGNING: &[u8] = b"ployz-authority/v1\n";

/// Hashed before a Machine ID into the home a preview Namespace names.
pub const PREVIEW_HOME: &[u8] = b"ployz-preview-home-v1\0";

/// Hashed before a preview Namespace and a Docker Volume name into a preview birth's nonce.
pub const PREVIEW_BIRTH: &[u8] = b"ployz-preview-birth-v1\0";

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(&str, &[u8]); 25] = [
        ("SEALED_VALUE", SEALED_VALUE),
        ("PLATFORM_SEALED", PLATFORM_SEALED),
        ("REGISTRY_LOGIN", REGISTRY_LOGIN),
        ("BUILD_SECRETS", BUILD_SECRETS),
        ("SEALING_KEY", SEALING_KEY),
        ("SEALING_KEY_SHARE", SEALING_KEY_SHARE),
        ("BUILD_SECRETS_ATTEST", BUILD_SECRETS_ATTEST),
        ("SEALING_KEY_BACKUP", SEALING_KEY_BACKUP),
        ("JOIN_CONFIRMATION", JOIN_CONFIRMATION),
        ("JOIN_RESTRICTIONS", JOIN_RESTRICTIONS),
        ("VERIFIED_BUILD_KEY", VERIFIED_BUILD_KEY),
        ("VERIFIED_BUILD", VERIFIED_BUILD),
        ("FREE_IMAGE_KEY", FREE_IMAGE_KEY),
        ("FREE_IMAGE", FREE_IMAGE),
        ("JOIN_CODE", JOIN_CODE),
        ("FOUNDING_CLAIM", FOUNDING_CLAIM),
        ("WIREGUARD_KEY_DIGEST", WIREGUARD_KEY_DIGEST),
        ("PUSH_SIGNATURE", PUSH_SIGNATURE),
        ("OTLP_TOKEN_SIGNATURE", OTLP_TOKEN_SIGNATURE),
        ("CHECKIN_SIGNATURE", CHECKIN_SIGNATURE),
        ("BUILD_CACHE", BUILD_CACHE),
        ("MANAGEMENT_PROOF", MANAGEMENT_PROOF),
        ("AUTHORITY_SIGNING", AUTHORITY_SIGNING),
        ("PREVIEW_HOME", PREVIEW_HOME),
        ("PREVIEW_BIRTH", PREVIEW_BIRTH),
    ];

    #[test]
    fn no_string_is_another_or_the_start_of_another() {
        for (index, (name, string)) in ALL.iter().enumerate() {
            for (other_name, other) in ALL.iter().skip(index + 1) {
                assert!(
                    !string.starts_with(other) && !other.starts_with(string),
                    "{name} and {other_name} overlap"
                );
            }
        }
    }

    #[test]
    fn each_string_is_the_bytes_old_machines_and_stored_rows_know() {
        let expected: [&[u8]; 25] = [
            b"ployz-sealed-v1",
            b"ployz-platform-sealed-v1",
            b"ployz-registry-login-v1",
            b"ployz build secrets v1",
            b"ployz-sealing-key-v1",
            b"ployz-sealing-key-share-v1",
            b"ployz build-secrets attest v1",
            b"ployz-sealing-key-backup-v1",
            b"ployz-join-confirmation-v1",
            b"ployz-join-restrictions-v1",
            b"ployz-verified-build-key-v1",
            b"ployz-verified-build/v1\n",
            b"ployz-free-image-key-v1",
            b"ployz-free-image/v1\n",
            b"ployz-join-v1\n",
            b"ployz founding claim v1\0",
            b"ployz-wireguard-key-v1\n",
            b"ployz-push-v1\0",
            b"ployz-otlp-v1\0",
            b"ployz-checkin-v1\0",
            b"ployz build cache v1\0",
            b"ployz management client proof v1",
            b"ployz-authority/v1\n",
            b"ployz-preview-home-v1\0",
            b"ployz-preview-birth-v1\0",
        ];
        for ((name, string), expected) in ALL.iter().zip(expected) {
            assert_eq!(*string, expected, "{name}");
        }
    }
}
