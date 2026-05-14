use super::{HTTP01_CHALLENGE_VISIBILITY_POLL, HTTP01_CHALLENGE_VISIBILITY_TIMEOUT};
use async_trait::async_trait;
use ployz_cert_acme_api::Http01ChallengeReadiness;
use ployz_error::{CertificateError, Result};
use ployz_store_api::{CertificateStore, StoreDriver};

pub struct LocalHttp01ChallengeReadiness;

#[async_trait]
impl Http01ChallengeReadiness for LocalHttp01ChallengeReadiness {
    async fn wait_ready(&self, store: &StoreDriver, hostname: &str, token: &str) -> Result<()> {
        wait_for_http01_challenge_visible(store, hostname, token).await
    }
}

pub async fn wait_for_http01_challenge_visible(
    store: &StoreDriver,
    hostname: &str,
    token: &str,
) -> Result<()> {
    let start = tokio::time::Instant::now();
    loop {
        let visible = store
            .list_acme_challenges()
            .await?
            .iter()
            .any(|challenge| challenge.hostname == hostname && challenge.token == token);
        if visible {
            return Ok(());
        }
        if start.elapsed() >= HTTP01_CHALLENGE_VISIBILITY_TIMEOUT {
            return Err(CertificateError::Http01LocalChallengeNotVisible {
                hostname: hostname.to_string(),
                token: token.to_string(),
            }
            .into());
        }
        tokio::time::sleep(HTTP01_CHALLENGE_VISIBILITY_POLL).await;
    }
}
