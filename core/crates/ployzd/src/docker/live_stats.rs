//! One reading of what running Ployz Service containers use, from Docker's stats, for
//! `ployz metrics`.

use std::time::Duration;

use bollard::{
    models::{ContainerMemoryStats, ContainerStatsResponse},
    query_parameters::StatsOptionsBuilder,
};
use futures_util::{StreamExt as _, future::join_all};
use ployz_core::{
    ContainerId, QualifiedService, labels,
    metrics::{LiveContainer, MetricScope},
};

use super::{Error, LocalDocker};

/// Docker waits for a second sample before it answers, to give a CPU rate.
const STATS_TIMEOUT: Duration = Duration::from_secs(5);

impl LocalDocker {
    /// A container Docker does not answer for in time is left out.
    pub(crate) async fn live_stats(
        &self,
        scope: &MetricScope,
    ) -> Result<Vec<LiveContainer>, Error> {
        let mut filter = vec![
            labels::MANAGED.to_owned(),
            labels::SERVICE_ID.to_owned(),
            labels::NAMESPACE.to_owned(),
            labels::SERVICE_NAME.to_owned(),
        ];
        if let MetricScope::Service { service } = scope {
            filter.push(format!("{}={}", labels::NAMESPACE, service.namespace));
            filter.push(format!("{}={}", labels::SERVICE_NAME, service.name));
        }
        let containers =
            self.running_labelled(filter)
                .await?
                .into_iter()
                .filter_map(|(id, labelled)| {
                    if labelled.contains_key(labels::HOOK) {
                        return None;
                    }
                    let (_, service) = labels::service_pair(&labelled)?;
                    Some((id, service))
                });
        let machine_memory = self
            .client
            .info()
            .await?
            .mem_total
            .and_then(|total| u64::try_from(total).ok());
        let read =
            join_all(containers.map(|(id, service)| self.read_stats(id, service, machine_memory)))
                .await;
        Ok(read.into_iter().flatten().collect())
    }

    async fn read_stats(
        &self,
        id: ContainerId,
        service: QualifiedService,
        machine_memory: Option<u64>,
    ) -> Option<LiveContainer> {
        let options = StatsOptionsBuilder::default()
            .stream(false)
            .one_shot(false)
            .build();
        let mut stats = self.client.stats(id.as_str(), Some(options));
        let stats = match tokio::time::timeout(STATS_TIMEOUT, stats.next()).await {
            Ok(Some(Ok(stats))) => stats,
            Ok(Some(Err(error))) => {
                tracing::debug!(container = %id, %error, "cannot read container stats");
                return None;
            }
            Ok(None) | Err(_) => return None,
        };
        Some(live(id, service, &stats, machine_memory))
    }
}

fn live(
    container_id: ContainerId,
    service: QualifiedService,
    stats: &ContainerStatsResponse,
    machine_memory: Option<u64>,
) -> LiveContainer {
    let networks = stats.networks.as_ref();
    let network = |bytes: fn(&bollard::models::ContainerNetworkStats) -> Option<u64>| {
        networks.map(|networks| networks.values().filter_map(bytes).sum())
    };
    let blkio = |op: &str| {
        stats
            .blkio_stats
            .as_ref()?
            .io_service_bytes_recursive
            .as_ref()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| {
                        entry
                            .op
                            .as_deref()
                            .is_some_and(|name| name.eq_ignore_ascii_case(op))
                    })
                    .filter_map(|entry| entry.value)
                    .sum()
            })
    };
    let memory = stats.memory_stats.as_ref();
    LiveContainer {
        container_id,
        service,
        cpu: cpu(stats),
        mem_bytes: memory.and_then(in_use),
        mem_limit: own_limit(memory.and_then(|memory| memory.limit), machine_memory),
        rx: network(|network| network.rx_bytes),
        tx: network(|network| network.tx_bytes),
        io_r: blkio("read"),
        io_w: blkio("write"),
    }
}

/// Docker reports the Machine's memory as the limit of a container that has none. Without a
/// Machine total to tell the two apart, no limit is claimed.
fn own_limit(limit: Option<u64>, machine_memory: Option<u64>) -> Option<u64> {
    limit.filter(|limit| Some(*limit) < machine_memory)
}

/// Memory as `docker stats` shows it: page cache the kernel can drop is not in use. Its
/// count is `total_inactive_file` under cgroup v1 and `inactive_file` under v2.
fn in_use(memory: &ContainerMemoryStats) -> Option<u64> {
    let usage = memory.usage?;
    let stats = memory.stats.as_ref();
    let inactive = stats
        .and_then(|stats| stats.get("total_inactive_file"))
        .or_else(|| stats.and_then(|stats| stats.get("inactive_file")))
        .copied()
        .filter(|inactive| *inactive < usage)
        .unwrap_or(0);
    Some(usage - inactive)
}

fn cpu(stats: &ContainerStatsResponse) -> Option<f64> {
    let now = stats.cpu_stats.as_ref()?;
    let before = stats.precpu_stats.as_ref()?;
    let used = now
        .cpu_usage
        .as_ref()?
        .total_usage?
        .checked_sub(before.cpu_usage.as_ref()?.total_usage?)?;
    let elapsed = now
        .system_cpu_usage?
        .checked_sub(before.system_cpu_usage?)?;
    if elapsed == 0 {
        return None;
    }
    #[expect(clippy::cast_precision_loss, reason = "a ratio of nanosecond counters")]
    Some(used as f64 / elapsed as f64 * f64::from(now.online_cpus?))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bollard::models::ContainerMemoryStats;

    use super::{in_use, own_limit};

    fn memory(stat: &str) -> ContainerMemoryStats {
        ContainerMemoryStats {
            usage: Some(500),
            stats: Some(HashMap::from([(stat.to_owned(), 400)])),
            ..Default::default()
        }
    }

    #[test]
    fn page_cache_is_left_out_under_cgroup_v1_and_v2() {
        assert_eq!(in_use(&memory("total_inactive_file")), Some(100));
        assert_eq!(in_use(&memory("inactive_file")), Some(100));
    }

    #[test]
    fn a_container_without_its_own_limit_claims_none() {
        let machine = Some(4_100);
        assert_eq!(own_limit(Some(512), machine), Some(512));
        assert_eq!(own_limit(Some(4_100), machine), None);
        assert_eq!(own_limit(Some(512), None), None);
        assert_eq!(own_limit(None, machine), None);
    }
}
