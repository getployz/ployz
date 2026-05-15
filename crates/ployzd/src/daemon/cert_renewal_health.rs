use std::path::Path;
use tracing::{debug, warn};

pub const NATS_CERT_RENEWAL_HEALTH_FILE: &str = "nats-cert-renewal-health.json";

pub type CertRenewalWorkerHealth = ployz_supervision::ComponentHealth;

pub async fn load_health(path: impl AsRef<Path>) -> std::io::Result<CertRenewalWorkerHealth> {
    ployz_supervision::load_component_health(path).await
}

pub async fn record_healthy(path: &Path) {
    write_health(path, &ployz_supervision::healthy_component_health()).await;
}

pub async fn record_unhealthy(
    path: &Path,
    health_state: &mut Option<CertRenewalWorkerHealth>,
    error: impl Into<String>,
) {
    let next =
        CertRenewalWorkerHealth::stale(ployz_time::now_unix_secs(), health_state.as_ref(), error);
    *health_state = Some(next.clone());
    write_health(path, &next).await;
}

async fn write_health(path: &Path, health: &CertRenewalWorkerHealth) {
    if let Err(error) = ployz_supervision::write_component_health(path, health).await {
        warn!(path = %path.display(), %error, "failed to write cert renewal health");
    } else {
        debug!(path = %path.display(), "wrote cert renewal health");
    }
}

#[cfg(test)]
mod tests {
    use super::{CertRenewalWorkerHealth, load_health, record_unhealthy, write_health};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn cert_renewal_health_roundtrip() {
        let path = temp_path("cert-renewal-health-roundtrip").join("health.json");
        let first = CertRenewalWorkerHealth::stale(1_777_646_000, None, "first");
        let health = CertRenewalWorkerHealth::stale(1_777_646_100, Some(&first), "fetch failed");

        write_health(&path, &health).await;

        let loaded = load_health(path).await.expect("load health");
        assert_eq!(loaded, health);
    }

    #[tokio::test]
    async fn cert_renewal_unhealthy_keeps_original_stale_since() {
        let path = temp_path("cert-renewal-health-stale").join("health.json");
        let mut health_state = None;

        record_unhealthy(&path, &mut health_state, "first").await;
        let first = load_health(path.clone()).await.expect("load first health");
        record_unhealthy(&path, &mut health_state, "second").await;
        let second = load_health(path).await.expect("load second health");

        let ployz_supervision::ComponentHealthState::Stale {
            stale_since_unix_secs: first_stale_since,
            ..
        } = first.state
        else {
            panic!("first health should be stale");
        };
        let ployz_supervision::ComponentHealthState::Stale {
            stale_since_unix_secs: second_stale_since,
            consecutive_failures,
            last_error,
        } = second.state
        else {
            panic!("second health should be stale");
        };
        assert_eq!(first_stale_since, second_stale_since);
        assert_eq!(consecutive_failures, 2);
        assert_eq!(last_error, "second");
    }

    #[test]
    fn cert_renewal_healthy_state_is_fresh() {
        let health = ployz_supervision::healthy_component_health();

        assert_eq!(
            health.state,
            ployz_supervision::ComponentHealthState::Healthy
        );
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
