//! Acting in Cloud from the CLI: Organizations, Organization Tokens and signed-in
//! devices, through Cloud's `/api/cli` surface; and the Config Store's `read` and
//! `write`, through `/api/config`.
//!
//! Every call carries one credential: `PLOYZ_TOKEN` when set, else this device's
//! approved sign-in. Either acts in exactly one Organization.

use std::{collections::BTreeMap, time::Duration};

use ployz_core::{MachineId, MachineName, RpcError};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::cloud_enroll::cloud_origin;
use crate::cloud_login::{
    self, CredentialStore, DEFAULT_CLOUD, LoginError, Organization, Secret, SignedIn,
};

/// How long a call that can start an operation waits for Cloud's answer. Cloud plans
/// before it answers, asking the Servers for their copies (up to about 20s when one is
/// slow), so the client's 15s default would give up on a run Cloud goes on to start.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Via {
    Cli,
    Agent,
}

/// What a Cloud call acts with.
pub(crate) enum Credential {
    /// This device's approved sign-in; its Organization can be switched.
    Device(SignedIn),
    /// An Organization Token from `PLOYZ_TOKEN`, bound to its Organization.
    Token { cloud: String, token: Secret },
}

impl Credential {
    pub(crate) fn cloud(&self) -> &str {
        match self {
            Self::Device(signed_in) => &signed_in.cloud,
            Self::Token { cloud, .. } => cloud,
        }
    }

    pub(crate) fn bearer(&self) -> &str {
        match self {
            Self::Device(signed_in) => signed_in.token().expose(),
            Self::Token { token, .. } => token.expose(),
        }
    }
}

/// `PLOYZ_TOKEN` (at [`named_cloud`], else the default) wins over the stored sign-in.
/// Never waits for a browser.
///
/// # Errors
///
/// Returns the stored sign-in's [`LoginError`] when there is no token.
pub(crate) async fn credential(
    store: &CredentialStore,
    token: Option<String>,
    cloud: Option<String>,
) -> Result<Credential, LoginError> {
    let Some(token) = token.filter(|token| !token.is_empty()) else {
        return cloud_login::signed_in(store).await.map(Credential::Device);
    };
    Ok(Credential::Token {
        cloud: named_cloud(store, true, cloud)?.unwrap_or_else(|| cloud_origin(DEFAULT_CLOUD)),
        token: Secret::new(token),
    })
}

/// The Cloud [`credential`] acts in, when anything names one: with a token, `cloud`
/// else the stored sign-in's; without, the stored sign-in's (a device acts only where
/// it signed in) else `cloud`. Whatever else talks to "the Cloud" reads this too, so
/// it never names a different one than the credential.
///
/// # Errors
///
/// Returns a credential store that cannot be read, when it is consulted.
pub(crate) fn named_cloud(
    store: &CredentialStore,
    token: bool,
    cloud: Option<String>,
) -> Result<Option<String>, LoginError> {
    let cloud = cloud.filter(|cloud| !cloud.trim().is_empty());
    let named = match cloud {
        Some(cloud) if token => Some(cloud),
        cloud => store.cloud()?.or(cloud),
    };
    Ok(named.map(|cloud| cloud_origin(&cloud)))
}

/// [`credential`] from `PLOYZ_TOKEN` and `PLOYZ_CLOUD_URL`: the one way commands
/// pick what they act with.
///
/// # Errors
///
/// Returns the stored sign-in's [`LoginError`] when there is no token.
pub(crate) async fn from_env(store: &CredentialStore) -> Result<Credential, LoginError> {
    use crate::cli::env;
    credential(
        store,
        std::env::var(env::TOKEN).ok(),
        std::env::var(env::CLOUD_URL).ok(),
    )
    .await
}

/// The Organization `credential` acts in: the device's, or the token's own.
///
/// # Errors
///
/// Returns a Cloud failure.
pub(crate) async fn acting_in(credential: &Credential) -> Result<Organization, LoginError> {
    match credential {
        Credential::Device(signed_in) => Ok(signed_in.organization.clone()),
        Credential::Token { .. } => organizations(credential)
            .await?
            .into_iter()
            .find(|entry| entry.current)
            .map(|entry| Organization {
                id: entry.id,
                slug: entry.slug,
            })
            .ok_or(LoginError::TokenRefused),
    }
}

/// One Organization the credential may act in.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct OrganizationEntry {
    pub(crate) id: String,
    pub(crate) slug: String,
    pub(crate) name: String,
    pub(crate) current: bool,
}

/// An Organization Token as listed: never its secret.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TokenEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) created_at: String,
    pub(crate) expires_at: String,
    pub(crate) expired: bool,
    pub(crate) current: bool,
    #[serde(default)]
    pub(crate) kind: TokenKind,
    pub(crate) scope: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "preset", rename_all = "snake_case")]
pub(crate) enum TokenPreset {
    Read,
    Deploy { environment: String },
    Full,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TokenKind {
    #[default]
    Cli,
    Agent,
}

impl TokenKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Agent => "agent",
        }
    }
}

/// A signed-in device of the caller's.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DeviceEntry {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) expires_at: String,
    pub(crate) current: bool,
}

/// A revoked token or device whose Clear some Servers haven't confirmed yet.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RevokingEntry {
    pub(crate) id: String,
    pub(crate) kind: CredentialKind,
    /// Servers still holding its Management Client.
    pub(crate) unconfirmed: Vec<MachineId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CredentialKind {
    /// An Organization Token (`PLOYZ_TOKEN`).
    Token,
    Device,
}

/// The Organization's tokens, the caller's signed-in devices, and revocations still to confirm.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Credentials {
    pub(crate) tokens: Vec<TokenEntry>,
    pub(crate) devices: Vec<DeviceEntry>,
    pub(crate) revoking: Vec<RevokingEntry>,
}

/// Which Servers (by Machine ID) confirmed clearing a revoked credential's Management Client.
/// Cloud refuses the credential at once either way; `unconfirmed` Servers still let its key in.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ServerClears {
    pub(crate) confirmed: Vec<MachineId>,
    pub(crate) unconfirmed: Vec<MachineId>,
}

/// This credential's own Management Capability on each Server Cloud could reach.
#[derive(Debug, Deserialize)]
pub(crate) struct ServerAccess {
    pub(crate) connections: Vec<crate::context::Connection>,
    /// Servers Cloud couldn't provision a holder on now.
    pub(crate) unreachable: Vec<MachineId>,
    /// The names enrollment gave the Organization's Servers, where Cloud holds one.
    #[serde(default)]
    pub(crate) names: BTreeMap<MachineId, String>,
}

/// A token just made: the one reply that carries its secret.
#[derive(Serialize, Deserialize)]
pub(crate) struct NewToken {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) organization: String,
    pub(crate) expires_at: String,
    pub(crate) secret: String,
    #[serde(default)]
    pub(crate) kind: TokenKind,
    pub(crate) scope: String,
}

/// A revoked token or signed-out device.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Removed {
    pub(crate) id: String,
    pub(crate) kind: CredentialKind,
}

/// The Organizations this credential may act in.
///
/// # Errors
///
/// Returns a Cloud failure.
pub(crate) async fn organizations(
    credential: &Credential,
) -> Result<Vec<OrganizationEntry>, LoginError> {
    #[derive(Deserialize)]
    struct Reply {
        organizations: Vec<OrganizationEntry>,
    }
    let reply: Reply = call(credential, Method::GET, "organizations", None).await?;
    Ok(reply.organizations)
}

/// The Organization to switch to: a slug the user typed, or one Cloud listed, sent by ID.
pub(crate) enum Target<'a> {
    Slug(&'a str),
    Listed(&'a OrganizationEntry),
}

/// Switch this device's sign-in to another of the user's Organizations.
///
/// # Errors
///
/// Returns [`LoginError::TokenBound`] for a token, [`LoginError::UnknownOrganization`]
/// when the user isn't a member, or a Cloud or store failure.
pub(crate) async fn use_organization(
    store: &CredentialStore,
    credential: &Credential,
    target: Target<'_>,
) -> Result<Organization, LoginError> {
    let Credential::Device(signed_in) = credential else {
        return Err(LoginError::TokenBound);
    };
    let (body, slug) = match target {
        Target::Slug(slug) => (serde_json::json!({ "organizationSlug": slug }), slug),
        Target::Listed(listed) => (
            serde_json::json!({ "organizationId": listed.id }),
            listed.slug.as_str(),
        ),
    };
    // better-auth's own endpoint moves the session's active Organization, checking membership.
    let response = send(
        credential,
        Method::POST,
        &format!("{}/api/auth/organization/set-active", signed_in.cloud),
        Some(&body),
    )
    .await?;
    let status = response.status();
    if matches!(status, StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN) {
        return Err(LoginError::UnknownOrganization(slug.to_owned()));
    }
    let organization: Organization = read(credential, response).await?;
    store
        .set_organization(signed_in, organization)
        .map(|signed_in| signed_in.organization)
}

/// An Organization just made: Cloud picked its slug.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NewOrganization {
    pub(crate) id: String,
    pub(crate) slug: String,
    pub(crate) name: String,
}

/// Make an Organization the user owns and switch this device's sign-in to it. Cloud
/// slugifies `slug` (else `name`) and suffixes it when taken; the user's other sign-ins
/// stay where they are.
///
/// # Errors
///
/// Returns [`LoginError::TokenBound`] for a token, Cloud's refusal of the name or slug,
/// or a Cloud or store failure.
pub(crate) async fn new_organization(
    store: &CredentialStore,
    credential: &Credential,
    name: &str,
    slug: Option<&str>,
) -> Result<NewOrganization, LoginError> {
    let Credential::Device(signed_in) = credential else {
        return Err(LoginError::TokenBound);
    };
    #[derive(Deserialize)]
    struct Reply {
        organization: NewOrganization,
    }
    let body = match slug {
        Some(slug) => serde_json::json!({ "name": name, "slug": slug }),
        None => serde_json::json!({ "name": name }),
    };
    let reply: Reply = call(credential, Method::POST, "organizations", Some(&body)).await?;
    let made = reply.organization;
    store.set_organization(
        signed_in,
        Organization {
            id: made.id.clone(),
            slug: made.slug.clone(),
        },
    )?;
    Ok(made)
}

/// The Organization's tokens and the caller's signed-in devices.
///
/// # Errors
///
/// Returns a Cloud failure.
pub(crate) async fn credentials(credential: &Credential) -> Result<Credentials, LoginError> {
    call(credential, Method::GET, "tokens", None).await
}

/// Make an Organization Token acting as the caller in the caller's Organization.
///
/// # Errors
///
/// Returns a Cloud failure; a rejected name or expiry is HTTP 422.
pub(crate) async fn new_token(
    credential: &Credential,
    name: &str,
    kind: TokenKind,
    preset: &TokenPreset,
    expires_in_days: u16,
) -> Result<NewToken, LoginError> {
    #[derive(Serialize)]
    struct Body<'a> {
        name: &'a str,
        kind: TokenKind,
        expires_in_days: u16,
        #[serde(flatten)]
        preset: &'a TokenPreset,
    }
    #[derive(Deserialize)]
    struct Reply {
        token: NewToken,
    }
    let body = serde_json::to_value(Body {
        name,
        kind,
        expires_in_days,
        preset,
    })
    .expect("a token request serializes");
    let reply: Reply = call(credential, Method::POST, "tokens", Some(&body)).await?;
    Ok(reply.token)
}

/// Revoke a token of the Organization, or sign out one of the caller's devices, then clear
/// its Management Client on each Server. Rerun on a revoked id, it retries unconfirmed Servers.
///
/// # Errors
///
/// Returns [`LoginError::UnknownCredential`] when there is no such id, or a Cloud failure.
pub(crate) async fn remove_token(
    credential: &Credential,
    id: &str,
) -> Result<(Removed, ServerClears), LoginError> {
    #[derive(Deserialize)]
    struct Reply {
        removed: Removed,
        servers: ServerClears,
    }
    match call::<Reply>(credential, Method::DELETE, &format!("tokens/{id}"), None).await {
        Ok(reply) => Ok((reply.removed, reply.servers)),
        Err(LoginError::Status { status: 404, .. }) => {
            Err(LoginError::UnknownCredential(id.to_owned()))
        }
        Err(error) => Err(error),
    }
}

/// This credential's own Management Capability on each Server of its Organization; Cloud
/// provisions a missing one, including on Servers enrolled after login.
///
/// # Errors
///
/// Returns Cloud's refusal when the credential's user is not an owner or admin, or a
/// Cloud failure.
pub(crate) async fn server_access(credential: &Credential) -> Result<ServerAccess, StoreCallError> {
    let url = format!("{}/api/cli/server-access", credential.cloud());
    let response = send(credential, Method::POST, &url, None).await?;
    notice_revision(&response);
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| cloud_login::unreachable(credential.cloud(), error))?;
    if status == StatusCode::FORBIDDEN
        && let Some(refused) = refusal(&body)
    {
        return Err(StoreCallError::Refused(refused));
    }
    Ok(answer(credential, status, &body)?)
}

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
    let body = serde_json::to_value(body).map_err(|error| LoginError::Reply(error.to_string()))?;
    let url = format!("{}/api/config/{operation}", credential.cloud());
    let lost = if operation == "write" {
        cloud_login::lost
    } else {
        cloud_login::unreachable
    };
    let response = approved_request(credential, via, Method::POST, &url, &body, approval, None)?
        .send()
        .await
        .map_err(|error| lost(credential.cloud(), error))?;
    store_answer(credential, response, lost).await
}

/// How a request that failed in flight becomes a [`LoginError`]: [`cloud_login::lost`]
/// for a write, else [`cloud_login::unreachable`].
type Lost = fn(&str, reqwest::Error) -> LoginError;

fn approved_request(
    credential: &Credential,
    via: Via,
    method: Method,
    url: &str,
    body: &serde_json::Value,
    approval: Option<&str>,
    request_key: Option<&str>,
) -> Result<reqwest::RequestBuilder, LoginError> {
    let mut request = request(credential, method, url, Some(body))?.timeout(OPERATION_TIMEOUT);
    if via == Via::Agent {
        request = request.header("x-ployz-via", "agent");
    }
    if let Some(approval) = approval {
        request = request.header("x-ployz-approval", approval);
    }
    if let Some(key) = request_key {
        request = request.header("x-ployz-request-key", key);
    }
    Ok(request)
}

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
        approved_request(credential, via, method, &url, body, approval, request_key)?
            .send()
            .await
            .map_err(|error| cloud_login::unreachable(credential.cloud(), error))
    };
    let response = match sent.await {
        Ok(response) => response,
        Err(error) => return (Err(error.into()), None),
    };
    let revision = revision_of(response.headers());
    (
        store_answer(credential, response, cloud_login::unreachable).await,
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
        tokio::time::Instant::now() + crate::handlers::mcp::CALL_DEADLINE,
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
            StoreCallError::Refused(RpcError {
                code: ployz_core::RpcErrorCode::Unavailable,
                message: format!(
                    "Stopped waiting after {} minutes. {doing} keeps running in Ployz Cloud and \
                     finishes on its own; this command no longer reports it.",
                    crate::handlers::mcp::CALL_DEADLINE.as_secs() / 60
                ),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            })
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

/// The Store's answer, its refusal verbatim, or why Cloud failed first.
async fn store_answer<T: DeserializeOwned>(
    credential: &Credential,
    response: reqwest::Response,
    lost: Lost,
) -> Result<T, StoreCallError> {
    let status = response.status();
    notice_revision(&response);
    let bytes = response
        .bytes()
        .await
        .map_err(|error| lost(credential.cloud(), error))?;
    if !status.is_success() {
        if let Some(refused) = refusal(&bytes) {
            return Err(StoreCallError::Refused(refused));
        }
        if !matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && let Some(refused) = public_error(&bytes)
        {
            return Err(StoreCallError::Refused(refused));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(LoginError::Unsupported(credential.cloud().to_owned()).into());
        }
    }
    Ok(answer(credential, status, &bytes)?)
}

/// Why a Server didn't count as reachable when Cloud tried it. One that answered
/// refuses the whole forget, so it never appears here.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reach {
    /// Cloud tried and it didn't answer in time.
    DidntAnswer,
    /// Cloud holds no connection to try, such as a founder that never published one.
    NoConnection,
}

/// One Server of the Organization as Cloud found it when it tried. Its name is the
/// one enrollment assigned, else its ID.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ObservedServer {
    pub(crate) id: MachineId,
    pub(crate) name: String,
    pub(crate) reach: Reach,
}

/// What forgetting the Organization's Servers lets go of: each Server with what Cloud
/// observed of it, and each Volume a Deploy put on them.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ForgetCheck {
    pub(crate) organization: String,
    pub(crate) servers: Vec<ObservedServer>,
    pub(crate) volumes: Vec<ployz_store::AppliedVolume>,
}

/// What forgetting the Organization's Servers let go of, with the Deployments that
/// might still have run, cancelled.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ServersForgotten {
    #[serde(flatten)]
    pub(crate) check: ForgetCheck,
    pub(crate) cancelled: Vec<ployz_store::DeploymentId>,
}

/// What forgetting the credential's Organization's Servers would let go of. Cloud
/// tries each Server first.
///
/// # Errors
///
/// Returns Cloud's refusal (`conflict` while a Server answers or one is still
/// joining, `forbidden` for a member who isn't an owner or admin), or a Cloud failure.
pub(crate) async fn check_forget_servers(
    credential: &Credential,
) -> Result<ForgetCheck, StoreCallError> {
    let url = format!("{}/api/cli/forget-servers", credential.cloud());
    store_answer(
        credential,
        send(credential, Method::GET, &url, None).await?,
        cloud_login::unreachable,
    )
    .await
}

/// Forget the Servers of the credential's Organization, named `organization`: Cloud
/// tries each again and forgets them only when none answers and nothing changed.
///
/// # Errors
///
/// As [`check_forget_servers`], `invalid_argument` for another Organization than the
/// credential's, and `approval_required` until a human approves it.
pub(crate) async fn forget_servers(
    credential: &Credential,
    organization: &str,
    approval: Option<&str>,
) -> Result<ServersForgotten, StoreCallError> {
    let body = serde_json::json!({ "organization": organization });
    approved_answer(credential, Method::POST, "forget-servers", &body, approval).await
}

/// What `org rm` did: whether the Organization is gone, and which Servers confirmed
/// clearing Cloud's key and every device key. Until all do, it stays, disabled.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct OrganizationRemoval {
    pub(crate) organization: String,
    pub(crate) removed: bool,
    pub(crate) servers: ServerClears,
}

/// Remove the credential's Organization, by its ID, once it has no Project.
/// Rerun while a Server hasn't confirmed, it retries the Clears.
///
/// # Errors
///
/// Returns Cloud's refusal (`conflict` while it has a Project, `invalid_argument`
/// for another Organization than the credential's, `approval_required` until a human
/// approves it), or a Cloud failure.
pub(crate) async fn remove_organization(
    credential: &Credential,
    id: &str,
    approval: Option<&str>,
) -> Result<OrganizationRemoval, StoreCallError> {
    let path = format!("organizations/{id}");
    approved_answer(
        credential,
        Method::DELETE,
        &path,
        &serde_json::json!({}),
        approval,
    )
    .await
}

/// What Cloud's removal of a Server did.
#[derive(Debug, Deserialize)]
pub(crate) struct CloudRemoval {
    /// Why the reset didn't finish, if it didn't: Cloud then keeps its hold.
    pub(crate) reset_warning: Option<String>,
    pub(crate) release: Release,
}

/// What became of Cloud's hold once the Server was gone.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Release {
    /// Other Servers keep the Cluster; Cloud dropped only this one.
    OthersRemain,
    /// It was the last: Cloud forgot the Cluster, and the next Server founds a new one.
    Released,
    /// Cloud kept its hold, for `reason`.
    Kept { reason: String },
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum RemovalRun {
    Succeeded(CloudRemoval),
}

/// How long `server rm` and `server forget` wait on Cloud's removal before it stops watching.
const REMOVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(600);

/// Have Cloud remove Server `machine` of the credential's Organization, resetting it
/// and accepting exactly `confirmation`'s Data Loss. It is the same durable removal the
/// dashboard starts, which Cloud runs under its own connection; [`follow_removal`]
/// waits for it. `approval` is the id of the human's approval, when Cloud asked for one.
///
/// # Errors
///
/// Returns Cloud's refusal (`not_found` for a Server it doesn't reach, `invalid_argument`
/// when the confirmation misses fresh Data Loss, `approval_required`), or a Cloud failure.
pub(crate) async fn start_removal(
    credential: &Credential,
    machine: &MachineId,
    confirmation: &ployz_core::DataLossConfirmation,
    approval: Option<&str>,
) -> Result<String, StoreCallError> {
    start_run(
        credential,
        Method::DELETE,
        &format!("servers/{machine}"),
        &serde_json::json!({ "confirm_data_loss": confirmation }),
        approval,
    )
    .await
}

/// Have Cloud forget Server `machine` of the credential's Organization: take it out of
/// the Cluster without a reset. Cloud runs it as the same durable removal
/// [`follow_removal`] waits for. `approval` is the id of the human's approval, when
/// Cloud asked for one.
///
/// # Errors
///
/// Returns Cloud's refusal (`not_found` for a Server it doesn't reach,
/// `approval_required`), or a Cloud failure.
pub(crate) async fn start_forget(
    credential: &Credential,
    machine: &MachineId,
    approval: Option<&str>,
) -> Result<String, StoreCallError> {
    start_run(
        credential,
        Method::POST,
        &format!("servers/{machine}/forget"),
        &serde_json::json!({}),
        approval,
    )
    .await
}

/// Wait for Cloud's removal `id` of Server `name` to settle. Cloud drops its row for
/// the Server, and when it saw the last one reset, lets go of the Cluster.
///
/// # Errors
///
/// Returns Cloud's refusal (`unavailable` when the removal failed), `unavailable` when it
/// hasn't settled within [`REMOVAL_WAIT`], or a Cloud failure.
pub(crate) async fn follow_removal(
    credential: &Credential,
    name: &MachineName,
    id: &str,
) -> Result<CloudRemoval, StoreCallError> {
    let deadline = tokio::time::Instant::now() + REMOVAL_WAIT;
    let doing = format!("Removing Server {name}");
    match follow_run(
        credential,
        &format!("server-removals/{id}"),
        &doing,
        Some(deadline),
    )
    .await?
    {
        Some(RemovalRun::Succeeded(removal)) => Ok(removal),
        None => Err(StoreCallError::Refused(RpcError {
            code: ployz_core::RpcErrorCode::Unavailable,
            message: format!(
                "Cloud is still removing Server {name}; it finishes on its own. \
                 The dashboard's Servers page shows when it's done."
            ),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        })),
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
    store_answer(
        credential,
        send(credential, method, &url, body).await?,
        cloud_login::unreachable,
    )
    .await
}

/// The most compressed source one upload may hold, the same as Cloud's cap. It stays
/// under the 100 MB request body Cloudflare passes, in front of ployz.dev or a Tunnel.
pub(crate) const UPLOAD_LIMIT: usize = 95 * 1024 * 1024;
/// How long one upload may take, sending included: 95 MiB at about 1.3 Mbit/s.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Keep `archive`, a gzipped tar of a source directory, in Cloud as the upload of
/// Deployment `deployment`, which the CLI admits next. An archive over
/// [`UPLOAD_LIMIT`] is refused here, before it is sent.
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
    if archive.len() > UPLOAD_LIMIT {
        return Err(StoreCallError::Refused(too_big(&format!(
            "The source is {mib} MiB compressed, over the {} MiB upload limit.",
            UPLOAD_LIMIT / 1024 / 1024
        ))));
    }
    let url = format!("{}/api/config/upload/{deployment}", credential.cloud());
    let response = cloud_login::http()?
        .post(&url)
        .timeout(UPLOAD_TIMEOUT)
        .bearer_auth(credential.bearer())
        .header(reqwest::header::CONTENT_TYPE, "application/gzip")
        .body(archive)
        .send()
        .await
        .map_err(|error| cloud_login::lost(credential.cloud(), error))?;
    // A proxy in front of Cloud with a lower cap refuses the body before Cloud sees it.
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return Err(StoreCallError::Refused(too_big(&format!(
            "Cloud refused the {mib} MiB compressed source as too large."
        ))));
    }
    store_answer::<serde_json::Value>(credential, response, cloud_login::lost).await?;
    Ok(())
}

/// The refusal of a source too large to upload: `why`, then how to deploy it anyway.
fn too_big(why: &str) -> RpcError {
    RpcError {
        code: ployz_core::RpcErrorCode::InvalidArgument,
        message: format!(
            "{why} Deploy from GitHub instead, or remove large files from the directory. In a Git checkout, ployz up leaves out files Git ignores, so add ones such as node_modules to .gitignore."
        ),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
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
    let response = send(credential, method, &url, body).await?;
    let status = response.status();
    notice_revision(&response);
    let body = response
        .bytes()
        .await
        .map_err(|error| cloud_login::unreachable(credential.cloud(), error))?;
    if read_only && status == StatusCode::NOT_FOUND && refusal(&body).is_none() {
        return Err(LoginError::Unsupported(credential.cloud().to_owned()));
    }
    answer(credential, status, &body)
}

/// The manifest revision each Cloud named on its latest `/api/cli` reply, by origin.
static REVISION: std::sync::Mutex<std::collections::BTreeMap<String, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

pub(crate) const REVISION_HEADER: &str = "x-ployz-manifest";

/// Keep the revision a reply names, so a command can refresh the published
/// commands after it prints when Cloud moved on.
pub(crate) fn notice_revision(response: &reqwest::Response) {
    if let Some(revision) = revision_of(response.headers()) {
        REVISION
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(response.url().origin().ascii_serialization(), revision);
    }
}

fn revision_of(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(REVISION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

/// The revision `cloud`'s latest `/api/cli` reply named, if one did.
pub(crate) fn revision_seen(cloud: &str) -> Option<String> {
    let origin = reqwest::Url::parse(cloud)
        .ok()?
        .origin()
        .ascii_serialization();
    REVISION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&origin)
        .cloned()
}

/// Cloud's refusal of a call, as it sent it.
pub(crate) fn refusal(body: &[u8]) -> Option<RpcError> {
    #[derive(Deserialize)]
    struct Refusal {
        error: RpcError,
    }
    serde_json::from_slice::<Refusal>(body)
        .ok()
        .map(|refusal| refusal.error)
}

/// A Cloud route's `PublicError`, as the refusal it stands for.
pub(crate) fn public_error(body: &[u8]) -> Option<RpcError> {
    #[derive(Deserialize)]
    #[serde(tag = "_tag")]
    enum Public {
        PublicError { code: String, message: String },
    }
    let Public::PublicError { code, message } = serde_json::from_slice(body).ok()?;
    let code = match code.as_str() {
        "CONFLICT" => ployz_core::RpcErrorCode::Conflict,
        "NOT_FOUND" => ployz_core::RpcErrorCode::NotFound,
        "VALIDATION_FAILED" => ployz_core::RpcErrorCode::InvalidArgument,
        "UNAUTHORIZED" | "FORBIDDEN" => ployz_core::RpcErrorCode::Unauthenticated,
        _ => ployz_core::RpcErrorCode::Internal,
    };
    Some(RpcError {
        code,
        message,
        details: serde_json::Value::Null,
        cause: Vec::new(),
    })
}

async fn send(
    credential: &Credential,
    method: Method,
    url: &str,
    body: Option<&serde_json::Value>,
) -> Result<reqwest::Response, LoginError> {
    request(credential, method, url, body)?
        .send()
        .await
        .map_err(|error| cloud_login::unreachable(credential.cloud(), error))
}

fn request(
    credential: &Credential,
    method: Method,
    url: &str,
    body: Option<&serde_json::Value>,
) -> Result<reqwest::RequestBuilder, LoginError> {
    let mut request = cloud_login::http()?
        .request(method, url)
        .bearer_auth(credential.bearer());
    if let Some(body) = body {
        request = request.json(body);
    }
    Ok(request)
}

/// Decode a success; turn a refused credential into the error that names its fix.
async fn read<T: DeserializeOwned>(
    credential: &Credential,
    response: reqwest::Response,
) -> Result<T, LoginError> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| cloud_login::unreachable(credential.cloud(), error))?;
    answer(credential, status, &body)
}

fn answer<T: DeserializeOwned>(
    credential: &Credential,
    status: StatusCode,
    body: &[u8],
) -> Result<T, LoginError> {
    match (status, credential) {
        (status, _) if status.is_success() => cloud_login::decode(body.to_vec()),
        (StatusCode::UNAUTHORIZED, Credential::Device(_)) => Err(LoginError::Ended),
        (StatusCode::UNAUTHORIZED, Credential::Token { .. }) => Err(LoginError::TokenRefused),
        // Cloud refuses a member's call in the RPC error vocabulary (`claim_required`);
        // only a bare 403 means the sign-in left its Organization.
        (StatusCode::FORBIDDEN, Credential::Device(signed_in)) if refusal(body).is_none() => {
            Err(LoginError::NotMember(signed_in.organization.slug.clone()))
        }
        (status, _) => Err(LoginError::Status {
            status: status.as_u16(),
            body: String::from_utf8_lossy(body).into_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_login::tests::fake_cloud;

    /// A device signed in to `cloud` as Organization `acme`.
    fn signed_in_store(cloud: &str) -> (tempfile::TempDir, CredentialStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::beside(&dir.path().join("config.yaml"));
        let stored = serde_json::json!({
            "state": "signed_in",
            "cloud": cloud,
            "token": "session-token",
            "account": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
            "organization": { "id": "o1", "slug": "acme" },
        });
        std::fs::write(dir.path().join("cloud.json"), stored.to_string()).unwrap();
        (dir, store)
    }

    fn token(cloud: &str) -> Credential {
        Credential::Token {
            cloud: cloud.to_owned(),
            token: Secret::new("ployz_secret".to_owned()),
        }
    }

    #[test]
    fn a_device_acts_where_it_signed_in_and_a_token_where_the_cloud_url_says() {
        let (dir, store) = signed_in_store("http://signed-in.test");
        let other = || Some("http://other.test".to_owned());
        assert_eq!(
            named_cloud(&store, false, other()).unwrap().as_deref(),
            Some("http://signed-in.test")
        );
        assert_eq!(
            named_cloud(&store, true, other()).unwrap().as_deref(),
            Some("http://other.test")
        );
        let empty = CredentialStore::beside(&dir.path().join("elsewhere/config.yaml"));
        assert_eq!(
            named_cloud(&empty, false, other()).unwrap().as_deref(),
            Some("http://other.test"),
            "signed out, the Cloud URL still names where commands are read"
        );
    }

    #[tokio::test]
    async fn ployz_token_wins_over_the_stored_sign_in() {
        let (_dir, store) = signed_in_store("http://cloud.test");
        let chosen = credential(&store, Some("ployz_secret".to_owned()), None)
            .await
            .unwrap();
        assert!(matches!(&chosen, Credential::Token { cloud, .. } if cloud == "http://cloud.test"));
        let chosen = credential(&store, Some(String::new()), None).await.unwrap();
        assert!(matches!(chosen, Credential::Device(_)));
    }

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

    #[tokio::test]
    async fn a_refused_credential_names_its_fix() {
        let cloud = fake_cloud(|route, _| match route {
            "GET /api/cli/tokens" => (401, serde_json::json!({ "code": "UNAUTHORIZED" })),
            _ => (403, serde_json::json!({ "code": "FORBIDDEN" })),
        });
        let (_dir, store) = signed_in_store(&cloud);
        let device = credential(&store, None, None).await.unwrap();
        assert!(matches!(credentials(&device).await, Err(LoginError::Ended)));
        assert!(matches!(
            credentials(&token(&cloud)).await,
            Err(LoginError::TokenRefused)
        ));
        let error = organizations(&device).await.unwrap_err();
        assert!(
            matches!(&error, LoginError::NotMember(slug) if slug == "acme"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn org_use_moves_the_device_among_its_own_organizations() {
        let cloud = fake_cloud(|route, body| {
            let body: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            let beta = body == serde_json::json!({ "organizationSlug": "beta" })
                || body == serde_json::json!({ "organizationId": "o2" });
            match route {
                "POST /api/auth/organization/set-active" if beta => (
                    200,
                    serde_json::json!({ "id": "o2", "slug": "beta", "name": "Beta", "members": [] }),
                ),
                _ => (403, serde_json::json!({ "message": "not a member" })),
            }
        });
        let (_dir, store) = signed_in_store(&cloud);
        let device = credential(&store, None, None).await.unwrap();
        let error = use_organization(&store, &device, Target::Slug("gamma"))
            .await
            .unwrap_err();
        assert!(
            matches!(error, LoginError::UnknownOrganization(_)),
            "{error}"
        );
        let error = use_organization(&store, &token(&cloud), Target::Slug("beta"))
            .await
            .unwrap_err();
        assert!(matches!(error, LoginError::TokenBound), "{error}");

        let organization = use_organization(&store, &device, Target::Slug("beta"))
            .await
            .unwrap();
        assert_eq!(organization.slug, "beta");
        let stored = cloud_login::signed_in(&store).await.unwrap();
        assert_eq!(stored.organization.id, "o2");

        // A listed Organization goes by its ID, so a slug Cloud no longer knows still moves.
        let listed = OrganizationEntry {
            id: "o2".to_owned(),
            slug: "beta-renamed".to_owned(),
            name: "Beta".to_owned(),
            current: false,
        };
        let organization = use_organization(&store, &device, Target::Listed(&listed))
            .await
            .unwrap();
        assert_eq!(organization.id, "o2");
    }

    #[tokio::test]
    async fn org_new_moves_the_device_into_the_organization_cloud_made() {
        let cloud = fake_cloud(|route, body| match route {
            "POST /api/cli/organizations" if body.contains("\"slug\"") => (
                422,
                serde_json::json!({ "error": {
                    "code": "invalid_argument",
                    "message": "An Organization needs a name of 1 to 64 characters, and a slug of 1 to 64 letters, digits or hyphens.",
                    "details": null,
                }}),
            ),
            "POST /api/cli/organizations" => {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(body).unwrap(),
                    serde_json::json!({ "name": "Acme Corp" })
                );
                (
                    200,
                    serde_json::json!({ "organization": {
                        "id": "o2", "slug": "acme-corp-1a2b3c4d", "name": "Acme Corp",
                    }}),
                )
            }
            other => panic!("unexpected {other}"),
        });
        let (_dir, store) = signed_in_store(&cloud);
        let device = credential(&store, None, None).await.unwrap();
        let error = new_organization(&store, &token(&cloud), "Acme Corp", None)
            .await
            .unwrap_err();
        assert!(matches!(error, LoginError::TokenBound), "{error}");

        let error = new_organization(&store, &device, "Acme Corp", Some("!!!"))
            .await
            .unwrap_err();
        let LoginError::Status { status: 422, body } = &error else {
            panic!("expected the refusal, got {error}");
        };
        assert!(refusal(body.as_bytes()).is_some());
        assert_eq!(
            cloud_login::signed_in(&store)
                .await
                .unwrap()
                .organization
                .slug,
            "acme",
            "a refused name leaves the device where it was"
        );

        let made = new_organization(&store, &device, "Acme Corp", None)
            .await
            .unwrap();
        assert_eq!(made.slug, "acme-corp-1a2b3c4d");
        let stored = cloud_login::signed_in(&store).await.unwrap();
        assert_eq!(
            stored.organization,
            Organization {
                id: "o2".to_owned(),
                slug: "acme-corp-1a2b3c4d".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn cloud_answers_become_their_own_errors() {
        let cloud = fake_cloud(|_, _| (404, serde_json::json!({ "code": "NOT_FOUND" })));
        let credential = token(&cloud);
        let error = remove_token(&credential, "t1").await.unwrap_err();
        assert!(matches!(error, LoginError::UnknownCredential(_)), "{error}");
        // A Cloud without the CLI surface (or with it switched off).
        let error = credentials(&credential).await.unwrap_err();
        assert!(matches!(error, LoginError::Unsupported(_)), "{error}");
    }

    #[tokio::test]
    async fn an_archive_over_the_limit_is_refused_before_it_is_sent() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let cloud = format!("http://{}", listener.local_addr().unwrap());
        let archive = vec![0; UPLOAD_LIMIT + 1];
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
    async fn server_access_reads_dialable_connections() {
        let capability = format!("ployz1:{}", "A".repeat(86));
        let reply = serde_json::json!({
            "connections": [{ "machine_id": "a".repeat(32), "management": capability }],
            "unreachable": ["b".repeat(32)],
        });
        let cloud = fake_cloud(move |route, _| match route {
            "POST /api/cli/server-access" => (200, reply.clone()),
            other => panic!("unexpected {other}"),
        });
        let access = server_access(&token(&cloud)).await.unwrap();
        assert_eq!(access.connections.len(), 1);
        assert_eq!(
            access.unreachable,
            [MachineId::parse("b".repeat(32)).unwrap()]
        );
        assert!(!format!("{access:?}").contains(&capability));
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

    #[tokio::test]
    async fn a_new_token_carries_its_secret_once() {
        let cloud = fake_cloud(|route, body| match route {
            "POST /api/cli/tokens" => {
                let body: serde_json::Value = serde_json::from_str(body).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({
                        "name": "ci", "kind": "agent", "expires_in_days": 30,
                        "preset": "deploy", "environment": "staging",
                    })
                );
                (
                    200,
                    serde_json::json!({ "token": {
                        "id": "t1", "name": "ci", "organization": "acme",
                        "expires_at": "2026-10-29T00:00:00.000Z", "secret": "ployz_new", "kind": "agent",
                        "scope": "Deploy staging",
                    } }),
                )
            }
            other => panic!("unexpected {other}"),
        });
        let deploy = TokenPreset::Deploy {
            environment: "staging".into(),
        };
        let made = new_token(&token(&cloud), "ci", TokenKind::Agent, &deploy, 30)
            .await
            .unwrap();
        assert_eq!(made.secret, "ployz_new");
        assert_eq!(made.kind, TokenKind::Agent);
        assert_eq!(made.scope, "Deploy staging");
    }
}
