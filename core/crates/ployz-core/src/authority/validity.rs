//! The signed lifetime and clock-skew limits shared by Device approvals.

use super::{AuthorityRefusal, Timestamp};

/// Maximum signed lifetime, excluding clock skew.
pub const MAX_LIFETIME_MILLIS: u64 = 15 * 60 * 1000;
/// Clock tolerance in milliseconds on either side of the signed interval.
pub const CLOCK_SKEW_MILLIS: u64 = 2 * 60 * 1000;

/// Check an approval's signed interval against the Machine's clock.
///
/// # Errors
/// Refuses an invalid lifetime, an issue time beyond clock skew, or an expired interval.
pub fn verify(
    issued_at: Timestamp,
    expires_at: Timestamp,
    now: Timestamp,
) -> Result<(), AuthorityRefusal> {
    verify_within(issued_at, expires_at, now, MAX_LIFETIME_MILLIS)
}

/// [`verify`] with a lifetime cap of `max_lifetime` milliseconds.
///
/// # Errors
/// As [`verify`].
pub fn verify_within(
    issued_at: Timestamp,
    expires_at: Timestamp,
    now: Timestamp,
    max_lifetime: u64,
) -> Result<(), AuthorityRefusal> {
    if expires_at < issued_at || expires_at.as_millis() - issued_at.as_millis() > max_lifetime {
        return Err(AuthorityRefusal::Mismatch {
            field: "a lifetime",
        });
    }
    if issued_at > now.plus(CLOCK_SKEW_MILLIS) {
        return Err(AuthorityRefusal::Mismatch {
            field: "an issue time",
        });
    }
    if now > expires_at.plus(CLOCK_SKEW_MILLIS) {
        return Err(AuthorityRefusal::Expired);
    }
    Ok(())
}
