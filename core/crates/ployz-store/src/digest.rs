//! The short digest a review or an approval binds: enough to tell reviews apart.

use ring::digest::{SHA256, digest};

const BYTES: usize = 8;

/// How many hex digits [`short_digest`] returns.
pub(crate) const HEX_LEN: usize = BYTES * 2;

/// The first 8 bytes of `text`'s SHA-256, in hex: enough to tell reviews apart.
pub(crate) fn short_digest(text: &str) -> String {
    let digest = digest(&SHA256, text.as_bytes());
    hex::encode(digest.as_ref().get(..BYTES).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_digest_is_the_hex_of_the_first_bytes_of_the_sha256() {
        assert_eq!(short_digest("{}"), "44136fa355b3678a");
        assert_eq!(short_digest("").len(), HEX_LEN);
    }
}
