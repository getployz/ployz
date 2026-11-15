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

#[derive(Deserialize)]
pub(super) struct LogoutReply {
    pub(super) signed_out: SignedOutDevice,
    pub(super) servers: crate::cloud_account::ServerClears,
}

#[derive(Deserialize)]
pub(super) struct SignedOutDevice {
    pub(super) id: String,
}
