use std::path::Path;

pub const NATS_CERT_RENEWAL_HEALTH_FILE: &str = "nats-cert-renewal-health.json";

pub type CertRenewalWorkerHealth = ployz_supervision::ComponentHealth;

pub async fn load_health(path: impl AsRef<Path>) -> std::io::Result<CertRenewalWorkerHealth> {
    ployz_supervision::load_component_health(path).await
}

#[cfg(test)]
mod tests {
    use super::{CertRenewalWorkerHealth, load_health};
    use ployz_supervision::FileHealthRecorder;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn cert_renewal_health_roundtrip() {
        let path = temp_path("cert-renewal-health-roundtrip").join("health.json");
        let first = CertRenewalWorkerHealth::stale(1_777_646_000, None, "first");
        let health = CertRenewalWorkerHealth::stale(1_777_646_100, Some(&first), "fetch failed");

        ployz_supervision::write_component_health(&path, &health)
            .await
            .expect("write health");

        let loaded = load_health(path).await.expect("load health");
        assert_eq!(loaded, health);
    }

    #[tokio::test]
    async fn cert_renewal_unhealthy_keeps_original_stale_since() {
        let path = temp_path("cert-renewal-health-stale").join("health.json");
        let recorder = FileHealthRecorder::new(path.clone());

        recorder
            .record_unhealthy("first")
            .await
            .expect("record first");
        let first = load_health(path.clone()).await.expect("load first health");
        recorder
            .record_unhealthy("second")
            .await
            .expect("record second");
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
