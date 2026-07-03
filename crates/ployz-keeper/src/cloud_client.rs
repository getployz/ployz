//! Minimal HTTPS JSON client for interactive Cloud bootstrap.

use std::fmt;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

#[derive(Debug, Clone)]
pub struct CloudClient {
    host: String,
    agent: ureq::Agent,
}

impl CloudClient {
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            agent: cloud_agent(),
        }
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
        let request = self.post_request(url, bearer_token);
        request
            .send_json(body)
            .map_err(CloudClientError::from_ureq)?
            .body_mut()
            .read_json()
            .map_err(CloudClientError::from_ureq)
    }

    pub fn validate_same_origin_url(&self, url: &str) -> Result<(), CloudClientError> {
        validate_same_origin_url(&self.host, url)
    }

    fn post_request(
        &self,
        url: &str,
        bearer_token: Option<&str>,
    ) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let request = self.agent.post(url);
        if let Some(token) = bearer_token {
            request.header("Authorization", format!("Bearer {token}"))
        } else {
            request
        }
    }
}

pub fn get_text_url(url: &str) -> Result<String, CloudClientError> {
    get_text_with_agent(&cloud_agent(), url)
}

fn get_text_with_agent(agent: &ureq::Agent, url: &str) -> Result<String, CloudClientError> {
    validate_https_url(url)?;
    agent
        .get(url)
        .call()
        .map_err(CloudClientError::from_ureq)?
        .body_mut()
        .read_to_string()
        .map_err(CloudClientError::from_ureq)
}

fn cloud_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .timeout_connect(Some(Duration::from_secs(5)))
        .build()
        .into()
}

pub fn endpoint_url(host: &str, path: &str) -> Result<String, CloudClientError> {
    let host = parse_https_url(host)?;
    if path.is_empty() || !path.starts_with('/') {
        return Err(CloudClientError::InvalidEndpoint);
    }
    let url = host
        .join(path)
        .map_err(|_| CloudClientError::InvalidEndpoint)?;
    Ok(url.to_string())
}

fn validate_https_url(url: &str) -> Result<(), CloudClientError> {
    parse_https_url(url)?;
    Ok(())
}

pub fn validate_same_origin_url(host: &str, url: &str) -> Result<(), CloudClientError> {
    let host = parse_https_url(host)?;
    let url = parse_https_url(url)?;
    if host.origin() != url.origin()
        || !url.path().starts_with('/')
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CloudClientError::InvalidEndpoint);
    }
    Ok(())
}

fn parse_https_url(raw: &str) -> Result<Url, CloudClientError> {
    let url = Url::parse(raw).map_err(|_| CloudClientError::InvalidEndpoint)?;
    if url.scheme() != "https" || url.host().is_none() {
        return Err(CloudClientError::InvalidEndpoint);
    }
    Ok(url)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudClientError {
    InvalidEndpoint,
    Http {
        status: Option<u16>,
        message: String,
    },
    Request {
        message: String,
    },
}

impl CloudClientError {
    fn from_ureq(error: ureq::Error) -> Self {
        match error {
            ureq::Error::StatusCode(status) => Self::Http {
                status: Some(status),
                message: error.to_string(),
            },
            error => Self::Request {
                message: error.to_string(),
            },
        }
    }
}

impl fmt::Display for CloudClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => formatter.write_str("cloud endpoint is invalid"),
            Self::Http { status, message } => {
                write!(formatter, "cloud request failed")?;
                if let Some(status) = status {
                    write!(formatter, " with HTTP status {status}")?;
                }
                if !message.is_empty() {
                    write!(formatter, ": {message}")?;
                }
                Ok(())
            }
            Self::Request { message } => {
                write!(formatter, "cloud request failed: {message}")
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
                "https://CLOUD.example.com",
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
