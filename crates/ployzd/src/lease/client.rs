use ployz_core::cert::{
    LeaseBearerToken, ManagedCertBundle, ManagedLeaseAcquireRequest, ManagedLeaseAcquired,
    ManagedLeaseName, ManagedLeaseRenewed,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseWorkerUrl(String);

impl LeaseWorkerUrl {
    #[must_use]
    pub fn default_worker() -> Self {
        Self(ployz_core::cert::DEFAULT_LEASE_WORKER_URL.to_owned())
    }

    pub fn try_new(value: impl Into<String>) -> Result<Self, LeaseWorkerUrlError> {
        let value = value.into();
        let uri = value
            .parse::<ureq::http::Uri>()
            .map_err(|_| LeaseWorkerUrlError::Invalid {
                value: value.clone(),
            })?;
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.authority().is_none()
            || uri.query().is_some()
            || !matches!(uri.path(), "" | "/")
        {
            return Err(LeaseWorkerUrlError::Invalid { value });
        }

        Ok(Self(value.trim_end_matches('/').to_owned()))
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeaseWorkerUrlError {
    #[error("invalid lease worker base URL {value:?}")]
    Invalid { value: String },
}

#[derive(Debug, Clone)]
pub struct LeaseClient {
    base_url: LeaseWorkerUrl,
    agent: ureq::Agent,
}

impl LeaseClient {
    #[must_use]
    pub fn new(base_url: LeaseWorkerUrl) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .build()
            .into();
        Self { base_url, agent }
    }

    pub async fn acquire(
        &self,
        cluster_id: String,
    ) -> Result<ManagedLeaseAcquired, LeaseClientError> {
        let request = ManagedLeaseAcquireRequest { cluster_id };
        self.post_json("/v1/leases", None, request).await
    }

    pub async fn renew(
        &self,
        name: ManagedLeaseName,
        token: LeaseBearerToken,
    ) -> Result<ManagedLeaseRenewed, LeaseClientError> {
        let path = format!("/v1/leases/{}/renew", name.as_str());
        self.post_json(&path, Some(token), ()).await
    }

    pub async fn download_bundle(
        &self,
        name: ManagedLeaseName,
        token: LeaseBearerToken,
    ) -> Result<ManagedCertBundle, LeaseClientError> {
        let agent = self.agent.clone();
        let url = self
            .base_url
            .endpoint(&format!("/v1/leases/{}/cert-bundle", name.as_str()));
        tokio::task::spawn_blocking(move || {
            let mut response = agent
                .get(&url)
                .header("Authorization", format!("Bearer {}", token.as_str()))
                .call()
                .map_err(LeaseClientError::from_ureq)?;
            decode_response(&mut response)
        })
        .await
        .map_err(|error| LeaseClientError::Transport {
            message: error.to_string(),
        })?
    }

    async fn post_json<T, R>(
        &self,
        path: &str,
        token: Option<LeaseBearerToken>,
        body: T,
    ) -> Result<R, LeaseClientError>
    where
        T: Serialize + Send + 'static,
        R: DeserializeOwned + Send + 'static,
    {
        let agent = self.agent.clone();
        let url = self.base_url.endpoint(path);
        tokio::task::spawn_blocking(move || {
            let request = agent.post(&url);
            let request = if let Some(token) = token {
                request.header("Authorization", format!("Bearer {}", token.as_str()))
            } else {
                request
            };
            let mut response = request
                .send_json(body)
                .map_err(LeaseClientError::from_ureq)?;
            decode_response(&mut response)
        })
        .await
        .map_err(|error| LeaseClientError::Transport {
            message: error.to_string(),
        })?
    }
}

fn decode_response<R: DeserializeOwned>(
    response: &mut ureq::http::Response<ureq::Body>,
) -> Result<R, LeaseClientError> {
    let body =
        response
            .body_mut()
            .read_to_string()
            .map_err(|error| LeaseClientError::Transport {
                message: error.to_string(),
            })?;
    serde_json::from_str(&body).map_err(|error| LeaseClientError::Decode {
        message: error.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeaseClientError {
    #[error("lease worker rejected the bearer token")]
    Unauthorized,
    #[error("managed lease was not found")]
    LeaseNotFound,
    #[error("lease worker returned HTTP status {status}")]
    Http { status: u16 },
    #[error("lease worker transport failed: {message}")]
    Transport { message: String },
    #[error("lease worker response decode failed: {message}")]
    Decode { message: String },
}

impl LeaseClientError {
    fn from_ureq(error: ureq::Error) -> Self {
        if let ureq::Error::StatusCode(status) = error {
            return match status {
                401 => Self::Unauthorized,
                404 => Self::LeaseNotFound,
                status => Self::Http { status },
            };
        }
        Self::Transport {
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_lease_worker::{StubLeaseWorker, serve};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    #[test]
    fn lease_worker_url_requires_http_origin_without_path() {
        assert!(
            LeaseWorkerUrl::try_new("https://up.ployz.app").is_ok()
                && LeaseWorkerUrl::try_new("http://127.0.0.1:8089/").is_ok()
                && LeaseWorkerUrl::try_new("ftp://up.ployz.app").is_err()
                && LeaseWorkerUrl::try_new("https://up.ployz.app/api").is_err()
        );
    }

    #[tokio::test]
    async fn client_acquires_downloads_and_renews_against_stub_worker() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub worker");
        let address = listener.local_addr().expect("stub worker address");
        let server = tokio::spawn(serve(
            listener,
            Arc::new(Mutex::new(StubLeaseWorker::new())),
        ));
        let client = LeaseClient::new(
            LeaseWorkerUrl::try_new(format!("http://{address}")).expect("valid worker URL"),
        );

        let acquired = client
            .acquire("cluster-one".to_owned())
            .await
            .expect("lease acquired");
        let bundle = client
            .download_bundle(acquired.lease.name.clone(), acquired.lease.token.clone())
            .await
            .expect("bundle downloaded");
        let renewed = client
            .renew(acquired.lease.name.clone(), acquired.lease.token.clone())
            .await
            .expect("lease renewed");

        assert_eq!(bundle.dns_names, acquired.lease.name.wildcard_and_apex());
        assert_eq!(renewed.lease.name, acquired.lease.name);
        assert!(
            bundle
                .certificate_chain_pem
                .starts_with("-----BEGIN CERTIFICATE-----")
        );
        server.abort();
    }

    #[tokio::test]
    async fn client_maps_invalid_token_to_unauthorized() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub worker");
        let address = listener.local_addr().expect("stub worker address");
        let server = tokio::spawn(serve(
            listener,
            Arc::new(Mutex::new(StubLeaseWorker::new())),
        ));
        let client = LeaseClient::new(
            LeaseWorkerUrl::try_new(format!("http://{address}")).expect("valid worker URL"),
        );
        let acquired = client
            .acquire("cluster-one".to_owned())
            .await
            .expect("lease acquired");
        let wrong_token = LeaseBearerToken::try_new("wrong-token").expect("valid token shape");

        let error = client
            .renew(acquired.lease.name, wrong_token)
            .await
            .expect_err("wrong token must fail");

        assert_eq!(error, LeaseClientError::Unauthorized);
        server.abort();
    }
}
