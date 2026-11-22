//! Reading Cloud's answer: its refusal verbatim, its `PublicError`, or the error that
//! names the credential's fix.

use ployz_core::RpcError;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

use super::credential::{Credential, env_token};
use crate::cloud;
use crate::cloud::http::{Effect, read_reply};
use crate::cloud::login::LoginError;

/// Why a Config Store call over HTTPS failed.
#[derive(Debug)]
pub(crate) enum StoreCallError {
    /// The Store refused the command or query, exactly as it would in-process.
    Refused(RpcError),
    /// Cloud or the credential failed before the Store answered.
    Cloud(LoginError),
    Stopped(crate::failure::Failure),
}

impl From<LoginError> for StoreCallError {
    fn from(error: LoginError) -> Self {
        Self::Cloud(error)
    }
}

/// The Store's answer, its refusal verbatim, or why Cloud failed first.
pub(crate) async fn store_answer<T: DeserializeOwned>(
    credential: &Credential,
    response: reqwest::Response,
    effect: Effect,
) -> Result<T, StoreCallError> {
    let reply = read_reply(credential.cloud(), response, effect).await?;
    stored(credential, reply.status, &reply.body)
}

pub(crate) fn stored<T: DeserializeOwned>(
    credential: &Credential,
    status: StatusCode,
    bytes: &[u8],
) -> Result<T, StoreCallError> {
    if !status.is_success() {
        if let Some(refused) = refusal(bytes) {
            return Err(StoreCallError::Refused(refused));
        }
        if !matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && let Some(refused) = crate::cloud::reply::public_error(bytes)
        {
            return Err(StoreCallError::Refused(refused));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(LoginError::Unsupported(credential.cloud().to_owned()).into());
        }
    }
    Ok(answer(credential, status, bytes)?)
}

/// Cloud's refusal of a call, with a next step that works under `PLOYZ_TOKEN`.
pub(crate) fn refusal(body: &[u8]) -> Option<RpcError> {
    crate::cloud::reply::command_refusal(body).map(past_token)
}

/// What Cloud refused with: a command's refusal, else a route's `PublicError`.
pub(crate) fn refusal_of(body: &[u8]) -> Option<RpcError> {
    refusal(body).or_else(|| crate::cloud::reply::public_error(body))
}

/// `ployz login` while `PLOYZ_TOKEN` is set: the token still wins over the sign-in.
pub(crate) const SIGN_IN_PAST_TOKEN: &str = "unset PLOYZ_TOKEN && ployz login";

/// Cloud can't know a token caller is a shell with `PLOYZ_TOKEN` set, so its
/// `ployz login` step becomes one that works while the token is.
fn past_token(mut refused: RpcError) -> RpcError {
    let token = env_token().is_some();
    if token
        && let Some(next) = refused.details.get_mut("next")
        && next == "ployz login"
    {
        *next = SIGN_IN_PAST_TOKEN.into();
    }
    refused
}

pub(crate) fn answer<T: DeserializeOwned>(
    credential: &Credential,
    status: StatusCode,
    body: &[u8],
) -> Result<T, LoginError> {
    match (status, credential) {
        (status, _) if status.is_success() => cloud::login::decode(body.to_vec()),
        (StatusCode::UNAUTHORIZED, Credential::Device(_)) => Err(LoginError::Ended),
        (StatusCode::UNAUTHORIZED, Credential::Token { .. }) => Err(credential.token_refused()),
        // Cloud refuses a member's call in the RPC error vocabulary (`claim_required`);
        // only a bare 403 means the sign-in left its Organization.
        (StatusCode::FORBIDDEN, Credential::Device(signed_in)) if refusal(body).is_none() => {
            Err(LoginError::NotMember(signed_in.organization.slug.clone()))
        }
        (status, _) => Err(LoginError::status(status, body)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::account::{
        credential,
        testing::{signed_in_store, token},
    };

    #[tokio::test]
    async fn a_device_keeps_cloud_s_structured_refusal_of_a_403() {
        let (_dir, store) = signed_in_store("http://cloud.test");
        let device = credential(&store, Some(String::new()), None).await.unwrap();
        let claim = br#"{"error":{"code":"claim_required","message":"Claim this Organization.","details":{"claim_url":"http://cloud.test/claim/x"}}}"#;
        let error = answer::<serde_json::Value>(&device, StatusCode::FORBIDDEN, claim).unwrap_err();
        assert!(
            matches!(error, LoginError::Status { status: 403, .. }),
            "{error:?}"
        );
        let bare = br#"{"_tag":"PublicError","code":"FORBIDDEN","message":"no"}"#;
        let error = answer::<serde_json::Value>(&device, StatusCode::FORBIDDEN, bare).unwrap_err();
        assert!(matches!(error, LoginError::NotMember(slug) if slug == "acme"));
    }

    #[test]
    fn a_proxy_s_error_page_prints_as_its_status_alone() {
        let credential = token("http://cloud.test");
        let page = b"<!DOCTYPE html>\n<html><head><title>502 Bad Gateway</title></head></html>";
        let error =
            answer::<serde_json::Value>(&credential, StatusCode::BAD_GATEWAY, page).unwrap_err();
        assert_eq!(error.to_string(), "Cloud answered HTTP 502");
        let LoginError::Status { body, .. } = &error else {
            panic!("expected a status error, got {error}");
        };
        assert!(
            body.starts_with("<!DOCTYPE"),
            "the body is kept for refusal parsing"
        );
        let error =
            answer::<serde_json::Value>(&credential, StatusCode::BAD_GATEWAY, b"upstream down")
                .unwrap_err();
        assert_eq!(error.to_string(), "Cloud answered HTTP 502: upstream down");
    }
}
