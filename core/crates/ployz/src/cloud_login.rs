//! Signing this device in to Cloud: RFC 8628 device authorization against Cloud's
//! better-auth, and the one stored sign-in every Cloud command picks up.
//!
//! The sign-in lives next to the Ployz config as `cloud.json` (mode 0600). A pending
//! code is stored too, so `ployz login --wait`, a repeated `ployz login`, or any
//! other command can finish a sign-in a browser approved in the meantime.

use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The Cloud a sign-in goes to when none is named or stored.
pub(crate) const DEFAULT_CLOUD: &str = "ployz.dev";
/// Cloud's device-authorization client id for this CLI.
const CLIENT_ID: &str = "ployz-cli";
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// RFC 8628 §3.5: a `slow_down` answer adds five seconds to the polling interval.
const SLOW_DOWN: Duration = Duration::from_secs(5);

/// Failures signing in to Cloud or reading the stored sign-in.
#[derive(Debug, Error)]
pub(crate) enum LoginError {
    #[error("could not reach Cloud at {cloud}: {detail}")]
    Unreachable { cloud: String, detail: String },
    #[error("Cloud at {0} does not offer CLI access")]
    Unsupported(String),
    #[error("Cloud answered HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("Cloud sent an unexpected reply: {0}")]
    Reply(String),
    #[error("could not use the sign-in stored at {}: {source}", .path.display())]
    Store { path: PathBuf, source: io::Error },
    #[error("the sign-in stored at {} is unreadable: {source}", .path.display())]
    Corrupt {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("signed in to {signed_in}, not {requested}; run ployz logout first")]
    OtherCloud {
        signed_in: String,
        requested: String,
    },
    #[error("not signed in to Cloud")]
    SignedOut,
    #[error("the sign-in code expired before it was approved")]
    Expired,
    #[error("the sign-in was denied in the browser")]
    Denied,
    #[error("Cloud ended this device's sign-in")]
    Ended,
    #[error("sign-in is waiting for approval at {url}")]
    AwaitingApproval { url: String },
    #[error(
        "Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, or its maker left its Organization"
    )]
    TokenRefused,
    #[error("this sign-in's Organization {0} is no longer one of yours")]
    NotMember(String),
    #[error("PLOYZ_TOKEN acts only in the Organization it was made in")]
    TokenBound,
    #[error("no Organization {0} of yours")]
    UnknownOrganization(String),
    #[error("no token or signed-in device {0} in this Organization")]
    UnknownCredential(String),
    #[error("Cloud at {0} has no billing: it is self-hosted")]
    NoBilling(String),
    #[error("this Organization already holds Pro")]
    AlreadyPro,
}

/// A bearer that never prints.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Secret(String);

impl Secret {
    pub(crate) fn new(secret: String) -> Self {
        Self(secret)
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret(..)")
    }
}

/// The Cloud account a device signed in as.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Account {
    pub(crate) id: String,
    pub(crate) email: String,
    pub(crate) name: String,
}

/// The Organization a device's sign-in acts in.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Organization {
    pub(crate) id: String,
    pub(crate) slug: String,
}

/// A code waiting for a signed-in browser to approve it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub(crate) cloud: String,
    device_code: Secret,
    pub(crate) code: String,
    /// The approval page with the code filled in.
    pub(crate) url: String,
    /// Unix seconds.
    expires_at: u64,
    interval_secs: u64,
}

impl Pending {
    /// Seconds until the code expires; zero once it has.
    pub(crate) fn expires_in(&self) -> u64 {
        self.expires_at.saturating_sub(now())
    }
}

/// An approved sign-in: the device's Cloud session and who it acts as.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SignedIn {
    pub(crate) cloud: String,
    token: Secret,
    pub(crate) account: Account,
    pub(crate) organization: Organization,
}

impl SignedIn {
    pub(crate) fn token(&self) -> &Secret {
        &self.token
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Stored {
    Pending(Pending),
    SignedIn(SignedIn),
}

/// Where this device keeps its Cloud sign-in: beside the Ployz config.
#[derive(Clone, Debug)]
pub(crate) struct CredentialStore {
    path: PathBuf,
}

impl CredentialStore {
    pub(crate) fn beside(config: &Path) -> Self {
        Self {
            path: config.with_file_name("cloud.json"),
        }
    }

    fn load(&self) -> Result<Option<Stored>, LoginError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|source| LoginError::Corrupt {
                        path: self.path.clone(),
                        source,
                    })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(self.failed(source)),
        }
    }

    fn save(&self, stored: &Stored) -> Result<(), LoginError> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|source| self.failed(source))?;
        // A named temp file is created 0600, so the bearer is never world-readable.
        let mut file =
            tempfile::NamedTempFile::new_in(dir).map_err(|source| self.failed(source))?;
        serde_json::to_writer_pretty(&mut file, stored)
            .map_err(|source| self.failed(io::Error::other(source)))?;
        file.persist(&self.path)
            .map_err(|error| self.failed(error.error))?;
        Ok(())
    }

    fn clear(&self) -> Result<(), LoginError> {
        match std::fs::remove_file(&self.path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(self.failed(error)),
            _ => Ok(()),
        }
    }

    fn failed(&self, source: io::Error) -> LoginError {
        LoginError::Store {
            path: self.path.clone(),
            source,
        }
    }

    /// Record the Organization a signed-in device now acts in.
    pub(crate) fn set_organization(
        &self,
        signed_in: &SignedIn,
        organization: Organization,
    ) -> Result<SignedIn, LoginError> {
        let signed_in = SignedIn {
            organization,
            ..signed_in.clone()
        };
        self.save(&Stored::SignedIn(signed_in.clone()))?;
        Ok(signed_in)
    }

    /// The Cloud the stored sign-in or pending code belongs to.
    pub(crate) fn cloud(&self) -> Result<Option<String>, LoginError> {
        Ok(self.load()?.map(|stored| match stored {
            Stored::Pending(pending) => pending.cloud,
            Stored::SignedIn(signed_in) => signed_in.cloud,
        }))
    }
}

/// The approved sign-in other commands act with. Never waits for a browser: a pending
/// code is polled once, and is still pending → [`LoginError::AwaitingApproval`].
///
/// # Errors
///
/// Returns [`LoginError::SignedOut`], [`LoginError::Expired`] or [`LoginError::Denied`]
/// when there is nothing to act with, or a Cloud or store failure.
pub(crate) async fn signed_in(store: &CredentialStore) -> Result<SignedIn, LoginError> {
    match store.load()? {
        None => Err(LoginError::SignedOut),
        Some(Stored::SignedIn(signed_in)) => Ok(signed_in),
        Some(Stored::Pending(pending)) => {
            let url = pending.url.clone();
            poll_once(store, &http()?, pending)
                .await?
                .ok_or(LoginError::AwaitingApproval { url })
        }
    }
}

/// Where `ployz login` starts from.
pub(crate) enum Start {
    /// Already signed in to this Cloud, and Cloud still honours the session.
    SignedIn(SignedIn),
    /// A code waiting for approval: resumed, or just requested.
    Pending { pending: Pending, resumed: bool },
}

/// Resume this Cloud's sign-in or pending code, or request a new code.
///
/// A resumed code is polled once, so a sign-in approved since shows as signed in.
///
/// # Errors
///
/// Returns [`LoginError::OtherCloud`] when signed in elsewhere, or a Cloud or store failure.
pub(crate) async fn start(store: &CredentialStore, cloud: &str) -> Result<Start, LoginError> {
    let cloud = crate::cloud_enroll::cloud_origin(cloud);
    let http = http()?;
    match store.load()? {
        Some(Stored::SignedIn(signed_in)) if signed_in.cloud != cloud => {
            return Err(LoginError::OtherCloud {
                signed_in: signed_in.cloud,
                requested: cloud,
            });
        }
        Some(Stored::SignedIn(signed_in)) => {
            if let Some((account, organization)) = session(&http, &cloud, &signed_in.token).await? {
                let signed_in = SignedIn {
                    account,
                    organization,
                    ..signed_in
                };
                store.save(&Stored::SignedIn(signed_in.clone()))?;
                return Ok(Start::SignedIn(signed_in));
            }
        }
        Some(Stored::Pending(pending)) if pending.cloud == cloud && pending.expires_in() > 0 => {
            return Ok(match poll_once(store, &http, pending.clone()).await {
                Ok(Some(signed_in)) => Start::SignedIn(signed_in),
                Ok(None) => Start::Pending {
                    pending,
                    resumed: true,
                },
                // Denied or expired: ask for a fresh code below.
                Err(LoginError::Denied | LoginError::Expired) => {
                    request_code(store, &http, cloud).await?
                }
                Err(error) => return Err(error),
            });
        }
        Some(Stored::Pending(_)) | None => {}
    }
    request_code(store, &http, cloud).await
}

/// Poll until the browser approves or denies `pending`, or it expires.
///
/// # Errors
///
/// Returns [`LoginError::Denied`] or [`LoginError::Expired`], or a Cloud or store failure.
pub(crate) async fn wait(
    store: &CredentialStore,
    pending: Pending,
) -> Result<SignedIn, LoginError> {
    let http = http()?;
    let mut interval = Duration::from_secs(pending.interval_secs);
    loop {
        tokio::time::sleep(interval).await;
        match poll(&http, &pending).await? {
            Poll::Approved(token) => return finish(store, &http, &pending.cloud, token).await,
            Poll::SlowDown => interval += SLOW_DOWN,
            Poll::Pending => {}
            Poll::Denied => return forget(store, LoginError::Denied),
            Poll::Expired => return forget(store, LoginError::Expired),
        }
    }
}

/// A finished logout: the Cloud, and for an approved sign-in, the device Cloud signed out and
/// which Servers confirmed clearing its key.
#[derive(Debug)]
pub(crate) struct SignedOut {
    pub(crate) cloud: String,
    pub(crate) device: Option<String>,
    pub(crate) servers: crate::cloud_account::ServerClears,
}

/// End this device's sign-in: Cloud first (which also clears its key on each Server), then
/// the stored credential. A sign-in Cloud already ended is only forgotten.
///
/// Returns `None` when nothing was signed in.
///
/// # Errors
///
/// Returns a Cloud failure, keeping the stored sign-in so logout can be retried,
/// or a store failure.
pub(crate) async fn logout(store: &CredentialStore) -> Result<Option<SignedOut>, LoginError> {
    #[derive(Deserialize)]
    struct Reply {
        signed_out: Device,
        servers: crate::cloud_account::ServerClears,
    }
    #[derive(Deserialize)]
    struct Device {
        id: String,
    }
    let signed_out = match store.load()? {
        None => return Ok(None),
        Some(Stored::Pending(pending)) => SignedOut {
            cloud: pending.cloud,
            device: None,
            servers: Default::default(),
        },
        Some(Stored::SignedIn(signed_in)) => {
            let url = format!("{}/api/cli/logout", signed_in.cloud);
            let response = http()?
                .post(url)
                .bearer_auth(&signed_in.token.0)
                .send()
                .await
                .map_err(|error| unreachable(&signed_in.cloud, error))?;
            let (device, servers) = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                (None, Default::default())
            } else {
                let reply: Reply = decode(ensure_success(response).await?)?;
                (Some(reply.signed_out.id), reply.servers)
            };
            SignedOut {
                cloud: signed_in.cloud,
                device,
                servers,
            }
        }
    };
    store.clear()?;
    Ok(Some(signed_out))
}

enum Poll {
    Pending,
    SlowDown,
    Approved(Secret),
    Denied,
    Expired,
}

#[derive(Deserialize)]
struct CodeReply {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionReply {
    user: Account,
    session: SessionFields,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionFields {
    active_organization_id: Option<String>,
    active_organization_slug: Option<String>,
}

async fn request_code(
    store: &CredentialStore,
    http: &reqwest::Client,
    cloud: String,
) -> Result<Start, LoginError> {
    let response = http
        .post(format!("{cloud}/api/auth/device/code"))
        .json(&serde_json::json!({ "client_id": CLIENT_ID }))
        .send()
        .await
        .map_err(|error| unreachable(&cloud, error))?;
    // Cloud answers 404 where CLI sign-in is not switched on.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(LoginError::Unsupported(cloud));
    }
    let reply: CodeReply = decode(ensure_success(response).await?)?;
    let pending = Pending {
        cloud,
        device_code: Secret(reply.device_code),
        code: reply.user_code,
        url: reply.verification_uri_complete,
        expires_at: now() + reply.expires_in,
        interval_secs: reply.interval.max(1),
    };
    store.save(&Stored::Pending(pending.clone()))?;
    Ok(Start::Pending {
        pending,
        resumed: false,
    })
}

/// Poll `pending` once; `None` while it still waits for the browser.
async fn poll_once(
    store: &CredentialStore,
    http: &reqwest::Client,
    pending: Pending,
) -> Result<Option<SignedIn>, LoginError> {
    match poll(http, &pending).await? {
        Poll::Approved(token) => finish(store, http, &pending.cloud, token).await.map(Some),
        Poll::Pending | Poll::SlowDown => Ok(None),
        Poll::Denied => forget(store, LoginError::Denied),
        Poll::Expired => forget(store, LoginError::Expired),
    }
}

async fn poll(http: &reqwest::Client, pending: &Pending) -> Result<Poll, LoginError> {
    if pending.expires_in() == 0 {
        return Ok(Poll::Expired);
    }
    let response = http
        .post(format!("{}/api/auth/device/token", pending.cloud))
        .json(&serde_json::json!({
            "grant_type": GRANT_TYPE,
            "device_code": pending.device_code.0,
            "client_id": CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|error| unreachable(&pending.cloud, error))?;
    if response.status() != reqwest::StatusCode::BAD_REQUEST {
        let reply: TokenReply = decode(ensure_success(response).await?)?;
        return Ok(Poll::Approved(Secret(reply.access_token)));
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| unreachable(&pending.cloud, error))?;
    let reply: TokenError = decode(body.to_vec())?;
    Ok(match reply.error.as_str() {
        "authorization_pending" => Poll::Pending,
        "slow_down" => Poll::SlowDown,
        "access_denied" => Poll::Denied,
        // An unknown device code has been spent or swept: it can never be approved.
        "expired_token" | "invalid_grant" => Poll::Expired,
        other => return Err(LoginError::Reply(format!("device token error {other}"))),
    })
}

/// Store an approved session with the account and Organization Cloud reports for it.
async fn finish(
    store: &CredentialStore,
    http: &reqwest::Client,
    cloud: &str,
    token: Secret,
) -> Result<SignedIn, LoginError> {
    let (account, organization) = session(http, cloud, &token)
        .await?
        .ok_or(LoginError::Ended)?;
    let signed_in = SignedIn {
        cloud: cloud.to_owned(),
        token,
        account,
        organization,
    };
    store.save(&Stored::SignedIn(signed_in.clone()))?;
    Ok(signed_in)
}

/// The account and Organization behind `token`; `None` once Cloud no longer honours it.
async fn session(
    http: &reqwest::Client,
    cloud: &str,
    token: &Secret,
) -> Result<Option<(Account, Organization)>, LoginError> {
    let response = http
        .get(format!("{cloud}/api/auth/get-session"))
        .bearer_auth(&token.0)
        .send()
        .await
        .map_err(|error| unreachable(cloud, error))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(None);
    }
    let Some(reply): Option<SessionReply> = decode(ensure_success(response).await?)? else {
        return Ok(None);
    };
    // Cloud gives every account a personal Organization when its session starts.
    let (Some(id), Some(slug)) = (
        reply.session.active_organization_id,
        reply.session.active_organization_slug,
    ) else {
        return Err(LoginError::Reply(
            "the session has no active Organization".to_owned(),
        ));
    };
    Ok(Some((reply.user, Organization { id, slug })))
}

fn forget<T>(store: &CredentialStore, error: LoginError) -> Result<T, LoginError> {
    store.clear()?;
    Err(error)
}

async fn ensure_success(response: reqwest::Response) -> Result<Vec<u8>, LoginError> {
    let status = response.status();
    let cloud = response.url().origin().ascii_serialization();
    let body = response
        .bytes()
        .await
        .map_err(|error| unreachable(&cloud, error))?;
    if status.is_success() {
        Ok(body.to_vec())
    } else {
        Err(LoginError::Status {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }
}

pub(crate) fn decode<T: serde::de::DeserializeOwned>(body: Vec<u8>) -> Result<T, LoginError> {
    serde_json::from_slice(&body).map_err(|error| LoginError::Reply(error.to_string()))
}

pub(crate) fn unreachable(cloud: &str, error: reqwest::Error) -> LoginError {
    LoginError::Unreachable {
        cloud: cloud.to_owned(),
        detail: crate::setup_retry::detail(&error.without_url()),
    }
}

/// Cloud's client; its User-Agent marks the sessions it starts as signed-in devices.
pub(crate) fn http() -> Result<reqwest::Client, LoginError> {
    reqwest::Client::builder()
        .user_agent(concat!("ployz-cli/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| LoginError::Reply(error.to_string()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
    };

    use super::*;

    /// A fake Cloud on loopback: `reply("POST /api/auth/device/token", body)` → (status, JSON).
    pub(crate) fn fake_cloud(
        reply: impl Fn(&str, &str) -> (u16, serde_json::Value) + Send + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let route = request_line.rsplit_once(' ').unwrap().0;
                let (status, json) = reply(route, &String::from_utf8(body).unwrap());
                let json = json.to_string();
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
                    json.len()
                )
                .unwrap();
            }
        });
        url
    }

    /// Cloud's device flow for one code, approved once `approved` is set.
    fn device_cloud(approved: Arc<Mutex<Option<&'static str>>>) -> String {
        fake_cloud(move |route, _| match route {
            "POST /api/auth/device/code" => (
                200,
                serde_json::json!({
                    "device_code": "device-secret",
                    "user_code": "ABCD2345",
                    "verification_uri": "http://cloud/device",
                    "verification_uri_complete": "http://cloud/device?user_code=ABCD2345",
                    "expires_in": 1800,
                    "interval": 1,
                }),
            ),
            "POST /api/auth/device/token" => match *approved.lock().unwrap() {
                None => (400, serde_json::json!({ "error": "authorization_pending" })),
                Some("denied") => (400, serde_json::json!({ "error": "access_denied" })),
                Some(_) => (200, serde_json::json!({ "access_token": "session-token" })),
            },
            "GET /api/auth/get-session" => (
                200,
                serde_json::json!({
                    "user": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
                    "session": { "activeOrganizationId": "o1", "activeOrganizationSlug": "acme" },
                }),
            ),
            "POST /api/cli/logout" => (
                200,
                serde_json::json!({
                    "signed_out": { "id": "d1" },
                    "servers": { "confirmed": ["m1"], "unconfirmed": ["m2"] },
                }),
            ),
            other => panic!("unexpected {other}"),
        })
    }

    fn store() -> (tempfile::TempDir, CredentialStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::beside(&dir.path().join("config.yaml"));
        (dir, store)
    }

    #[tokio::test]
    async fn other_commands_pick_up_an_approved_sign_in_without_waiting() {
        let approved = Arc::new(Mutex::new(None));
        let cloud = device_cloud(approved.clone());
        let (_dir, store) = store();
        assert!(matches!(
            signed_in(&store).await,
            Err(LoginError::SignedOut)
        ));

        let Start::Pending { pending, .. } = start(&store, &cloud).await.unwrap() else {
            panic!("a fresh login waits for approval");
        };
        let waiting = signed_in(&store).await.unwrap_err();
        assert!(
            matches!(&waiting, LoginError::AwaitingApproval { url } if *url == pending.url),
            "{waiting}"
        );

        *approved.lock().unwrap() = Some("approved");
        let picked = signed_in(&store).await.unwrap();
        assert_eq!(picked.account.email, "dev@example.test");
        assert_eq!(picked.organization.slug, "acme");
        // Stored: the next command needs no Cloud round trip.
        assert_eq!(signed_in(&store).await.unwrap().account, picked.account);
        let stored = std::fs::read_to_string(&store.path).unwrap();
        assert!(stored.contains("session-token"));
        assert!(!format!("{picked:?}").contains("session-token"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&store.path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn logout_reports_servers_still_to_clear_and_forgets_the_sign_in() {
        let cloud = device_cloud(Arc::new(Mutex::new(Some("approved"))));
        let (_dir, store) = store();
        let Start::Pending { pending, .. } = start(&store, &cloud).await.unwrap() else {
            panic!("a fresh login waits for approval");
        };
        wait(&store, pending).await.unwrap();
        let out = logout(&store).await.unwrap().unwrap();
        assert_eq!(out.device.as_deref(), Some("d1"));
        assert_eq!(out.servers.unconfirmed, ["m2"]);
        assert!(matches!(
            signed_in(&store).await,
            Err(LoginError::SignedOut)
        ));
        assert!(logout(&store).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_denied_code_is_forgotten() {
        let approved = Arc::new(Mutex::new(Some("denied")));
        let cloud = device_cloud(approved);
        let (_dir, store) = store();
        start(&store, &cloud).await.unwrap();
        assert!(matches!(signed_in(&store).await, Err(LoginError::Denied)));
        assert!(matches!(
            signed_in(&store).await,
            Err(LoginError::SignedOut)
        ));
    }

    #[tokio::test]
    async fn login_refuses_to_switch_clouds_while_signed_in() {
        let cloud = device_cloud(Arc::new(Mutex::new(Some("approved"))));
        let (_dir, store) = store();
        let Start::Pending { pending, .. } = start(&store, &cloud).await.unwrap() else {
            panic!("a fresh login waits for approval");
        };
        wait(&store, pending).await.unwrap();
        let error = start(&store, "http://127.0.0.1:1").await.err().unwrap();
        assert!(matches!(error, LoginError::OtherCloud { .. }), "{error}");
    }
}
