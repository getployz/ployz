//! The signed `/api/checkin` client and the Check-in Key it signs with.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use iroh::SecretKey;
use reqwest::{
    StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE, DATE, RETRY_AFTER},
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::wire::{self, Answer, CheckinRequest};

const SIGNATURE_CONTEXT: &[u8] = b"ployz-checkin-v1\0";
const TIMESTAMP_HEADER: &str = "Ployz-Timestamp";
const SIGNATURE_HEADER: &str = "Ployz-Signature";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
pub(super) const BACKOFF: Duration = Duration::from_secs(60);
pub(super) const OUTDATED_BACKOFF: Duration = Duration::from_secs(60 * 60);
const FLEET_TOKEN_PREFIX: &str = "pflt_";
const FLEET_TOKEN_SECRET_CHARS: usize = 43;
const TOKEN_MODE: u32 = 0o600;
const MAX_SKEW_SECONDS: i64 = 300;
const TRUSTED_SKEW_SECONDS: i64 = MAX_SKEW_SECONDS - 60;
const CLOUD_REFUSAL: &str = "Authentication is required.";

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("read the Check-in Key at {path}")]
    Read { path: PathBuf, source: io::Error },
    #[error("write the Check-in Key at {path}")]
    Write { path: PathBuf, source: io::Error },
    #[error("the Check-in Key at {path} isn't 32 bytes; remove it to start over as a new server")]
    Malformed { path: PathBuf },
    #[error("read the Fleet Token at {path}")]
    Token { path: PathBuf, source: io::Error },
    #[error("{path} doesn't hold a Fleet Token (pflt_…)")]
    NotAToken { path: PathBuf },
}

pub(crate) struct CheckinKey {
    secret: SecretKey,
    public_hex: String,
}

impl CheckinKey {
    pub(crate) fn load_or_create(path: &Path) -> Result<Self, KeyError> {
        match fs::read(path) {
            Ok(bytes) => Self::from_file(path, &bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::create(path),
            Err(source) => Err(KeyError::Read {
                path: path.to_owned(),
                source,
            }),
        }
    }

    fn create(path: &Path) -> Result<Self, KeyError> {
        let write = |source| KeyError::Write {
            path: path.to_owned(),
            source,
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(write)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).map_err(write)?;
        }
        let secret = SecretKey::generate();
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let bytes = fs::read(path).map_err(|source| KeyError::Read {
                    path: path.to_owned(),
                    source,
                })?;
                return Self::from_file(path, &bytes);
            }
            Err(source) => return Err(write(source)),
        };
        file.write_all(&secret.to_bytes())
            .and_then(|()| file.sync_all())
            .map_err(write)?;
        Ok(Self::new(secret))
    }

    fn from_file(path: &Path, bytes: &[u8]) -> Result<Self, KeyError> {
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| KeyError::Malformed {
            path: path.to_owned(),
        })?;
        Ok(Self::new(SecretKey::from_bytes(&bytes)))
    }

    pub(super) fn new(secret: SecretKey) -> Self {
        let public_hex = hex::encode(secret.public().as_bytes());
        Self { secret, public_hex }
    }

    pub(crate) fn public_hex(&self) -> &str {
        &self.public_hex
    }

    pub(super) fn sign(&self, ts: i64, body: &[u8]) -> String {
        let mut message = SIGNATURE_CONTEXT.to_vec();
        message.extend_from_slice(self.public_hex.as_bytes());
        message.push(0);
        message.extend_from_slice(&ts.to_be_bytes());
        message.extend_from_slice(&Sha256::digest(body));
        URL_SAFE_NO_PAD.encode(self.secret.sign(&message).to_bytes())
    }
}

#[derive(PartialEq, Eq)]
pub(crate) struct FleetToken(String);

impl FleetToken {
    pub(crate) fn read(path: &Path) -> Result<Self, KeyError> {
        let text = fs::read_to_string(path).map_err(|source| KeyError::Token {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text).ok_or_else(|| KeyError::NotAToken {
            path: path.to_owned(),
        })
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        let token = text.trim();
        let secret = token.strip_prefix(FLEET_TOKEN_PREFIX).unwrap_or_default();
        (secret.len() == FLEET_TOKEN_SECRET_CHARS
            && secret
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
        .then(|| Self(token.to_owned()))
    }

    #[cfg(test)]
    pub(super) fn literal(token: &str) -> Self {
        Self(token.to_owned())
    }
}

pub(crate) struct CheckinClient {
    http: reqwest::Client,
    url: String,
    token: FleetToken,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Reply {
    Answer(Answer),
    /// Cloud refused the token, at the time its `Date` header gives.
    Refused(DateTime<Utc>),
    Unreachable(Duration, String),
    Held(Duration, String),
    /// The URL answered, and the answer can't have come from Ployz Cloud.
    NotCloud(String),
    ClockOff(String),
    Outdated(String),
}

impl CheckinClient {
    pub(crate) fn new(cloud_url: &str, token: FleetToken) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ployz-cli/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            url: format!(
                "{}/api/checkin",
                crate::cloud_enroll::cloud_origin(cloud_url)
            ),
            token,
        })
    }

    pub(super) fn use_token(&mut self, token: FleetToken) {
        self.token = token;
    }

    pub(crate) fn sends(&self, token: &FleetToken) -> bool {
        self.token == *token
    }

    /// Writes the token this client sends to `path` in one step, readable by root only.
    pub(crate) fn write_token(&self, path: &Path) -> io::Result<()> {
        let directory = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(TOKEN_MODE))?;
        writeln!(file, "{}", self.token.0)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    /// Names the token in status.json without writing the token itself there.
    pub(super) fn fingerprint(&self) -> String {
        hex::encode(
            Sha256::digest(self.token.0.as_bytes())
                .get(..8)
                .unwrap_or_default(),
        )
    }

    pub(super) async fn post(&self, key: &CheckinKey, request: &CheckinRequest<'_>) -> Reply {
        let body = match serde_json::to_vec(request) {
            Ok(body) => body,
            Err(error) => return Reply::Unreachable(BACKOFF, error.to_string()),
        };
        let ts = unix_seconds();
        let sent = self
            .http
            .post(&self.url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token.0))
            .header(CONTENT_TYPE, "application/json")
            .header(TIMESTAMP_HEADER, ts.to_string())
            .header(SIGNATURE_HEADER, format!("v1={}", key.sign(ts, &body)))
            .body(body)
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(error) => return Reply::Unreachable(BACKOFF, error.without_url().to_string()),
        };
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(|seconds| Duration::from_secs(seconds.max(1)));
        let cloud = response
            .headers()
            .get(DATE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| DateTime::parse_from_rfc2822(value).ok())
            .map(|cloud| cloud.to_utc());
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => return Reply::Unreachable(BACKOFF, error.without_url().to_string()),
        };
        let reply = reply(status, retry_after, cloud, &body);
        if let Reply::NotCloud(why) = &reply {
            return Reply::NotCloud(format!(
                "{} {why}, which isn't how Ployz Cloud answers. A proxy or firewall in between may have answered instead",
                self.url
            ));
        }
        reply
    }
}

pub(super) fn reply(
    status: StatusCode,
    retry_after: Option<Duration>,
    cloud: Option<DateTime<Utc>>,
    body: &[u8],
) -> Reply {
    if status.is_success() {
        return wire::answer(body).map_or_else(
            || {
                Reply::NotCloud(format!(
                    "answered {status} with something other than a check-in answer"
                ))
            },
            Reply::Answer,
        );
    }
    if status == StatusCode::UNAUTHORIZED {
        return unauthorized(cloud, body);
    }
    if status == StatusCode::BAD_REQUEST {
        return Reply::Outdated(format!(
            "Cloud answered {status} to this check-in. Install a newer ployz"
        ));
    }
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        return Reply::Outdated(format!("Cloud couldn't take this check-in ({status})"));
    }
    let said = cloud_error(body);
    if status == StatusCode::TOO_MANY_REQUESTS {
        let after = retry_after.unwrap_or(BACKOFF).min(wire::WAIT_CAP);
        return match said {
            Some(why) => Reply::Held(after, why),
            None => Reply::Unreachable(after, format!("Cloud answered {status}")),
        };
    }
    if let Some(why) = said {
        return Reply::Unreachable(BACKOFF, format!("Cloud answered {status}: {why}"));
    }
    if status.is_client_error() && status != StatusCode::REQUEST_TIMEOUT {
        return Reply::NotCloud(format!("answered {status}"));
    }
    Reply::Unreachable(BACKOFF, format!("Cloud answered {status}"))
}

fn cloud_error(body: &[u8]) -> Option<String> {
    let body = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let text = |field: &str| body.get(field).and_then(serde_json::Value::as_str);
    let message = match text("_tag") {
        Some("PublicError") => text("code").and(text("message")),
        _ => text("error"),
    };
    message.map(str::to_owned)
}

/// A 401 counts against the token only when Cloud itself refused it and the clocks agree well
/// inside Cloud's window. Anything else may be a proxy or a clock, and says nothing about it.
fn unauthorized(cloud: Option<DateTime<Utc>>, body: &[u8]) -> Reply {
    let refused = serde_json::from_slice::<serde_json::Value>(body).is_ok_and(|body| {
        body.get("error").and_then(serde_json::Value::as_str) == Some(CLOUD_REFUSAL)
    });
    if !refused {
        return Reply::Unreachable(
            BACKOFF,
            format!("Cloud answered {}", StatusCode::UNAUTHORIZED),
        );
    }
    let Some(cloud) = cloud else {
        return Reply::Unreachable(
            BACKOFF,
            "Cloud refused this check-in without a readable Date, so the clock can't be checked"
                .to_owned(),
        );
    };
    match cloud.timestamp() - unix_seconds() {
        skew if skew.abs() <= TRUSTED_SKEW_SECONDS => Reply::Refused(cloud),
        skew if skew.abs() <= MAX_SKEW_SECONDS => Reply::ClockOff(format!(
            "Cloud refused this Fleet Token, and this server's clock is {}s off from Cloud's. Set the clock right (timedatectl) so the refusal can be confirmed",
            skew.abs()
        )),
        skew => Reply::ClockOff(format!(
            "This server's clock is {}s off from Cloud's. Set the clock right (timedatectl)",
            skew.abs()
        )),
    }
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}
