//! The Config Store's `read` and `write` over `/api/config`, and uploading a source
//! archive for a Deployment.

use std::time::Duration;

use ployz_core::{RpcError, cloud_contract};
use reqwest::{Method, StatusCode};
use serde::{Serialize, de::DeserializeOwned};

use super::{
    credential::Credential,
    http::{self, Effect, Reply, Via},
    reply::{StoreCallError, public_error, refusal, store_answer, stored},
};
use crate::cloud_login::LoginError;

/// Call the Config Store's `operation` (`read` or `write`) in the credential's
/// Organization. `body` is a Query or a Command; the reply is a View or Written.
///
/// # Errors
///
/// Returns the Store's refusal, or a Cloud failure; a Cloud without the Store is
/// [`LoginError::Unsupported`].
pub(crate) async fn config_store<T: DeserializeOwned>(
    credential: &Credential,
    via: Via,
    operation: &str,
    body: &impl Serialize,
    approval: Option<&str>,
) -> Result<T, StoreCallError> {
    let reply = config_exchange(credential, via, operation, body, approval).await?;
    stored(credential, reply.status, &reply.body)
}

/// [`config_store`] on a route a Cloud may predate: `None` when this Cloud has no
/// `operation` route, the 404 `PublicError` `NOT_FOUND` it answers an unknown path
/// with. A Store's own `not_found` travels as `{"error": …}` and stays a refusal.
///
/// # Errors
///
/// As [`config_store`].
pub(crate) async fn config_store_if_routed<T: DeserializeOwned>(
    credential: &Credential,
    via: Via,
    operation: &str,
    body: &impl Serialize,
) -> Result<Option<T>, StoreCallError> {
    let reply = config_exchange(credential, via, operation, body, None).await?;
    let unrouted = reply.status == StatusCode::NOT_FOUND
        && refusal(&reply.body).is_none()
        && public_error(&reply.body)
            .is_some_and(|error| error.code == ployz_core::RpcErrorCode::NotFound);
    if unrouted {
        return Ok(None);
    }
    stored(credential, reply.status, &reply.body).map(Some)
}

/// `POST /api/config/<operation>`, which a `write` may have applied though its answer
/// never came.
async fn config_exchange(
    credential: &Credential,
    via: Via,
    operation: &str,
    body: &impl Serialize,
    approval: Option<&str>,
) -> Result<Reply, StoreCallError> {
    let body = serde_json::to_value(body).map_err(|error| LoginError::Reply(error.to_string()))?;
    let url = format!("{}/api/config/{operation}", credential.cloud());
    let effect = if operation == "write" {
        Effect::Write
    } else {
        Effect::Read
    };
    let request =
        http::approved_request(credential, via, Method::POST, &url, &body, approval, None)?;
    Ok(http::exchange(credential.cloud(), request, effect).await?)
}

/// How long one upload may take, sending included: 95 MiB at about 1.3 Mbit/s.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Keep `archive`, a gzipped tar of a source directory, in Cloud as the upload of
/// Deployment `deployment`, which the CLI admits next. An archive over
/// [`cloud_contract::UPLOAD_LIMIT`] is refused here, before it is sent.
///
/// # Errors
///
/// Returns Cloud's refusal (`conflict` once the Deployment was admitted or already has
/// an upload, `invalid_argument` over the size cap), or a Cloud failure.
pub(crate) async fn upload(
    credential: &Credential,
    deployment: &str,
    archive: Vec<u8>,
) -> Result<(), StoreCallError> {
    let mib = archive.len().div_ceil(1024 * 1024);
    if archive.len() > cloud_contract::UPLOAD_LIMIT {
        return Err(StoreCallError::Refused(too_big(&format!(
            "The source is {mib} MiB compressed, over the {} MiB upload limit.",
            cloud_contract::UPLOAD_LIMIT / 1024 / 1024
        ))));
    }
    let url = format!("{}/api/config/upload/{deployment}", credential.cloud());
    let upload = http::client()?
        .post(&url)
        .timeout(UPLOAD_TIMEOUT)
        .bearer_auth(credential.bearer())
        .header(reqwest::header::CONTENT_TYPE, "application/gzip")
        .body(archive);
    let response = http::dispatch(credential.cloud(), upload, Effect::Write).await?;
    // A proxy in front of Cloud with a lower cap refuses the body before Cloud sees it.
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return Err(StoreCallError::Refused(too_big(&format!(
            "Cloud refused the {mib} MiB compressed source as too large."
        ))));
    }
    store_answer::<serde_json::Value>(credential, response, Effect::Write).await?;
    Ok(())
}

/// The refusal of a source too large to upload: `why`, then how to deploy it anyway.
fn too_big(why: &str) -> RpcError {
    RpcError::invalid_argument(format!(
        "{why} Deploy from GitHub instead, or remove large files from the directory. In a Git checkout, ployz up leaves out files Git ignores, so add ones such as node_modules to .gitignore."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_account::testing::{fake_cloud, token};

    #[tokio::test]
    async fn an_archive_over_the_limit_is_refused_before_it_is_sent() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let cloud = format!("http://{}", listener.local_addr().unwrap());
        let archive = vec![0; cloud_contract::UPLOAD_LIMIT + 1];
        let error = upload(&token(&cloud), "d1", archive).await.unwrap_err();
        let StoreCallError::Refused(refused) = error else {
            panic!("expected the size refusal");
        };
        assert_eq!(refused.code, ployz_core::RpcErrorCode::InvalidArgument);
        assert_eq!(
            refused.message,
            "The source is 96 MiB compressed, over the 95 MiB upload limit. Deploy from GitHub instead, or remove large files from the directory. In a Git checkout, ployz up leaves out files Git ignores, so add ones such as node_modules to .gitignore."
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "nothing reached Cloud"
        );
    }

    #[tokio::test]
    async fn a_proxy_s_413_reads_as_the_size_refusal() {
        let cloud = fake_cloud(|_, _| (413, serde_json::json!({})));
        let error = upload(&token(&cloud), "d1", b"source".to_vec())
            .await
            .unwrap_err();
        let StoreCallError::Refused(refused) = error else {
            panic!("expected the size refusal");
        };
        assert_eq!(refused.code, ployz_core::RpcErrorCode::InvalidArgument);
        assert!(
            refused.message.starts_with(
                "Cloud refused the 1 MiB compressed source as too large. Deploy from GitHub instead"
            ),
            "{}",
            refused.message
        );
    }
}
