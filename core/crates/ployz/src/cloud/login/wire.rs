//! What Cloud's device-flow routes send, as this device reads it.

use serde::Deserialize;

use super::Account;

#[derive(Deserialize)]
pub(super) struct CodeReply {
    pub(super) device_code: String,
    pub(super) user_code: String,
    pub(super) verification_uri_complete: String,
    pub(super) expires_in: u64,
    pub(super) interval: u64,
}

#[derive(Deserialize)]
pub(super) struct TokenReply {
    pub(super) access_token: String,
}

#[derive(Deserialize)]
pub(super) struct TokenError {
    pub(super) error: String,
}

/// The `error` of a 400 from the device token route (RFC 8628 §3.5). Cloud may name one this
/// build does not know.
pub(super) enum DeviceTokenError {
    Pending,
    SlowDown,
    Denied,
    /// An unknown device code has been spent or swept: it can never be approved.
    Expired,
    Unknown(String),
}

impl DeviceTokenError {
    pub(super) fn from_wire(error: &str) -> Self {
        match error {
            "authorization_pending" => Self::Pending,
            "slow_down" => Self::SlowDown,
            "access_denied" => Self::Denied,
            "expired_token" | "invalid_grant" => Self::Expired,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionReply {
    pub(super) user: Account,
    pub(super) session: SessionFields,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionFields {
    pub(super) active_organization_id: Option<String>,
    pub(super) active_organization_slug: Option<String>,
}

/// Named `Reply` and `Device` because serde's decode errors name the struct, and those
/// sentences reach the user as they always did.
#[derive(Deserialize)]
pub(super) struct Reply {
    pub(super) signed_out: Device,
    pub(super) servers: crate::cloud::account::ServerClears,
}

#[derive(Deserialize)]
pub(super) struct Device {
    pub(super) id: String,
}

#[cfg(test)]
mod tests {
    use super::{Device, Reply};

    #[test]
    fn a_logout_reply_that_does_not_decode_names_the_structs_it_always_named() {
        let error = |body: &str| {
            serde_json::from_str::<Reply>(body)
                .err()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            error("null"),
            "invalid type: null, expected struct Reply at line 1 column 4"
        );
        assert_eq!(
            error(r#"{"signed_out":null,"servers":{"confirmed":[],"unconfirmed":[]}}"#),
            "invalid type: null, expected struct Device at line 1 column 18"
        );
        assert!(serde_json::from_str::<Device>(r#"{"id":1}"#).is_err());
    }
}
