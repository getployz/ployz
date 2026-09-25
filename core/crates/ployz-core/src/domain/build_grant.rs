//! Build Grant framing: permission to push one Build's image into one Machine.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::management_capability::{decode_keys, encode_keys};
use crate::{ManagementIdentity, ValueError};

/// ALPN of the Build Grant push transport. It is served beside, never through, the
/// Machine RPC ALPN, so a grant key reaches image ingest and nothing else.
pub const BUILD_GRANT_ALPN: &[u8] = b"ployz/build-grant/1";

/// QUIC application close code on [`BUILD_GRANT_ALPN`] for a key that holds no live grant.
pub const BUILD_GRANT_REFUSED: u32 = 0x53;
/// QUIC application close code on [`BUILD_GRANT_ALPN`] once a served grant ends.
pub const BUILD_GRANT_ENDED: u32 = 0x54;

/// The only tag a grant push may write: this prefix plus the pushed manifest's
/// SHA-256 hex. Direct Image Transfer retains images under the same tags, so Image
/// Cleanup covers grant pushes without a second rule.
pub const RETAINED_DIGEST_TAG_PREFIX: &str = "ployz-sha256-";

const PREFIX: &str = "ployzgrant1:";

/// A Machine-minted permission to push one Build's image into that Machine and
/// nothing else: the Machine's Management Identity and the grant's secret key.
///
/// String form is `ployzgrant1:` plus unpadded base64url of the 64-byte body
/// (identity then secret). It is not a Management Capability: the Machine admits the
/// key only on [`BUILD_GRANT_ALPN`], never for Machine RPC.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BuildGrant {
    machine: ManagementIdentity,
    secret: [u8; 32],
}

impl BuildGrant {
    /// Frame the minting Machine's identity and the grant's secret key.
    #[must_use]
    pub fn new(machine: ManagementIdentity, secret: [u8; 32]) -> Self {
        Self { machine, secret }
    }

    /// Parse the single-line string form.
    ///
    /// # Errors
    /// Returns a redacted [`ValueError`] when the prefix, encoding or body length is wrong.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let (machine, secret) = decode_keys(PREFIX, value.as_ref()).ok_or_else(|| {
            ValueError::new(
                "Build Grant",
                "[redacted]",
                "`ployzgrant1:` followed by unpadded base64url of a 64-byte body",
            )
        })?;
        Ok(Self::new(ManagementIdentity::from_bytes(machine), secret))
    }

    /// Management Identity of the Machine that minted this grant.
    #[must_use]
    pub fn machine(&self) -> &ManagementIdentity {
        &self.machine
    }

    /// Secret key the pusher connects with. Do not log it.
    #[must_use]
    pub fn secret(&self) -> &[u8; 32] {
        &self.secret
    }

    /// Secret-bearing string form. Do not log it.
    #[must_use]
    pub fn to_secret_string(&self) -> String {
        encode_keys(PREFIX, &self.machine, &self.secret)
    }
}

impl fmt::Debug for BuildGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl TryFrom<String> for BuildGrant {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<BuildGrant> for String {
    fn from(value: BuildGrant) -> Self {
        value.to_secret_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_round_trips_redacted_and_is_not_a_management_capability() {
        let grant = BuildGrant::new(ManagementIdentity::from_bytes([1; 32]), [2; 32]);
        let text = grant.to_secret_string();
        assert_eq!(BuildGrant::parse(&text).unwrap(), grant);
        assert_eq!(format!("{grant:?}"), "[redacted]");
        let capability = crate::ManagementCapability::new(*grant.machine(), *grant.secret());
        assert!(BuildGrant::parse(capability.to_secret_string()).is_err());
        assert!(crate::ManagementCapability::parse(&text).is_err());
        let error = BuildGrant::parse(format!("{text} ")).unwrap_err();
        assert!(!format!("{error}").contains(&text[PREFIX.len()..]));
    }
}
