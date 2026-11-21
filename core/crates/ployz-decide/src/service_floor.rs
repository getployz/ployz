//! A Service's floor on one Machine: the time of the newest forget that reached it. An
//! approval made before the floor no longer admits a Container of that Service there.
//!
//! Times are signers' clocks, and the floor is the forget's own. A Machine refuses a time
//! more than [`MAX_AHEAD_MILLIS`] past its own floored clock, rather than clamping it: a
//! clamp would rise again with each resend of the same forget.

use ployz_core::authority::Timestamp;

/// How far past this Machine's floored clock a signed time may be.
pub const MAX_AHEAD_MILLIS: u64 = 10 * 60 * 1000;

/// Why a time is refused against a floor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refused {
    /// Made before the floor.
    Forgotten { floor: Timestamp },
    /// More than [`MAX_AHEAD_MILLIS`] past `now`.
    Ahead,
}

/// Whether an approval made at `at` may admit a new Container, or start a kept one, at
/// `now` on a Machine whose floor for its Service is `floor`.
///
/// # Errors
/// Returns [`Refused::Ahead`] for a time past the guard, and [`Refused::Forgotten`] for
/// one below the floor.
pub fn admits(at: Timestamp, floor: Option<Timestamp>, now: Timestamp) -> Result<(), Refused> {
    if at > now.plus(MAX_AHEAD_MILLIS) {
        return Err(Refused::Ahead);
    }
    match floor {
        Some(floor) if at < floor => Err(Refused::Forgotten { floor }),
        _ => Ok(()),
    }
}

/// What a kept admission's times say against `floor`: whether one key that is still
/// trusted signed at or after it. Each key keeps its own time, so removing the key that
/// signed after the floor ends the admission even while one that signed before is trusted.
pub fn keys_stand(
    keys: impl IntoIterator<Item = (bool, Timestamp)>,
    floor: Option<Timestamp>,
) -> bool {
    keys.into_iter()
        .any(|(trusted, at)| trusted && floor.is_none_or(|floor| at >= floor))
}

/// What a forget signed at `signed_at` does to `floor` at `now`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Raise {
    /// The floor rose to the forget's time.
    Raised(Timestamp),
    /// The floor was already at or above it.
    Held(Timestamp),
}

/// The floor after a forget signed at `signed_at` reaches a Machine at `now`, its floored
/// clock. A floor only rises.
///
/// # Errors
/// Returns [`Refused::Ahead`] for a forget signed past the guard, leaving the floor.
pub fn raise(
    floor: Option<Timestamp>,
    signed_at: Timestamp,
    now: Timestamp,
) -> Result<Raise, Refused> {
    if signed_at > now.plus(MAX_AHEAD_MILLIS) {
        return Err(Refused::Ahead);
    }
    Ok(match floor {
        Some(floor) if floor >= signed_at => Raise::Held(floor),
        _ => Raise::Raised(signed_at),
    })
}
