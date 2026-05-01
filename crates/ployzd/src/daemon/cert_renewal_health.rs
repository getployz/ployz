use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

pub const NATS_CERT_RENEWAL_HEALTH_FILE: &str = "nats-cert-renewal-health.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CertRenewalWorkerHealth {
    pub healthy: bool,
    pub updated_at_unix_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_since_unix_secs: Option<u64>,
    pub consecutive_failures: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

pub async fn load_health(path: impl AsRef<Path>) -> std::io::Result<CertRenewalWorkerHealth> {
    let bytes = tokio::fs::read(path).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

pub async fn record_healthy(path: &Path) {
    write_health(path, healthy_health()).await;
}

pub async fn record_unhealthy(
    path: &Path,
    consecutive_failures: &mut u64,
    stale_since_unix_secs: &mut Option<u64>,
    error: impl Into<String>,
) {
    *consecutive_failures = consecutive_failures.saturating_add(1);
    let now = unix_secs();
    let stale_since = *stale_since_unix_secs.get_or_insert(now);
    write_health(
        path,
        CertRenewalWorkerHealth {
            healthy: false,
            updated_at_unix_secs: now,
            stale_since_unix_secs: Some(stale_since),
            consecutive_failures: *consecutive_failures,
            last_error: Some(error.into()),
        },
    )
    .await;
}

fn healthy_health() -> CertRenewalWorkerHealth {
    CertRenewalWorkerHealth {
        healthy: true,
        updated_at_unix_secs: unix_secs(),
        stale_since_unix_secs: None,
        consecutive_failures: 0,
        last_error: None,
    }
}

async fn write_health(path: &Path, health: CertRenewalWorkerHealth) {
    let Ok(payload) = serde_json::to_vec_pretty(&health) else {
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        warn!(path = %path.display(), %error, "failed to create cert renewal health directory");
        return;
    }
    if let Err(error) = tokio::fs::write(path, payload).await {
        warn!(path = %path.display(), %error, "failed to write cert renewal health");
    } else {
        debug!(path = %path.display(), "wrote cert renewal health");
    }
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::{
        CertRenewalWorkerHealth, healthy_health, load_health, record_unhealthy, write_health,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn cert_renewal_health_roundtrip() {
        let path = temp_path("cert-renewal-health-roundtrip").join("health.json");
        let health = CertRenewalWorkerHealth {
            healthy: false,
            updated_at_unix_secs: 1_777_646_000,
            stale_since_unix_secs: Some(1_777_646_000),
            consecutive_failures: 2,
            last_error: Some(String::from("fetch failed")),
        };

        write_health(&path, health.clone()).await;

        let loaded = load_health(path).await.expect("load health");
        assert_eq!(loaded, health);
    }

    #[tokio::test]
    async fn cert_renewal_unhealthy_keeps_original_stale_since() {
        let path = temp_path("cert-renewal-health-stale").join("health.json");
        let mut failures = 0;
        let mut stale_since = None;

        record_unhealthy(&path, &mut failures, &mut stale_since, "first").await;
        let first = load_health(path.clone()).await.expect("load first health");
        record_unhealthy(&path, &mut failures, &mut stale_since, "second").await;
        let second = load_health(path).await.expect("load second health");

        assert_eq!(first.stale_since_unix_secs, second.stale_since_unix_secs);
        assert_eq!(second.consecutive_failures, 2);
        assert_eq!(second.last_error.as_deref(), Some("second"));
    }

    #[test]
    fn cert_renewal_healthy_state_is_fresh() {
        let health = healthy_health();

        assert!(health.healthy);
        assert_eq!(health.consecutive_failures, 0);
        assert_eq!(health.stale_since_unix_secs, None);
        assert_eq!(health.last_error, None);
    }

    fn temp_path(label: &str) -> PathBuf {
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}-{sequence}", std::process::id()))
    }
}
