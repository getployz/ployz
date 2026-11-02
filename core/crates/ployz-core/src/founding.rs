//! Which founding a Participating Machine's Cluster came from, so Cloud enrollment
//! resumes only a Cluster its own Founding Claim founded.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use crate::ValueError;

/// Names the Cloud pairing that granted a Founding Claim, without carrying its bearer.
#[derive(Clone, Copy, Eq, Hash, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct FoundingClaim([u8; 32]);

impl FoundingClaim {
    /// The claim a Cloud pairing secret names. Domain-separated, so it equals no other
    /// digest of that secret.
    #[must_use]
    pub fn of_pairing(secret: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"ployz founding claim v1\0");
        hasher.update(secret.as_bytes());
        Self(hasher.finalize().into())
    }
}

impl fmt::Display for FoundingClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&STANDARD.encode(self.0))
    }
}

impl fmt::Debug for FoundingClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "FoundingClaim({self})")
    }
}

impl TryFrom<String> for FoundingClaim {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        STANDARD
            .decode(&value)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .map(Self)
            .ok_or_else(|| ValueError::new("Founding Claim", value, "32 bytes in base64"))
    }
}

impl From<FoundingClaim> for String {
    fn from(value: FoundingClaim) -> Self {
        value.to_string()
    }
}

/// What granted a founding, as the founding client states it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FoundingGrant {
    /// Founded without Ployz Cloud, such as by `ployz init`.
    Standalone,
    /// Founded under this Cloud Founding Claim.
    Claim { claim: FoundingClaim },
    /// A grant a later version wrote, read back by this one.
    #[serde(other)]
    Unrecognized,
}

/// How a Machine came to its Cluster, as far as its local record says.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Founding {
    /// Joined a Cluster another Machine founded.
    Joined,
    /// Founded without a Founding Claim, such as by `ployz init`.
    Standalone,
    /// Founded under this Founding Claim.
    Claim { claim: FoundingClaim },
    /// Not Participating, founded before the record kept its founding, or a daemon
    /// that doesn't say.
    #[default]
    #[serde(other)]
    Unrecorded,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claim_names_its_pairing_and_round_trips() {
        let claim = FoundingClaim::of_pairing("ppair_fake");
        assert_eq!(claim, FoundingClaim::of_pairing("ppair_fake"));
        assert_ne!(claim, FoundingClaim::of_pairing("ppair_other"));
        let wire = serde_json::to_value(Founding::Claim { claim }).expect("serializes");
        assert_eq!(
            serde_json::from_value::<Founding>(wire).expect("parses"),
            Founding::Claim { claim }
        );
    }

    #[test]
    fn a_founding_this_build_does_not_know_reads_as_unrecorded() {
        let parsed: Founding = serde_json::from_str(r#"{"kind":"later"}"#).expect("parses");
        assert_eq!(parsed, Founding::Unrecorded);
    }

    #[test]
    fn a_grant_this_build_does_not_know_still_reads() {
        let parsed: FoundingGrant = serde_json::from_str(r#"{"kind":"later"}"#).expect("parses");
        assert_eq!(parsed, FoundingGrant::Unrecognized);
    }
}
