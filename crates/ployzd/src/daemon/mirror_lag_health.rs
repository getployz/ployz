//! Periodic mirror-lag observation for Mirror nodes.
//!
//! Audience (per `AGENTS.md` failure-audience rule): the operator
//! reading `ployzctl status`. The daemon polls
//! `ployz_nats::buckets::mirror_lag` every 30s and writes the result to
//! a JSON file alongside the existing cert-renewal health surface.
//! Status RPC reads the file and renders it. If the poll fails for
//! `consecutive_failures` ticks, the status output reports
//! `SnapshotHealth::Stale` so a stalled mirror never silently serves
//! out-of-date reads.
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz_nats::NatsStore;
use ployz_nats::buckets::{MirrorLag, mirror_lag};
use serde::{Deserialize, Serialize};
use std::future::Future;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

pub const NATS_MIRROR_LAG_HEALTH_FILE: &str = "nats-mirror-lag-health.json";
pub const MIRROR_LAG_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// JSON-serialised health record. Mirrors the shape of
/// `CertRenewalWorkerHealth` so status rendering can stay regular.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MirrorLagHealth {
    pub healthy: bool,
    pub updated_at_unix_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_since_unix_secs: Option<u64>,
    pub consecutive_failures: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub lags: Vec<MirrorLagEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MirrorLagEntry {
    pub asset_name: String,
    pub local_last_seq: u64,
    pub hub_last_seq: u64,
    pub behind_by: u64,
    pub observed_at_unix_secs: u64,
}

impl From<&MirrorLag> for MirrorLagEntry {
    fn from(lag: &MirrorLag) -> Self {
        Self {
            asset_name: lag.asset_name.clone(),
            local_last_seq: lag.local_last_seq,
            hub_last_seq: lag.hub_last_seq,
            behind_by: lag.behind_by(),
            observed_at_unix_secs: lag.observed_at_unix_secs,
        }
    }
}

/// Cancellable owner for the mirror-lag polling task.
pub(crate) struct MirrorLagHealthTask {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl MirrorLagHealthTask {
    pub(crate) fn spawn<F>(run: impl FnOnce(CancellationToken) -> F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(cancel.clone()));
        Self { cancel, task }
    }

    pub(crate) async fn shutdown(self) {
        self.cancel.cancel();
        if let Err(error) = self.task.await {
            warn!(?error, "mirror lag health shutdown error");
        }
    }
}

pub(crate) fn spawn(
    store: NatsStore,
    health_path: std::path::PathBuf,
) -> MirrorLagHealthTask {
    MirrorLagHealthTask::spawn(move |cancel| async move {
        let mut consecutive_failures = 0u64;
        let mut stale_since = None;
        // Run an immediate sample so the first status read has a non-empty
        // health surface even before the first interval tick.
        let _ = poll_once(
            &store,
            &health_path,
            &mut consecutive_failures,
            &mut stale_since,
        )
        .await;
        let mut interval = tokio::time::interval(MIRROR_LAG_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the immediate first tick — we already polled above.
        interval.tick().await;
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = interval.tick() => {
                    let _ = poll_once(
                        &store,
                        &health_path,
                        &mut consecutive_failures,
                        &mut stale_since,
                    )
                    .await;
                }
            }
        }
    })
}

async fn poll_once(
    store: &NatsStore,
    health_path: &Path,
    consecutive_failures: &mut u64,
    stale_since: &mut Option<u64>,
) -> Result<(), ()> {
    match mirror_lag(store).await {
        Ok(lags) => {
            *consecutive_failures = 0;
            *stale_since = None;
            let entries: Vec<MirrorLagEntry> = lags.iter().map(MirrorLagEntry::from).collect();
            write_health(
                health_path,
                MirrorLagHealth {
                    healthy: true,
                    updated_at_unix_secs: unix_secs(),
                    stale_since_unix_secs: None,
                    consecutive_failures: 0,
                    last_error: None,
                    lags: entries,
                },
            )
            .await;
            Ok(())
        }
        Err(error) => {
            *consecutive_failures = consecutive_failures.saturating_add(1);
            let now = unix_secs();
            let stale_since_value = *stale_since.get_or_insert(now);
            warn!(?error, "mirror lag poll failed");
            // Preserve the last successful lag samples so the operator
            // still has per-asset context while health is degraded —
            // dropping them would leave the status surface blank
            // exactly when diagnosis matters most.
            let stale_lags = match load_health(health_path).await {
                Ok(previous) => previous.lags,
                Err(_) => Vec::new(),
            };
            write_health(
                health_path,
                MirrorLagHealth {
                    healthy: false,
                    updated_at_unix_secs: now,
                    stale_since_unix_secs: Some(stale_since_value),
                    consecutive_failures: *consecutive_failures,
                    last_error: Some(error.to_string()),
                    lags: stale_lags,
                },
            )
            .await;
            Err(())
        }
    }
}

pub async fn load_health(path: impl AsRef<Path>) -> std::io::Result<MirrorLagHealth> {
    let bytes = tokio::fs::read(path).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

async fn write_health(path: &Path, health: MirrorLagHealth) {
    let Ok(payload) = serde_json::to_vec_pretty(&health) else {
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        warn!(path = %path.display(), %error, "failed to create mirror lag health directory");
        return;
    }
    if let Err(error) = tokio::fs::write(path, payload).await {
        warn!(path = %path.display(), %error, "failed to write mirror lag health");
    } else {
        debug!(path = %path.display(), "wrote mirror lag health");
    }
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> PathBuf {
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "{label}-{}-{nanos}-{sequence}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn mirror_lag_health_roundtrip() {
        let path = temp_path("mirror-lag-roundtrip").join("health.json");
        let health = MirrorLagHealth {
            healthy: true,
            updated_at_unix_secs: 1_777_646_000,
            stale_since_unix_secs: None,
            consecutive_failures: 0,
            last_error: None,
            lags: vec![MirrorLagEntry {
                asset_name: "machines".into(),
                local_last_seq: 5,
                hub_last_seq: 7,
                behind_by: 2,
                observed_at_unix_secs: 1_777_646_000,
            }],
        };
        write_health(&path, health.clone()).await;
        let loaded = load_health(path).await.expect("load health");
        assert_eq!(loaded, health);
    }

    #[test]
    fn mirror_lag_entry_computes_behind_by_from_seq_diff() {
        let lag = MirrorLag {
            asset_name: "deploy_commits".into(),
            local_last_seq: 10,
            hub_last_seq: 13,
            observed_at_unix_secs: 100,
        };
        let entry = MirrorLagEntry::from(&lag);
        assert_eq!(entry.behind_by, 3);
    }

    #[test]
    fn mirror_lag_entry_clamps_behind_by_when_local_runs_ahead() {
        // Local can briefly run ahead of hub during a source republish.
        let lag = MirrorLag {
            asset_name: "machines".into(),
            local_last_seq: 13,
            hub_last_seq: 10,
            observed_at_unix_secs: 100,
        };
        let entry = MirrorLagEntry::from(&lag);
        assert_eq!(entry.behind_by, 0);
    }
}
