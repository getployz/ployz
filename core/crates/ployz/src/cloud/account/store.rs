//! The Config Store's `read` and `write` over `/api/config`, the one client that reaches
//! either Store, and uploading a source archive for a Deployment.

use std::time::Duration;

use ployz_core::{RpcError, RpcErrorCode, cloud_contract};
use ployz_store::{Actor, Ask, Command, ConfigStore, Query, Tell, Trusted, View, Written};
use reqwest::{Method, StatusCode};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::{
    credential::Credential,
    reply::{StoreCallError, refusal, store_answer, stored},
};
use crate::cloud::http::{self, Effect, Reply, Via};
use crate::cloud::login::LoginError;

/// The Organization of the hidden in-process Store.
pub(crate) const LOCAL_ORGANIZATION: &str = "local";

/// Whether commands use the hidden in-process Store (`PLOYZ_STORE`).
pub(crate) fn local_mode() -> bool {
    std::env::var_os(crate::env::STORE).is_some()
}

/// A route under `/api/config` that takes a JSON body.
#[derive(Clone, Copy, Debug)]
pub(super) enum Operation {
    /// Takes a Query and answers its View.
    Read,
    /// Takes a Command and answers what it wrote.
    Write,
    /// Takes a `SealingTargetsQuery` and answers how this CLI seals its secrets.
    Sealing,
}

impl Operation {
    const fn path(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Sealing => "sealing",
        }
    }

    const fn effect(self) -> Effect {
        match self {
            Self::Write => Effect::Write,
            Self::Read | Self::Sealing => Effect::Read,
        }
    }
}

/// The Config Store one call reaches: the hidden in-process one as an Actor, or Cloud's
/// as a credential.
#[derive(Clone, Copy)]
pub(crate) enum StoreClient<'a> {
    /// Answers without awaiting, so a caller outside a runtime can poll it once.
    Local(&'a ConfigStore, &'a Actor),
    Cloud(&'a Credential, Via),
}

impl StoreClient<'_> {
    /// Answer `query` as its own view. `seen` is what this CLI observed, which only the
    /// in-process Store takes; Cloud gathers its own.
    ///
    /// # Errors
    ///
    /// The Store's refusal, or a Cloud failure.
    pub(crate) async fn read<Q: Ask>(
        self,
        query: &Q,
        seen: &Trusted,
    ) -> Result<Q::View, StoreCallError> {
        match self {
            Self::Local(store, who) => store
                .read_trusted(who, query, seen)
                .map_err(StoreCallError::Refused),
            Self::Cloud(credential, via) => {
                let view: View =
                    config_store(credential, via, Operation::Read, &query.to_query(), None).await?;
                Q::view(view).map_err(StoreCallError::Refused)
            }
        }
    }

    /// Apply `command` as its own result. `trusted` is what this CLI observed, which only
    /// the in-process Store takes; `approval` answers Cloud's ask for one.
    ///
    /// # Errors
    ///
    /// The Store's refusal, or a Cloud failure.
    pub(crate) async fn write<C: Tell>(
        self,
        command: &C,
        trusted: &Trusted,
        approval: Option<&str>,
    ) -> Result<C::Written, StoreCallError> {
        match self {
            Self::Local(store, who) => store
                .write_trusted(who, command, trusted)
                .map_err(StoreCallError::Refused),
            Self::Cloud(credential, via) => {
                let written: Written = config_store(
                    credential,
                    via,
                    Operation::Write,
                    &command.to_command(),
                    approval,
                )
                .await?;
                C::written(written).map_err(StoreCallError::Refused)
            }
        }
    }

    /// [`Self::read`] of a whole Query, as JSON: Cloud's answer passes through as it came,
    /// fields this CLI doesn't know included.
    ///
    /// # Errors
    ///
    /// As [`Self::read`].
    pub(crate) async fn read_json(self, query: &Query) -> Result<Value, StoreCallError> {
        match self {
            Self::Local(store, who) => store
                .read(who, query)
                .and_then(|view| json(&view))
                .map_err(StoreCallError::Refused),
            Self::Cloud(credential, via) => {
                config_store(credential, via, Operation::Read, query, None).await
            }
        }
    }

    /// [`Self::write`] of a whole Command, as JSON the way [`Self::read_json`] answers.
    ///
    /// # Errors
    ///
    /// As [`Self::write`].
    pub(crate) async fn write_json(
        self,
        command: &Command,
        approval: Option<&str>,
    ) -> Result<Value, StoreCallError> {
        match self {
            Self::Local(store, who) => store
                .write(who, command)
                .and_then(|written| json(&written))
                .map_err(StoreCallError::Refused),
            Self::Cloud(credential, via) => {
                config_store(credential, via, Operation::Write, command, approval).await
            }
        }
    }
}

fn json(answer: &impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(answer).map_err(|error| RpcError::caused(RpcErrorCode::Internal, &error))
}

/// What Cloud answers about sealing `query`'s secrets: `None` from a Cloud that predates
/// the route.
///
/// # Errors
///
/// As [`config_store`].
pub(crate) async fn sealing<T: DeserializeOwned>(
    credential: &Credential,
    via: Via,
    query: &ployz_store::SealingTargetsQuery,
) -> Result<Option<T>, StoreCallError> {
    config_store_if_routed(credential, via, Operation::Sealing, query).await
}

/// Call the Config Store's `operation` in the credential's Organization with `body`, what
/// the operation takes.
///
/// # Errors
///
/// Returns the Store's refusal, or a Cloud failure; a Cloud without the Store is
/// [`LoginError::Unsupported`].
pub(super) async fn config_store<T: DeserializeOwned>(
    credential: &Credential,
    via: Via,
    operation: Operation,
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
pub(super) async fn config_store_if_routed<T: DeserializeOwned>(
    credential: &Credential,
    via: Via,
    operation: Operation,
    body: &impl Serialize,
) -> Result<Option<T>, StoreCallError> {
    let reply = config_exchange(credential, via, operation, body, None).await?;
    let unrouted = reply.status == StatusCode::NOT_FOUND
        && refusal(&reply.body).is_none()
        && crate::cloud::reply::public_error(&reply.body)
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
    operation: Operation,
    body: &impl Serialize,
    approval: Option<&str>,
) -> Result<Reply, StoreCallError> {
    let body = serde_json::to_value(body).map_err(|error| LoginError::Reply(error.to_string()))?;
    if matches!(operation, Operation::Write) {
        refuse_unsealed_logins(&body)?;
    }
    let url = format!("{}/api/config/{}", credential.cloud(), operation.path());
    let request =
        http::approved_request(credential, via, Method::POST, &url, &body, approval, None)?;
    Ok(http::exchange(credential.cloud(), request, operation.effect()).await?)
}

/// Refuses a write before it leaves for Cloud unless each registry login it sets is
/// one this CLI sealed, kept or removed, by any path or key the Store reads one from.
/// Every Store write to Cloud, typed or JSON, from a command or an agent tool, passes
/// here, so sealing is the only way a login leaves.
fn refuse_unsealed_logins(body: &Value) -> Result<(), StoreCallError> {
    let command = Command::decode(body.clone()).map_err(|error| {
        StoreCallError::Refused(RpcError::new(
            RpcErrorCode::Internal,
            format!("this CLI could not read its own write: {error}"),
        ))
    })?;
    command
        .refuse_unsealed_logins()
        .map_err(StoreCallError::Refused)
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
    use crate::cloud::account::testing::{fake_cloud, token};

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
