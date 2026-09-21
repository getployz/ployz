//! Management Capability framing shared by the daemon, the CLI and the SDK.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::ValueError;

/// The only relay the management transport is configured with. Public relays are never used.
pub const DEFAULT_RELAY_URL: &str = "https://relay.ployz.dev";

/// ALPN of the Machine RPC stream on the management transport.
pub const MANAGEMENT_ALPN: &[u8] = b"ployz/rpc/1";

const PREFIX: &str = "ployz1:";
const BODY_LEN: usize = 64;

fn framing_error() -> ValueError {
    ValueError::new(
        "Management Capability",
        "[redacted]",
        "`ployz1:` followed by unpadded base64url of a 64-byte body",
    )
}

/// The public key identifying a Machine's management endpoint, distinct from a client secret.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ManagementIdentity([u8; 32]);

impl ManagementIdentity {
    /// Construct an identity from its encoded public key; the transport validates the key.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Encoded public key for the management transport.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Protected bearer granting shared administrative Machine RPC access to one Machine:
/// the Management Identity (the Machine's public key) and the client secret key that
/// the Machine accepts. Possession proves neither Machine identity nor Organization
/// authorization.
///
/// String form is `ployz1:` plus unpadded base64url of the 64-byte body
/// (identity then secret). The core crate holds raw bytes and does not depend on iroh.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ManagementCapability {
    machine: ManagementIdentity,
    client_secret: [u8; 32],
}

impl ManagementCapability {
    /// Frame a Machine public key and the client secret authorized to manage it.
    #[must_use]
    pub fn new(machine: ManagementIdentity, client_secret: [u8; 32]) -> Self {
        Self {
            machine,
            client_secret,
        }
    }

    /// Parse the single-line string form.
    ///
    /// # Errors
    /// Returns a redacted [`ValueError`] when the prefix, encoding or body length is wrong.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let encoded = value
            .as_ref()
            .strip_prefix(PREFIX)
            .ok_or_else(framing_error)?;
        // base64url rejects whitespace and control characters, so a multiline value fails here.
        let body = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| framing_error())?;
        if body.len() != BODY_LEN {
            return Err(framing_error());
        }
        let (machine, client_secret) = body.split_at(32);
        Ok(Self {
            machine: ManagementIdentity::from_bytes(
                machine.try_into().expect("split at 32 of a 64-byte body"),
            ),
            client_secret: client_secret
                .try_into()
                .expect("split at 32 of a 64-byte body"),
        })
    }

    /// Management Identity: the Machine's public key.
    #[must_use]
    pub fn machine(&self) -> &ManagementIdentity {
        &self.machine
    }

    /// Secret key the holder connects with. Do not log it.
    #[must_use]
    pub fn client_secret(&self) -> &[u8; 32] {
        &self.client_secret
    }

    /// Secret-bearing string form for serialization or a context file. Do not log it.
    #[must_use]
    pub fn to_secret_string(&self) -> String {
        let mut body = [0; BODY_LEN];
        body[..32].copy_from_slice(self.machine.as_bytes());
        body[32..].copy_from_slice(&self.client_secret);
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(body))
    }
}

impl fmt::Debug for ManagementCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl fmt::Display for ManagementCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl TryFrom<String> for ManagementCapability {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<ManagementCapability> for String {
    fn from(value: ManagementCapability) -> Self {
        value.to_secret_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ManagementCapability {
        ManagementCapability::new(ManagementIdentity::from_bytes([0xab; 32]), [0xcd; 32])
    }

    #[test]
    fn string_form_round_trips_through_parse_and_serde() {
        let capability = sample();
        let text = capability.to_secret_string();
        assert!(text.starts_with("ployz1:"));
        assert!(!text.contains('='));
        let parsed = ManagementCapability::parse(&text).unwrap();
        assert_eq!(parsed, capability);
        assert_eq!(parsed.machine().as_bytes(), &[0xab; 32]);
        assert_eq!(parsed.client_secret(), &[0xcd; 32]);
        let encoded = serde_json::to_string(&capability).unwrap();
        assert_eq!(encoded, serde_json::to_string(&text).unwrap());
        assert_eq!(
            serde_json::from_str::<ManagementCapability>(&encoded).unwrap(),
            capability
        );
    }

    #[test]
    fn debug_and_display_are_redacted() {
        let capability = sample();
        let secret = URL_SAFE_NO_PAD.encode([0xcd; 32]);
        let shown = format!("{capability} {capability:?}");
        assert!(!shown.contains(&secret));
        assert_eq!(shown, "[redacted] [redacted]");
    }

    #[test]
    fn malformed_input_is_rejected_without_echo() {
        let valid = sample().to_secret_string();
        let body = &valid["ployz1:".len()..];
        for value in [
            String::new(),
            "ployz1:".into(),
            body.to_owned(),
            format!("ployz2:{body}"),
            format!("ployz1:{}", URL_SAFE_NO_PAD.encode([0; 63])),
            format!("ployz1:{}", URL_SAFE_NO_PAD.encode([0; 65])),
            format!("ployz1:{}=", &body[..body.len() - 1]),
            "ployz1:not*base64!".into(),
            format!("{valid}\n"),
            format!("ployz1:{}\n{}", &body[..40], &body[40..]),
            format!("ployz1:{body} "),
        ] {
            let error = ManagementCapability::parse(&value).unwrap_err();
            assert!(!format!("{error} {error:?}").contains(body));
            let encoded = serde_json::to_string(&value).unwrap();
            let error = serde_json::from_str::<ManagementCapability>(&encoded).unwrap_err();
            assert!(!error.to_string().contains(body));
        }
    }
}
