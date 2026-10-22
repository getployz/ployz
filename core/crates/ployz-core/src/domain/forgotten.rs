//! The forgotten-Server question: a Machine asks a member whether the Cluster
//! forgot its Management Identity, so a Server removed while unreachable can fence
//! itself once it is back (#2927).

use crate::ManagementIdentity;

/// ALPN of the forgotten question on the management transport. The asker dials
/// with its own Management Identity as the transport key, opens no stream, and reads
/// the answer from the close code. It needs no Management Client, so a forgotten
/// Machine that holds nothing else can still ask.
pub const FORGOTTEN_ALPN: &[u8] = b"ployz/forgotten/1";

/// QUIC application close code on [`FORGOTTEN_ALPN`]: the Cluster forgot this identity.
pub const FORGOTTEN: u32 = 0x55;
/// QUIC application close code on [`FORGOTTEN_ALPN`]: this identity is a listed Machine.
pub const FORGOTTEN_LISTED: u32 = 0x56;
/// QUIC application close code on [`FORGOTTEN_ALPN`]: the member knows nothing either way.
pub const FORGOTTEN_NO_ANSWER: u32 = 0x57;

/// What a member tells an asker about one Management Identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgottenAnswer {
    /// The identity's row was forgotten and nothing lists it since.
    Forgotten,
    /// A Machine row carries the identity, or the forgotten id is listed again.
    Listed,
    /// Neither a row nor a tombstone mentions the identity.
    NoAnswer,
}

impl ForgottenAnswer {
    const CODES: [(Self, u32); 3] = [
        (Self::Forgotten, FORGOTTEN),
        (Self::Listed, FORGOTTEN_LISTED),
        (Self::NoAnswer, FORGOTTEN_NO_ANSWER),
    ];

    /// The close code a member answers with.
    #[must_use]
    pub fn close_code(self) -> u32 {
        Self::CODES
            .iter()
            .find(|(answer, _)| *answer == self)
            .map(|(_, code)| *code)
            .expect("every answer has a code")
    }

    /// The answer a close code carries, if it is one of ours.
    #[must_use]
    pub fn from_close_code(code: u32) -> Option<Self> {
        Self::CODES
            .iter()
            .find(|(_, candidate)| *candidate == code)
            .map(|(answer, _)| *answer)
    }
}

/// Tombstone key under which a forgotten Machine's Management Identity lives in the
/// `cluster` table; its value is the forgotten Machine's id.
#[must_use]
pub fn forgotten_key(identity: &ManagementIdentity) -> String {
    format!("forgotten:{}", identity.to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_round_trip_through_their_close_codes() {
        for answer in [
            ForgottenAnswer::Forgotten,
            ForgottenAnswer::Listed,
            ForgottenAnswer::NoAnswer,
        ] {
            assert_eq!(
                ForgottenAnswer::from_close_code(answer.close_code()),
                Some(answer)
            );
        }
        assert_eq!(ForgottenAnswer::from_close_code(0x50), None);
    }
}
