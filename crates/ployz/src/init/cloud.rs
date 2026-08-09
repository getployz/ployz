//! Cloud's paste-line envelope and bounded progress-only callback.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use ployz_core::ids::{ClusterName, MachineName, PeerName};
use ployz_core::network::WireGuardPublicKey;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::commands::CloudToken;

const TOKEN_PREFIX: &str = "pcbs2_";
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(10);
const CALLBACK_ATTEMPTS: usize = 3;
const CALLBACK_RETRY_BACKOFF: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct CloudEnvelope {
    pub callback_url: Url,
    callback_token: String,
    pub peer_id: PeerName,
    pub peer_name: String,
    pub public_key: WireGuardPublicKey,
}

impl fmt::Debug for CloudEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CloudEnvelope")
            .field("callback_url", &"[REDACTED]")
            .field("callback_token", &"[REDACTED]")
            .field("peer_id", &self.peer_id)
            .field("peer_name", &self.peer_name)
            .field("public_key", &self.public_key)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloudEnvelopeWire {
    callback_url: String,
    callback_token: String,
    peer_id: String,
    peer_name: String,
    public_key: String,
}

impl CloudEnvelope {
    pub fn decode(token: &CloudToken) -> Result<Self, CloudError> {
        if token.expose().len() > MAX_TOKEN_BYTES {
            return Err(CloudError::InvalidToken);
        }
        let Some(payload) = token.expose().strip_prefix(TOKEN_PREFIX) else {
            return Err(CloudError::InvalidToken);
        };
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| CloudError::InvalidToken)?;
        let wire: CloudEnvelopeWire =
            serde_json::from_slice(&decoded).map_err(|_| CloudError::InvalidToken)?;
        if wire.callback_token.is_empty()
            || wire.callback_token.len() > 4_096
            || wire
                .callback_token
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || wire.peer_name.is_empty()
            || wire.peer_name.len() > 128
            || wire.peer_name.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(CloudError::InvalidToken);
        }
        let callback_url = Url::parse(&wire.callback_url).map_err(|_| CloudError::InvalidToken)?;
        if (callback_url.scheme() != "https" && !is_loopback_http(&callback_url))
            || !callback_url.username().is_empty()
            || callback_url.password().is_some()
            || callback_url.query().is_some()
            || callback_url.fragment().is_some()
        {
            return Err(CloudError::InsecureCallback);
        }
        Ok(Self {
            callback_url,
            callback_token: wire.callback_token,
            peer_id: PeerName::try_new(wire.peer_id).map_err(|_| CloudError::InvalidToken)?,
            peer_name: wire.peer_name,
            public_key: WireGuardPublicKey::try_new(wire.public_key)
                .map_err(|_| CloudError::InvalidToken)?,
        })
    }

    pub async fn report(&self, progress: CloudProgress) -> Result<(), CloudError> {
        let client = Client::builder()
            .connect_timeout(CALLBACK_TIMEOUT)
            .timeout(CALLBACK_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CloudError::Callback)?;
        let idempotency_key = progress.idempotency_key();
        for attempt in 0..CALLBACK_ATTEMPTS {
            match client
                .post(self.callback_url.clone())
                .bearer_auth(&self.callback_token)
                .header("Idempotency-Key", &idempotency_key)
                .json(&progress)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(_) | Err(_) => {}
            }
            if attempt + 1 < CALLBACK_ATTEMPTS {
                tokio::time::sleep(CALLBACK_RETRY_BACKOFF.saturating_mul((attempt + 1) as u32))
                    .await;
            }
        }
        Err(CloudError::Callback)
    }
}

fn is_loopback_http(url: &Url) -> bool {
    url.scheme() == "http"
        && url
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback())
}

/// The callback has progress and public enrollment only. Bootstrap answers are
/// absent by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CloudProgress {
    Enrolled {
        cluster_id: ClusterName,
        machine_id: MachineName,
        machine_public_key: WireGuardPublicKey,
    },
    Ready {
        cluster_id: ClusterName,
        machine_id: MachineName,
        machine_public_key: WireGuardPublicKey,
    },
    Failed {
        cluster_id: ClusterName,
        #[serde(skip_serializing_if = "Option::is_none")]
        machine_id: Option<MachineName>,
        reason: String,
        repair_command: Option<String>,
    },
}

impl CloudProgress {
    /// Cloud treats repeated updates with this key as one idempotent state write.
    fn idempotency_key(&self) -> String {
        let (state, cluster_id, machine_id) = match self {
            Self::Enrolled {
                cluster_id,
                machine_id,
                ..
            } => ("enrolled", cluster_id, machine_id),
            Self::Ready {
                cluster_id,
                machine_id,
                ..
            } => ("ready", cluster_id, machine_id),
            Self::Failed {
                cluster_id,
                machine_id: Some(machine_id),
                ..
            } => return format!("ployz-init-{cluster_id}-{machine_id}-failed"),
            Self::Failed {
                cluster_id,
                machine_id: None,
                ..
            } => return format!("ployz-init-{cluster_id}-preflight-failed"),
        };
        format!("ployz-init-{cluster_id}-{machine_id}-{state}")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("cloud token is invalid")]
    InvalidToken,
    #[error("cloud callback must use HTTPS")]
    InsecureCallback,
    #[error("cloud progress callback failed")]
    Callback,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(wire: serde_json::Value) -> CloudToken {
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&wire).expect("wire JSON"));
        format!("{TOKEN_PREFIX}{encoded}")
            .parse()
            .expect("nonempty token")
    }

    #[test]
    fn token_carries_public_enrollment_and_redacts_callback_authority() {
        let envelope = CloudEnvelope::decode(&token(serde_json::json!({
            "callback_url": "https://cloud.example/bootstrap",
            "callback_token": "callback-secret",
            "peer_id": "cloud-peer",
            "peer_name": "Ployz Cloud",
            "public_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        })))
        .expect("token decodes");
        let debug = format!("{envelope:?}");
        assert!(!debug.contains("callback-secret"));
        assert!(!debug.contains("cloud.example"));
        assert!(debug.contains("Ployz Cloud"));
    }

    #[test]
    fn callback_payload_has_no_bootstrap_answers_or_credentials() {
        let progress = CloudProgress::Failed {
            cluster_id: ClusterName::try_new("cluster-a").expect("cluster"),
            machine_id: Some(MachineName::try_new("machine-a").expect("machine")),
            reason: "bounded failure".to_owned(),
            repair_command: None,
        };
        let payload = serde_json::to_string(&progress).expect("progress JSON");
        for forbidden in [
            "storage",
            "container_network",
            "service_urls",
            "callback_token",
        ] {
            assert!(!payload.contains(forbidden));
        }
    }

    #[test]
    fn repeated_progress_has_identical_body_and_idempotency_key() {
        let progress = CloudProgress::Ready {
            cluster_id: ClusterName::try_new("cluster-a").expect("cluster"),
            machine_id: MachineName::try_new("machine-a").expect("machine"),
            machine_public_key: WireGuardPublicKey::try_new(
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            )
            .expect("public key"),
        };
        assert_eq!(
            serde_json::to_vec(&progress).expect("first body"),
            serde_json::to_vec(&progress).expect("replayed body")
        );
        assert_eq!(progress.idempotency_key(), progress.idempotency_key());
    }

    #[test]
    fn preflight_failure_has_no_fake_machine_and_a_stable_key() {
        let cluster_id = ClusterName::try_new("cluster-a").expect("cluster");
        let progress = CloudProgress::Failed {
            cluster_id: cluster_id.clone(),
            machine_id: None,
            reason: "foreign state".to_owned(),
            repair_command: Some("ployz machine reset".to_owned()),
        };
        let payload = serde_json::to_string(&progress).expect("preflight failure JSON");
        assert!(!payload.contains("machine_id"));
        assert_eq!(
            progress.idempotency_key(),
            format!("ployz-init-{cluster_id}-preflight-failed")
        );
    }

    #[test]
    fn invalid_tokens_do_not_echo_the_secret() {
        let token: CloudToken = "not-a-token-secret".parse().expect("opaque token");
        let error = CloudEnvelope::decode(&token).expect_err("invalid token");
        assert!(!error.to_string().contains(token.expose()));
    }

    #[test]
    fn callback_url_cannot_smuggle_authority_into_debug_or_errors() {
        for callback_url in [
            "https://user:secret@cloud.example/bootstrap",
            "https://cloud.example/bootstrap?token=secret",
            "https://cloud.example/bootstrap#secret",
        ] {
            let opaque = token(serde_json::json!({
                "callback_url": callback_url,
                "callback_token": "callback-secret",
                "peer_id": "cloud-peer",
                "peer_name": "Ployz Cloud",
                "public_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            }));
            let error = CloudEnvelope::decode(&opaque).expect_err("unsafe callback URL");
            let rendered = error.to_string();
            assert!(!rendered.contains("secret"));
            assert!(!rendered.contains(opaque.expose()));
        }
    }
}
