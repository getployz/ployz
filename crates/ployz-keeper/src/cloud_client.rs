//! Minimal HTTPS JSON client for interactive Cloud bootstrap.

use std::fmt;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudClient {
    host: String,
}

impl CloudClient {
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }

    pub fn post_json<T, R>(
        &self,
        path: &str,
        bearer_token: Option<&str>,
        body: &T,
    ) -> Result<R, CloudClientError>
    where
        T: Serialize,
        R: DeserializeOwned,
    {
        let url = endpoint_url(&self.host, path)?;
        self.post_json_to_url(&url, bearer_token, body)
    }

    pub fn post_json_to_url<T, R>(
        &self,
        url: &str,
        bearer_token: Option<&str>,
        body: &T,
    ) -> Result<R, CloudClientError>
    where
        T: Serialize,
        R: DeserializeOwned,
    {
        validate_same_origin_url(&self.host, url)?;
        let body = serde_json::to_vec(body).map_err(|error| CloudClientError::Serialize {
            message: error.to_string(),
        })?;
        let body_file = write_curl_body_file(&body)?;

        let mut command = Command::new("curl");
        command.args([
            "-K",
            "-",
            "-fsS",
            "--max-time",
            "15",
            "--connect-timeout",
            "5",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
        ]);
        command
            .arg("--data-binary")
            .arg(format!("@{}", body_file.display()))
            .arg(url);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_file(&body_file);
                return Err(CloudClientError::Spawn {
                    message: error.to_string(),
                });
            }
        };
        if let Err(error) = child
            .stdin
            .take()
            .expect("curl stdin is piped")
            .write_all(curl_config(bearer_token).as_bytes())
        {
            let _ = fs::remove_file(&body_file);
            return Err(CloudClientError::WriteRequest {
                message: error.to_string(),
            });
        }
        let output = match child.wait_with_output() {
            Ok(output) => output,
            Err(error) => {
                let _ = fs::remove_file(&body_file);
                return Err(CloudClientError::Wait {
                    message: error.to_string(),
                });
            }
        };
        let _ = fs::remove_file(&body_file);
        if !output.status.success() {
            return Err(CloudClientError::Http {
                status: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        serde_json::from_slice(&output.stdout).map_err(|error| CloudClientError::Deserialize {
            message: error.to_string(),
        })
    }

    pub fn validate_same_origin_url(&self, url: &str) -> Result<(), CloudClientError> {
        validate_same_origin_url(&self.host, url)
    }
}

fn curl_config(bearer_token: Option<&str>) -> String {
    bearer_token
        .map(|token| format!("header = \"Authorization: Bearer {token}\"\n"))
        .unwrap_or_default()
}

fn write_curl_body_file(body: &[u8]) -> Result<PathBuf, CloudClientError> {
    let path = curl_body_file_path()?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| CloudClientError::WriteRequest {
            message: error.to_string(),
        })?;
    file.write_all(body)
        .map_err(|error| CloudClientError::WriteRequest {
            message: error.to_string(),
        })?;
    Ok(path)
}

fn curl_body_file_path() -> Result<PathBuf, CloudClientError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CloudClientError::WriteRequest {
            message: error.to_string(),
        })?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!(
        "ployz-cloud-body-{}-{nanos}.json",
        std::process::id()
    )))
}

pub fn endpoint_url(host: &str, path: &str) -> Result<String, CloudClientError> {
    if !host.starts_with("https://") || path.is_empty() || !path.starts_with('/') {
        return Err(CloudClientError::InvalidEndpoint);
    }
    Ok(format!("{host}{path}"))
}

pub fn validate_same_origin_url(host: &str, url: &str) -> Result<(), CloudClientError> {
    if !host.starts_with("https://") || !url.starts_with("https://") {
        return Err(CloudClientError::InvalidEndpoint);
    }
    let Some(path) = url.strip_prefix(host) else {
        return Err(CloudClientError::InvalidEndpoint);
    };
    if !path.starts_with('/') || path.contains('?') || path.contains('#') {
        return Err(CloudClientError::InvalidEndpoint);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudClientError {
    InvalidEndpoint,
    Serialize {
        message: String,
    },
    Spawn {
        message: String,
    },
    WriteRequest {
        message: String,
    },
    Wait {
        message: String,
    },
    Http {
        status: Option<i32>,
        message: String,
    },
    Deserialize {
        message: String,
    },
}

impl fmt::Display for CloudClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => formatter.write_str("cloud endpoint is invalid"),
            Self::Serialize { message } => {
                write!(formatter, "failed to serialize cloud request: {message}")
            }
            Self::Spawn { message } => write!(
                formatter,
                "failed to start curl for cloud request: {message}"
            ),
            Self::WriteRequest { message } => {
                write!(formatter, "failed to write cloud request: {message}")
            }
            Self::Wait { message } => {
                write!(formatter, "failed to wait for cloud request: {message}")
            }
            Self::Http { status, message } => {
                write!(formatter, "cloud request failed")?;
                if let Some(status) = status {
                    write!(formatter, " with exit status {status}")?;
                }
                if !message.is_empty() {
                    write!(formatter, ": {message}")?;
                }
                Ok(())
            }
            Self::Deserialize { message } => {
                write!(formatter, "failed to parse cloud response: {message}")
            }
        }
    }
}

impl std::error::Error for CloudClientError {}

#[cfg(test)]
mod tests {
    use super::{endpoint_url, validate_same_origin_url};

    #[test]
    fn endpoint_url_keeps_secrets_out_of_url() {
        assert_eq!(
            endpoint_url("https://cloud.example.com", "/api/bootstrap/sessions").unwrap(),
            "https://cloud.example.com/api/bootstrap/sessions"
        );
        assert!(endpoint_url("http://cloud.example.com", "/api").is_err());
        assert!(endpoint_url("https://cloud.example.com", "api").is_err());
    }

    #[test]
    fn callback_url_must_be_https_same_origin_and_path_only() {
        assert!(
            validate_same_origin_url(
                "https://cloud.example.com",
                "https://cloud.example.com/api/bootstrap/redemptions/pcbr_1/callback"
            )
            .is_ok()
        );
        assert!(
            validate_same_origin_url(
                "https://cloud.example.com",
                "https://evil.example.com/api/bootstrap/redemptions/pcbr_1/callback"
            )
            .is_err()
        );
        assert!(
            validate_same_origin_url(
                "https://cloud.example.com",
                "https://cloud.example.com/api/bootstrap/redemptions/pcbr_1/callback?token=secret"
            )
            .is_err()
        );
    }
}
