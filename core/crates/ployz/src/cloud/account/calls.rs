//! Calls to `/api/cli`: one that answers at once, one Cloud runs durably and the CLI
//! follows, and Cloud's own refusals.

use ployz_agent_tools::{Act, CloudQuery};
use ployz_core::RpcError;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{
    credential::Credential,
    reply::{StoreCallError, answer, refusal, store_answer, stored},
};
use crate::cloud::http::{self, CALL_DEADLINE, Effect, Via};
use crate::cloud::login::LoginError;
use crate::cloud::reply::{EndedCode, RunState};

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

/// What `ployz mcp` hands Cloud: an ask's query or an Act.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudCall {
    Ask(CloudQuery),
    Act(Act),
}

/// Cloud's answer to `call`, carrying `approval` when Cloud asked for one.
///
/// # Errors
///
/// Returns Cloud's refusal, `approval_required` among them, or a Cloud failure.
pub(crate) async fn agent_call<T: DeserializeOwned>(
    credential: &Credential,
    call: &CloudCall,
    approval: Option<&str>,
) -> Result<T, StoreCallError> {
    let path = match call {
        CloudCall::Ask(_) => "agent/ask",
        CloudCall::Act(_) => "agent/act",
    };
    let body = serde_json::to_value(call).expect("a Cloud call is JSON");
    approved_answer(credential, Method::POST, path, &body, approval).await
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

/// Cloud's answer to a read at `/api/<path>`, outside the `/api/cli` commands.
pub(crate) async fn read_answer<T: DeserializeOwned>(
    credential: &Credential,
    path: &str,
    body: &serde_json::Value,
) -> Result<T, StoreCallError> {
    let url = format!("{}/api/{path}", credential.cloud());
    let response =
        http::approved_request(credential, Via::Cli, Method::POST, &url, body, None, None)
            .map_err(StoreCallError::from)?;
    let response = http::dispatch(credential.cloud(), response, Effect::Read).await?;
    store_answer(credential, response, Effect::Read).await
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
        if !RunState::of(&read).going() {
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
    follow_operation_for(credential, path, doing, CALL_DEADLINE).await
}

/// [`follow_operation`] for an operation Cloud lets run longer than [`CALL_DEADLINE`]:
/// it follows for `within`.
pub(crate) async fn follow_operation_for<T: DeserializeOwned>(
    credential: &Credential,
    path: &str,
    doing: &str,
    within: std::time::Duration,
) -> Result<Settled<T>, StoreCallError> {
    follow_operation_until(
        credential,
        path,
        doing,
        tokio::time::Instant::now() + within,
        within,
    )
    .await
}

async fn follow_operation_until<T: DeserializeOwned>(
    credential: &Credential,
    path: &str,
    doing: &str,
    deadline: tokio::time::Instant,
    within: std::time::Duration,
) -> Result<Settled<T>, StoreCallError> {
    follow_run(credential, path, doing, Some(deadline))
        .await?
        .ok_or_else(|| {
            let minutes = within.as_secs() / 60;
            let waited = if minutes.is_multiple_of(60) && minutes >= 120 {
                format!("{} hours", minutes / 60)
            } else {
                format!("{minutes} minutes")
            };
            StoreCallError::Refused(RpcError::unavailable(format!(
                "Stopped waiting after {waited}. {doing} keeps running in Ployz Cloud and \
                     finishes on its own; this command no longer reports it."
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
                let code = EndedCode::from_wire(&code).rpc_code();
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
    use crate::cloud::account::testing::{fake_cloud, token};

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
    async fn a_run_is_followed_only_while_cloud_says_pending_or_running() {
        for (state, followed) in [
            (serde_json::json!("pending"), true),
            (serde_json::json!("running"), true),
            (serde_json::json!("finished"), false),
            (serde_json::json!("ended"), false),
            (serde_json::json!("Pending"), false),
            (serde_json::json!("running "), false),
            (serde_json::json!("queued"), false),
            (serde_json::json!(""), false),
            (serde_json::json!(null), false),
            (serde_json::json!(7), false),
            (serde_json::json!(["running"]), false),
        ] {
            let body = serde_json::json!({ "state": state, "id": "run_1" });
            let cloud = fake_cloud(move |_, _| (200, body.clone()));
            let past = Some(tokio::time::Instant::now());
            let read = follow_run::<serde_json::Value>(&token(&cloud), "runs/run_1", "Run", past)
                .await
                .unwrap();
            assert_eq!(read.is_none(), followed, "{state}");
        }
        let cloud = fake_cloud(|_, _| (200, serde_json::json!({ "id": "run_1" })));
        let past = Some(tokio::time::Instant::now());
        let read = follow_run::<serde_json::Value>(&token(&cloud), "runs/run_1", "Run", past)
            .await
            .unwrap();
        assert_eq!(read, Some(serde_json::json!({ "id": "run_1" })));
    }

    #[test]
    fn a_run_that_ended_without_a_result_maps_its_code_to_one_failure() {
        let ended = |code: &str| {
            serde_json::from_value::<Settled<u8>>(
                serde_json::json!({ "state": "ended", "code": code, "message": "Said." }),
            )
            .unwrap()
            .finished()
            .unwrap_err()
            .report()
        };
        for (code, rpc) in [
            ("refused", "conflict"),
            ("busy", "unavailable"),
            ("failed", "unavailable"),
            ("Refused", "unavailable"),
            ("REFUSED", "unavailable"),
            ("refused ", "unavailable"),
            ("", "unavailable"),
        ] {
            let report = ended(code);
            assert_eq!(report.code.as_str(), rpc, "{code}");
            assert_eq!(report.message, "Said.", "{code}");
        }
        let finished = serde_json::from_value::<Settled<u8>>(
            serde_json::json!({ "state": "finished", "extra": true }),
        );
        assert!(finished.is_err(), "a finished run carries its result");
        for body in [
            serde_json::json!({ "state": "ended", "message": "m" }),
            serde_json::json!({ "state": "ended", "code": "refused" }),
            serde_json::json!({ "state": "running" }),
            serde_json::json!({ "code": "refused", "message": "m" }),
        ] {
            assert!(
                serde_json::from_value::<Settled<u8>>(body.clone()).is_err(),
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn a_longer_wait_names_its_hours() {
        let cloud = fake_cloud(|route, _| match route {
            "GET /api/cli/server-drains/drn_1" => (200, serde_json::json!({ "state": "running" })),
            other => panic!("unexpected {other}"),
        });
        let error = follow_operation_until::<serde_json::Value>(
            &token(&cloud),
            "server-drains/drn_1",
            "Draining Server web-1",
            tokio::time::Instant::now(),
            std::time::Duration::from_secs(25 * 60 * 60),
        )
        .await
        .unwrap_err();
        let StoreCallError::Refused(refused) = error else {
            panic!("{error:?}")
        };
        assert!(
            refused
                .message
                .starts_with("Stopped waiting after 25 hours. "),
            "{}",
            refused.message
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
            CALL_DEADLINE,
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
