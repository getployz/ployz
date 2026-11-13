//! Calls to `/api/cli`: one that answers at once, one Cloud runs durably and the CLI
//! follows, and Cloud's own refusals.

use ployz_core::RpcError;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, de::DeserializeOwned};

use super::{
    credential::Credential,
    http::{self, CALL_DEADLINE, Effect, Via},
    reply::{StoreCallError, answer, refusal, store_answer, stored},
};
use crate::cloud_login::LoginError;

/// Start a durable Cloud run at `/api/cli/<path>`, carrying `approval` when Cloud asked
/// for one, and return the run's id.
///
/// # Errors
///
/// Returns Cloud's refusal, `approval_required` among them, or a Cloud failure.
pub(crate) async fn start_run(
    credential: &Credential,
    method: Method,
    path: &str,
    body: &serde_json::Value,
    approval: Option<&str>,
) -> Result<String, StoreCallError> {
    #[derive(Deserialize)]
    struct Queued {
        id: String,
    }
    let queued: Queued = approved_answer(credential, method, path, body, approval).await?;
    Ok(queued.id)
}

/// Cloud's answer to `/api/cli/<path>`, carrying `approval` when Cloud asked for one.
///
/// # Errors
///
/// Returns Cloud's refusal, `approval_required` among them, or a Cloud failure.
pub(crate) async fn approved_answer<T: DeserializeOwned>(
    credential: &Credential,
    method: Method,
    path: &str,
    body: &serde_json::Value,
    approval: Option<&str>,
) -> Result<T, StoreCallError> {
    keyed_answer(credential, method, path, body, approval, None).await
}

/// [`approved_answer`] with `x-ployz-request-key`, so a retried call of a Cloud
/// command runs once.
///
/// # Errors
///
/// Returns Cloud's refusal, or a Cloud failure.
pub(crate) async fn keyed_answer<T: DeserializeOwned>(
    credential: &Credential,
    method: Method,
    path: &str,
    body: &serde_json::Value,
    approval: Option<&str>,
    request_key: Option<&str>,
) -> Result<T, StoreCallError> {
    keyed_reply(
        credential,
        Via::Cli,
        method,
        path,
        body,
        approval,
        request_key,
    )
    .await
    .0
}

/// [`keyed_answer`] with the manifest revision the reply named, for a caller that
/// holds its own copy of the published commands rather than reading [`revision_seen`].
pub(crate) async fn keyed_reply<T: DeserializeOwned>(
    credential: &Credential,
    via: Via,
    method: Method,
    path: &str,
    body: &serde_json::Value,
    approval: Option<&str>,
    request_key: Option<&str>,
) -> (Result<T, StoreCallError>, Option<String>) {
    let url = format!("{}/api/cli/{path}", credential.cloud());
    let sent = async {
        let request =
            http::approved_request(credential, via, method, &url, body, approval, request_key)?;
        http::dispatch(credential.cloud(), request, Effect::Read).await
    };
    let response = match sent.await {
        Ok(response) => response,
        Err(error) => return (Err(error.into()), None),
    };
    let revision = http::revision_of(response.headers());
    (
        store_answer(credential, response, Effect::Read).await,
        revision,
    )
}

/// Read the Cloud run at `/api/cli/<path>` every second until its `state` is neither
/// `pending` nor `running`, and decode it then. `None` once `deadline` passes first.
///
/// # Errors
///
/// Returns Cloud's refusal, a settled run this can't decode, a Cloud failure, or
/// [`StoreCallError::Stopped`] on Ctrl-C.
pub(crate) async fn follow_run<T: DeserializeOwned>(
    credential: &Credential,
    path: &str,
    doing: &str,
    deadline: Option<tokio::time::Instant>,
) -> Result<Option<T>, StoreCallError> {
    let interrupted = crate::cancellation::interrupted()
        .map_err(|error| StoreCallError::Stopped(crate::failure::Failure::from(error)))?;
    let stopped = || {
        StoreCallError::Stopped(
            crate::failure::Failure::coded(
                ployz_core::RpcErrorCode::Internal,
                format!("Stopped following. {doing} keeps running in Ployz Cloud and finishes on its own."),
            )
            .interrupted(),
        )
    };
    loop {
        let read: serde_json::Value = tokio::select! {
            () = interrupted.cancelled() => return Err(stopped()),
            read = refusable(credential, Method::GET, path, None) => read?,
        };
        let state = read.get("state").and_then(serde_json::Value::as_str);
        if !matches!(state, Some("pending" | "running")) {
            return serde_json::from_value(read)
                .map(Some)
                .map_err(|error| LoginError::Reply(error.to_string()).into());
        }
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Ok(None);
        }
        tokio::select! {
            () = interrupted.cancelled() => return Err(stopped()),
            () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
        }
    }
}

pub(crate) async fn follow_operation<T: DeserializeOwned>(
    credential: &Credential,
    path: &str,
    doing: &str,
) -> Result<Settled<T>, StoreCallError> {
    follow_operation_until(
        credential,
        path,
        doing,
        tokio::time::Instant::now() + CALL_DEADLINE,
    )
    .await
}

async fn follow_operation_until<T: DeserializeOwned>(
    credential: &Credential,
    path: &str,
    doing: &str,
    deadline: tokio::time::Instant,
) -> Result<Settled<T>, StoreCallError> {
    follow_run(credential, path, doing, Some(deadline))
        .await?
        .ok_or_else(|| {
            StoreCallError::Refused(RpcError::unavailable(format!(
                "Stopped waiting after {} minutes. {doing} keeps running in Ployz Cloud and \
                     finishes on its own; this command no longer reports it.",
                CALL_DEADLINE.as_secs() / 60
            )))
        })
}

/// How a Cloud run of an operation settled: its result, or why it ended without one.
#[derive(Debug, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum Settled<T> {
    Finished(T),
    Ended { code: String, message: String },
}

impl<T> Settled<T> {
    pub(crate) fn finished(self) -> Result<T, crate::failure::Failure> {
        match self {
            Self::Finished(result) => Ok(result),
            Self::Ended { code, message } => {
                let code = match code.as_str() {
                    "refused" => ployz_core::RpcErrorCode::Conflict,
                    _ => ployz_core::RpcErrorCode::Unavailable,
                };
                Err(crate::failure::Failure::coded(code, message))
            }
        }
    }
}

/// Call `/api/cli/<path>`, answered as its reply or as Cloud's refusal, verbatim.
///
/// # Errors
///
/// Returns Cloud's refusal, or a Cloud failure; a Cloud without the route is
/// [`LoginError::Unsupported`].
pub(crate) async fn refusable<T: DeserializeOwned>(
    credential: &Credential,
    method: Method,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<T, StoreCallError> {
    let url = format!("{}/api/cli/{path}", credential.cloud());
    let request = http::request(credential, method, &url, body)?;
    let reply = http::exchange(credential.cloud(), request, Effect::Read).await?;
    stored(credential, reply.status, &reply.body)
}

/// [`refusable`] for a call Cloud answers only once it has read the Servers, so it waits
/// [`OPERATION_TIMEOUT`] rather than the client's 15s default.
///
/// # Errors
///
/// Returns Cloud's refusal, or a Cloud failure; a Cloud without the route is
/// [`LoginError::Unsupported`].
pub(crate) async fn refusable_operation<T: DeserializeOwned>(
    credential: &Credential,
    method: Method,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<T, StoreCallError> {
    let request = http::operation_request(credential, method, path, body)?;
    let reply = http::exchange(credential.cloud(), request, Effect::Read).await?;
    stored(credential, reply.status, &reply.body)
}

/// Call `/api/cli/<path>`. A 404 on a read means Cloud doesn't offer the CLI surface,
/// unless Cloud sent its refusal envelope with it.
pub(crate) async fn call<T: DeserializeOwned>(
    credential: &Credential,
    method: Method,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<T, LoginError> {
    let read_only = method == Method::GET;
    let url = format!("{}/api/cli/{path}", credential.cloud());
    let request = http::request(credential, method, &url, body)?;
    let reply = http::exchange(credential.cloud(), request, Effect::Read).await?;
    if read_only && reply.status == StatusCode::NOT_FOUND && refusal(&reply.body).is_none() {
        return Err(LoginError::Unsupported(credential.cloud().to_owned()));
    }
    answer(credential, reply.status, &reply.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_account::testing::{fake_cloud, token};

    #[tokio::test]
    async fn a_reads_refusal_reaches_the_caller() {
        let refused = serde_json::json!({ "error": {
            "code": "not_found",
            "message": "No repository by that name that this Organization can read.",
            "details": { "valid_children": ["acme/web"] },
        }});
        let cloud = fake_cloud(move |_, _| (404, refused.clone()));
        let error = call::<serde_json::Value>(&token(&cloud), Method::GET, "github/tree", None)
            .await
            .unwrap_err();
        let LoginError::Status { status: 404, body } = error else {
            panic!("expected the refusal, got {error}");
        };
        let refused = refusal(body.as_bytes()).expect("the body is a refusal");
        assert_eq!(refused.code, ployz_core::RpcErrorCode::NotFound);
        assert_eq!(
            refused.details,
            serde_json::json!({ "valid_children": ["acme/web"] })
        );
    }

    #[tokio::test]
    async fn an_operation_that_outlasts_the_wait_says_it_keeps_running_in_cloud() {
        let cloud = fake_cloud(|route, _| match route {
            "GET /api/cli/server-drains/drn_1" => (200, serde_json::json!({ "state": "running" })),
            other => panic!("unexpected {other}"),
        });
        let past = tokio::time::Instant::now();
        let error = follow_operation_until::<serde_json::Value>(
            &token(&cloud),
            "server-drains/drn_1",
            "Draining Server web-1",
            past,
        )
        .await
        .unwrap_err();
        let StoreCallError::Refused(refused) = error else {
            panic!("{error:?}")
        };
        assert_eq!(refused.code, ployz_core::RpcErrorCode::Unavailable);
        assert_eq!(
            refused.message,
            "Stopped waiting after 30 minutes. Draining Server web-1 keeps running in Ployz Cloud \
             and finishes on its own; this command no longer reports it."
        );
    }
}
