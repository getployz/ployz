//! The signed `/api/checkin` client and the Check-in Key it signs with.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use iroh::SecretKey;
use reqwest::{
    StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER},
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::wire::{self, Answer, CheckinRequest, StopReason};

const SIGNATURE_CONTEXT: &[u8] = b"ployz-checkin-v1\0";
const TIMESTAMP_HEADER: &str = "Ployz-Timestamp";
const SIGNATURE_HEADER: &str = "Ployz-Signature";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the loop waits after Cloud didn't answer.
pub(super) const BACKOFF: Duration = Duration::from_secs(60);
const FLEET_TOKEN_PREFIX: &str = "pflt_";

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

/// The ed25519 key a waiting host signs every check-in with. It names the host
/// to Cloud until it enrolls, and is never its Management Identity.
pub(crate) struct CheckinKey {
    secret: SecretKey,
    public_hex: String,
}

impl CheckinKey {
    /// Reads the key at `path`, or creates it with mode 0600 when there is none,
    /// so every rerun on this host keeps the same identity.
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
            // Another run made it first; its key is the one this host keeps.
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

    /// The public key as Cloud stores it: 64 lowercase hex characters.
    pub(crate) fn public_hex(&self) -> &str {
        &self.public_hex
    }

    /// `v1=` signature over the context, the key, the timestamp and the body's sha256.
    pub(super) fn sign(&self, ts: i64, body: &[u8]) -> String {
        let mut message = SIGNATURE_CONTEXT.to_vec();
        message.extend_from_slice(self.public_hex.as_bytes());
        message.push(0);
        message.extend_from_slice(&ts.to_be_bytes());
        message.extend_from_slice(&Sha256::digest(body));
        URL_SAFE_NO_PAD.encode(self.secret.sign(&message).to_bytes())
    }
}

/// A `pflt_` token, read from its file and never printed.
pub(crate) struct FleetToken(String);

impl FleetToken {
    pub(crate) fn read(path: &Path) -> Result<Self, KeyError> {
        let text = fs::read_to_string(path).map_err(|source| KeyError::Token {
            path: path.to_owned(),
            source,
        })?;
        let token = text.trim();
        if token.starts_with(FLEET_TOKEN_PREFIX) && !token.contains(char::is_whitespace) {
            Ok(Self(token.to_owned()))
        } else {
            Err(KeyError::NotAToken {
                path: path.to_owned(),
            })
        }
    }

    #[cfg(test)]
    pub(super) fn literal(token: &str) -> Self {
        Self(token.to_owned())
    }
}

/// Posts signed check-ins to one Cloud.
pub(crate) struct CheckinClient {
    http: reqwest::Client,
    url: String,
    token: FleetToken,
}

/// What came back from one post, read so the loop never has to look at HTTP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Reply {
    Answer(Answer),
    /// Cloud didn't answer, or answered with an error; try again after the wait.
    Unreachable(Duration, String),
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
        let body = response.bytes().await.unwrap_or_default();
        reply(status, retry_after, &body)
    }
}

pub(super) fn reply(status: StatusCode, retry_after: Option<Duration>, body: &[u8]) -> Reply {
    if status.is_success() {
        return Reply::Answer(wire::answer(body));
    }
    if status == StatusCode::UNAUTHORIZED {
        // Cloud says only "unauthorized" for an unknown, revoked or misused token,
        // and none of them gets better by asking again.
        return Reply::Answer(Answer::Stop(
            StopReason::TokenRevoked,
            "Cloud no longer accepts this Fleet Token.".to_owned(),
        ));
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Reply::Answer(Answer::Wait(retry_after.unwrap_or(Duration::from_secs(1))));
    }
    Reply::Unreachable(BACKOFF, format!("Cloud answered {status}"))
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}
