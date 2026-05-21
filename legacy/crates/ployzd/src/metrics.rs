use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ployz_metrics::register_metric;
use ployz_runtime_backends::runtime::{ContainerEngine, WorkloadResourceSnapshot};
use prometheus::{
    CounterVec, GaugeVec, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts,
};
use tokio_util::sync::CancellationToken;

use crate::daemon::handlers::RequestLane;

const CONTAINER_RESOURCE_LABELS: [&str; 4] = ["namespace", "service", "instance_id", "machine_id"];
const CONTAINER_RESOURCE_POLL_INTERVAL: Duration = Duration::from_secs(15);
const BYTES_PER_MEBIBYTE: f64 = 1024.0 * 1024.0;

static REQUESTS_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();
static REQUEST_DURATION: OnceLock<HistogramVec> = OnceLock::new();
static CONTAINER_CPU_USAGE_SECONDS_TOTAL: OnceLock<CounterVec> = OnceLock::new();
static CONTAINER_MEMORY_USAGE_BYTES: OnceLock<IntGaugeVec> = OnceLock::new();
static CONTAINER_MEMORY_LIMIT_BYTES: OnceLock<IntGaugeVec> = OnceLock::new();
static CONTAINER_CPU_USAGE_CORES: OnceLock<GaugeVec> = OnceLock::new();
static CONTAINER_CPU_USAGE_PERCENT: OnceLock<GaugeVec> = OnceLock::new();
static CONTAINER_MEMORY_USAGE_MEBIBYTES: OnceLock<GaugeVec> = OnceLock::new();
static CONTAINER_MEMORY_LIMIT_MEBIBYTES: OnceLock<GaugeVec> = OnceLock::new();

pub fn observe_request(request: &str, lane: RequestLane, outcome_ok: bool, duration: Duration) {
    let lane = lane_label(lane);
    let outcome = if outcome_ok { "ok" } else { "error" };

    register_metric(&REQUESTS_TOTAL, || {
        let metric = IntCounterVec::new(
            Opts::new(
                "ployz_daemon_requests_total",
                "Total daemon requests handled by ployzd.",
            ),
            &["request", "lane", "outcome"],
        )
        .expect("daemon request counter should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("daemon request counter should register");
        metric
    })
    .with_label_values(&[request, lane, outcome])
    .inc();

    register_metric(&REQUEST_DURATION, || {
        let metric = HistogramVec::new(
            HistogramOpts::new(
                "ployz_daemon_request_duration_seconds",
                "Latency of daemon requests handled by ployzd.",
            ),
            &["request", "lane", "outcome"],
        )
        .expect("daemon request histogram should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("daemon request histogram should register");
        metric
    })
    .with_label_values(&[request, lane, outcome])
    .observe(duration.as_secs_f64());
}

#[must_use]
pub fn request_name(request: &ployz_api::DaemonRequest) -> &'static str {
    match request {
        ployz_api::DaemonRequest::Ping => "ping",
        ployz_api::DaemonRequest::Status => "status",
        ployz_api::DaemonRequest::Doctor => "doctor",
        ployz_api::DaemonRequest::DebugTick { .. } => "debug_tick",
        ployz_api::DaemonRequest::MeshList => "mesh_list",
        ployz_api::DaemonRequest::MeshStatus { .. } => "mesh_status",
        ployz_api::DaemonRequest::MeshJoin { .. } => "mesh_join",
        ployz_api::DaemonRequest::MeshReady { .. } => "mesh_ready",
        ployz_api::DaemonRequest::MeshCreate { .. } => "mesh_create",
        ployz_api::DaemonRequest::MeshInit { .. } => "mesh_init",
        ployz_api::DaemonRequest::MeshStart { .. } => "mesh_start",
        ployz_api::DaemonRequest::MeshStop { .. } => "mesh_stop",
        ployz_api::DaemonRequest::MeshDestroy { .. } => "mesh_destroy",
        ployz_api::DaemonRequest::MeshPeerPrepareDestroy { .. } => "mesh_peer_prepare_destroy",
        ployz_api::DaemonRequest::MeshPeerCancelDestroy { .. } => "mesh_peer_cancel_destroy",
        ployz_api::DaemonRequest::MeshPeerExecuteDestroy { .. } => "mesh_peer_execute_destroy",
        ployz_api::DaemonRequest::MachineList => "machine_list",
        ployz_api::DaemonRequest::MachineRtt => "machine_rtt",
        ployz_api::DaemonRequest::MeshPeerRttSnapshot => "mesh_peer_rtt_snapshot",
        ployz_api::DaemonRequest::MachineInit { .. } => "machine_init",
        ployz_api::DaemonRequest::MachineAdd { .. } => "machine_add",
        ployz_api::DaemonRequest::MachineStoragePromote { .. } => "machine_storage_promote",
        ployz_api::DaemonRequest::MachineUpdate { .. } => "machine_update",
        ployz_api::DaemonRequest::MeshPeerPrepareUpdate { .. } => "mesh_peer_prepare_update",
        ployz_api::DaemonRequest::MeshPeerExecuteUpdate { .. } => "mesh_peer_execute_update",
        ployz_api::DaemonRequest::MachineActivate { .. } => "machine_activate",
        ployz_api::DaemonRequest::MachineDrain { .. } => "machine_drain",
        ployz_api::DaemonRequest::MachineStandby { .. } => "machine_standby",
        ployz_api::DaemonRequest::MachineRemove { .. } => "machine_remove",
        ployz_api::DaemonRequest::MeshPeerRemoveMachine { .. } => "mesh_peer_remove_machine",
        ployz_api::DaemonRequest::MachineOperationList => "machine_operation_list",
        ployz_api::DaemonRequest::MachineOperationGet { .. } => "machine_operation_get",
        ployz_api::DaemonRequest::MachineInviteCreate { .. } => "machine_invite_create",
        ployz_api::DaemonRequest::MachineInviteRevoke { .. } => "machine_invite_revoke",
        ployz_api::DaemonRequest::MachineInviteList => "machine_invite_list",
        ployz_api::DaemonRequest::MachineInviteImport { .. } => "machine_invite_import",
        ployz_api::DaemonRequest::MeshBootstrap { .. } => "mesh_bootstrap",
        ployz_api::DaemonRequest::MachineTransitionSelf { .. } => "machine_transition_self",
        ployz_api::DaemonRequest::MachineStoragePromoteSelf { .. } => {
            "machine_storage_promote_self"
        }
        ployz_api::DaemonRequest::MachineStorageRestoreSelf { .. } => {
            "machine_storage_restore_self"
        }
        ployz_api::DaemonRequest::AcmeChallengeReady { .. } => "acme_challenge_ready",
        ployz_api::DaemonRequest::AcmeHttp01Status { .. } => "acme_http01_status",
        ployz_api::DaemonRequest::MeshSelfRecord => "mesh_self_record",
        ployz_api::DaemonRequest::DeployPreview { .. } => "deploy_preview",
        ployz_api::DaemonRequest::DeployPrepare { .. } => "deploy_prepare",
        ployz_api::DaemonRequest::DeployApply { .. } => "deploy_apply",
        ployz_api::DaemonRequest::DeployApplyPrepared { .. } => "deploy_apply_prepared",
        ployz_api::DaemonRequest::DeployExport { .. } => "deploy_export",
        ployz_api::DaemonRequest::BranchNamespace { .. } => "branch_namespace",
        ployz_api::DaemonRequest::BranchApplyPrepared { .. } => "branch_apply_prepared",
        ployz_api::DaemonRequest::BranchEnvironmentStatus { .. } => "branch_environment_status",
        ployz_api::DaemonRequest::BranchEnvironmentList => "branch_environment_list",
        ployz_api::DaemonRequest::MigrateService { .. } => "migrate_service",
        ployz_api::DaemonRequest::ImageStatus { .. } => "image_status",
        ployz_api::DaemonRequest::ImageInspect { .. } => "image_inspect",
        ployz_api::DaemonRequest::ImagePush { .. } => "image_push",
        ployz_api::DaemonRequest::ImageDistribute { .. } => "image_distribute",
        ployz_api::DaemonRequest::ImageReceiveSession { .. } => "image_receive_session",
        ployz_api::DaemonRequest::ImageReceivedImport { .. } => "image_received_import",
        ployz_api::DaemonRequest::ImageOperationGet { .. } => "image_operation_get",
        ployz_api::DaemonRequest::ImageOperationList => "image_operation_list",
        ployz_api::DaemonRequest::BuildLocal { .. } => "build_local",
        ployz_api::DaemonRequest::BuildMachine { .. } => "build_machine",
        ployz_api::DaemonRequest::BuildOperationGet { .. } => "build_operation_get",
        ployz_api::DaemonRequest::BuildOperationList => "build_operation_list",
        ployz_api::DaemonRequest::DeployNodeInspectNamespace { .. } => {
            "deploy_node_inspect_namespace"
        }
        ployz_api::DaemonRequest::DeployNodeStartCandidate { .. } => "deploy_node_start_candidate",
        ployz_api::DaemonRequest::DeployNodeDrainInstance { .. } => "deploy_node_drain_instance",
        ployz_api::DaemonRequest::DeployNodeRemoveInstance { .. } => "deploy_node_remove_instance",
        ployz_api::DaemonRequest::DeployNodeCloneVolume { .. } => "deploy_node_clone_volume",
        ployz_api::DaemonRequest::DeployNodeCleanupUncommittedVolumeClone { .. } => {
            "deploy_node_cleanup_uncommitted_volume_clone"
        }
        ployz_api::DaemonRequest::RuntimeSubscribe => "runtime_subscribe",
        ployz_api::DaemonRequest::VolumeZfsInspect { .. } => "volume_zfs_inspect",
        ployz_api::DaemonRequest::VolumeZfsSnapshot { .. } => "volume_zfs_snapshot",
        ployz_api::DaemonRequest::VolumeZfsSend { .. } => "volume_zfs_send",
        ployz_api::DaemonRequest::VolumeZfsPeerSnapshot { .. } => "volume_zfs_peer_snapshot",
        ployz_api::DaemonRequest::VolumeZfsPeerSnapshotGuid { .. } => {
            "volume_zfs_peer_snapshot_guid"
        }
        ployz_api::DaemonRequest::VolumeZfsPeerStartSend { .. } => "volume_zfs_peer_start_send",
        ployz_api::DaemonRequest::VolumeZfsTransferGet { .. } => "volume_zfs_transfer_get",
        ployz_api::DaemonRequest::VolumeZfsTransferList => "volume_zfs_transfer_list",
    }
}

#[async_trait]
pub(crate) trait ContainerResourceMetricsSource: Send + Sync {
    async fn list_workload_resource_snapshots(
        &self,
    ) -> Result<Vec<WorkloadResourceSnapshot>, String>;
}

pub(crate) struct DockerContainerResourceMetricsSource;

#[async_trait]
impl ContainerResourceMetricsSource for DockerContainerResourceMetricsSource {
    async fn list_workload_resource_snapshots(
        &self,
    ) -> Result<Vec<WorkloadResourceSnapshot>, String> {
        let engine = ContainerEngine::connect()
            .await
            .map_err(|error| format!("connect docker engine for container metrics: {error}"))?;
        engine
            .list_workload_resource_snapshots()
            .await
            .map_err(|error| format!("list workload container metrics: {error}"))
    }
}

pub(crate) fn spawn_container_resource_metrics_loop(
    cancel: CancellationToken,
    source: Arc<dyn ContainerResourceMetricsSource>,
) {
    tokio::spawn(async move {
        let mut collector = ContainerResourceMetricsCollector::default();
        let mut interval = tokio::time::interval(CONTAINER_RESOURCE_POLL_INTERVAL);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    match source.list_workload_resource_snapshots().await {
                        Ok(snapshots) => collector.observe_snapshots(snapshots, Instant::now()),
                        Err(error) => tracing::warn!(%error, "failed to refresh container resource metrics"),
                    }
                }
            }
        }
    });
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ContainerResourceSeriesKey {
    namespace: String,
    service: String,
    instance_id: String,
    machine_id: String,
}

#[derive(Debug, Clone, Copy)]
struct CpuBaseline {
    cpu_total_seconds: f64,
    sampled_at: Instant,
}

#[derive(Default)]
pub(crate) struct ContainerResourceMetricsCollector {
    baselines: HashMap<ContainerResourceSeriesKey, CpuBaseline>,
}

impl ContainerResourceMetricsCollector {
    pub(crate) fn observe_snapshots(
        &mut self,
        snapshots: Vec<WorkloadResourceSnapshot>,
        sampled_at: Instant,
    ) {
        let mut observed_keys = HashSet::new();

        for snapshot in snapshots {
            let key = ContainerResourceSeriesKey::from_snapshot(&snapshot);
            observed_keys.insert(key.clone());
            self.observe_snapshot(key, snapshot, sampled_at);
        }

        let stale_keys: Vec<_> = self
            .baselines
            .keys()
            .filter(|key| !observed_keys.contains(*key))
            .cloned()
            .collect();
        for key in stale_keys {
            remove_container_resource_series(&key);
            self.baselines.remove(&key);
        }
    }

    fn observe_snapshot(
        &mut self,
        key: ContainerResourceSeriesKey,
        snapshot: WorkloadResourceSnapshot,
        sampled_at: Instant,
    ) {
        let label_values = key.label_values();
        let memory_usage_bytes = saturating_u64_to_i64(snapshot.memory_usage_bytes);
        let memory_limit_bytes = saturating_u64_to_i64(snapshot.memory_limit_bytes);

        container_memory_usage_bytes()
            .with_label_values(&label_values)
            .set(memory_usage_bytes);
        container_memory_limit_bytes()
            .with_label_values(&label_values)
            .set(memory_limit_bytes);
        container_memory_usage_mebibytes()
            .with_label_values(&label_values)
            .set(snapshot.memory_usage_bytes as f64 / BYTES_PER_MEBIBYTE);
        container_memory_limit_mebibytes()
            .with_label_values(&label_values)
            .set(snapshot.memory_limit_bytes as f64 / BYTES_PER_MEBIBYTE);

        let Some(previous) = self.baselines.get(&key).copied() else {
            container_cpu_usage_seconds_total()
                .with_label_values(&label_values)
                .inc_by(snapshot.cpu_total_seconds);
            container_cpu_usage_cores()
                .with_label_values(&label_values)
                .set(0.0);
            container_cpu_usage_percent()
                .with_label_values(&label_values)
                .set(0.0);
            self.baselines.insert(
                key,
                CpuBaseline {
                    cpu_total_seconds: snapshot.cpu_total_seconds,
                    sampled_at,
                },
            );
            return;
        };

        if snapshot.cpu_total_seconds < previous.cpu_total_seconds {
            remove_container_resource_series(&key);
            self.baselines.remove(&key);
            return;
        }

        let cpu_delta = snapshot.cpu_total_seconds - previous.cpu_total_seconds;
        container_cpu_usage_seconds_total()
            .with_label_values(&label_values)
            .inc_by(cpu_delta);

        let elapsed = sampled_at.duration_since(previous.sampled_at).as_secs_f64();
        let cpu_usage_cores = if elapsed > 0.0 {
            cpu_delta / elapsed
        } else {
            0.0
        };
        container_cpu_usage_cores()
            .with_label_values(&label_values)
            .set(cpu_usage_cores);
        container_cpu_usage_percent()
            .with_label_values(&label_values)
            .set(cpu_usage_cores * 100.0);

        self.baselines.insert(
            key,
            CpuBaseline {
                cpu_total_seconds: snapshot.cpu_total_seconds,
                sampled_at,
            },
        );
    }
}

impl ContainerResourceSeriesKey {
    fn from_snapshot(snapshot: &WorkloadResourceSnapshot) -> Self {
        Self {
            namespace: snapshot.namespace.clone(),
            service: snapshot.service.clone(),
            instance_id: snapshot.instance_id.clone(),
            machine_id: snapshot.machine_id.clone(),
        }
    }

    fn label_values(&self) -> [&str; 4] {
        [
            self.namespace.as_str(),
            self.service.as_str(),
            self.instance_id.as_str(),
            self.machine_id.as_str(),
        ]
    }
}

fn container_cpu_usage_seconds_total() -> &'static CounterVec {
    register_metric(&CONTAINER_CPU_USAGE_SECONDS_TOTAL, || {
        let metric = CounterVec::new(
            Opts::new(
                "ployz_container_cpu_usage_seconds_total",
                "Total CPU time consumed by managed ployz workload containers.",
            ),
            &CONTAINER_RESOURCE_LABELS,
        )
        .expect("container cpu total counter should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("container cpu total counter should register");
        metric
    })
}

fn container_memory_usage_bytes() -> &'static IntGaugeVec {
    register_metric(&CONTAINER_MEMORY_USAGE_BYTES, || {
        let metric = IntGaugeVec::new(
            Opts::new(
                "ployz_container_memory_usage_bytes",
                "Current memory usage for managed ployz workload containers.",
            ),
            &CONTAINER_RESOURCE_LABELS,
        )
        .expect("container memory usage gauge should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("container memory usage gauge should register");
        metric
    })
}

fn container_memory_limit_bytes() -> &'static IntGaugeVec {
    register_metric(&CONTAINER_MEMORY_LIMIT_BYTES, || {
        let metric = IntGaugeVec::new(
            Opts::new(
                "ployz_container_memory_limit_bytes",
                "Current memory limit for managed ployz workload containers.",
            ),
            &CONTAINER_RESOURCE_LABELS,
        )
        .expect("container memory limit gauge should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("container memory limit gauge should register");
        metric
    })
}

fn container_cpu_usage_cores() -> &'static GaugeVec {
    register_metric(&CONTAINER_CPU_USAGE_CORES, || {
        let metric = GaugeVec::new(
            Opts::new(
                "ployz_container_cpu_usage_cores",
                "CPU usage in cores for managed ployz workload containers.",
            ),
            &CONTAINER_RESOURCE_LABELS,
        )
        .expect("container cpu cores gauge should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("container cpu cores gauge should register");
        metric
    })
}

fn container_cpu_usage_percent() -> &'static GaugeVec {
    register_metric(&CONTAINER_CPU_USAGE_PERCENT, || {
        let metric = GaugeVec::new(
            Opts::new(
                "ployz_container_cpu_usage_percent",
                "CPU usage percentage for managed ployz workload containers.",
            ),
            &CONTAINER_RESOURCE_LABELS,
        )
        .expect("container cpu percent gauge should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("container cpu percent gauge should register");
        metric
    })
}

fn container_memory_usage_mebibytes() -> &'static GaugeVec {
    register_metric(&CONTAINER_MEMORY_USAGE_MEBIBYTES, || {
        let metric = GaugeVec::new(
            Opts::new(
                "ployz_container_memory_usage_mebibytes",
                "Current memory usage in MiB for managed ployz workload containers.",
            ),
            &CONTAINER_RESOURCE_LABELS,
        )
        .expect("container memory usage MiB gauge should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("container memory usage MiB gauge should register");
        metric
    })
}

fn container_memory_limit_mebibytes() -> &'static GaugeVec {
    register_metric(&CONTAINER_MEMORY_LIMIT_MEBIBYTES, || {
        let metric = GaugeVec::new(
            Opts::new(
                "ployz_container_memory_limit_mebibytes",
                "Current memory limit in MiB for managed ployz workload containers.",
            ),
            &CONTAINER_RESOURCE_LABELS,
        )
        .expect("container memory limit MiB gauge should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("container memory limit MiB gauge should register");
        metric
    })
}

fn remove_container_resource_series(key: &ContainerResourceSeriesKey) {
    let label_values = key.label_values();
    let _ = container_cpu_usage_seconds_total().remove_label_values(&label_values);
    let _ = container_memory_usage_bytes().remove_label_values(&label_values);
    let _ = container_memory_limit_bytes().remove_label_values(&label_values);
    let _ = container_cpu_usage_cores().remove_label_values(&label_values);
    let _ = container_cpu_usage_percent().remove_label_values(&label_values);
    let _ = container_memory_usage_mebibytes().remove_label_values(&label_values);
    let _ = container_memory_limit_mebibytes().remove_label_values(&label_values);
}

fn saturating_u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn lane_label(lane: RequestLane) -> &'static str {
    match lane {
        RequestLane::Shared => "shared",
        RequestLane::Exclusive => "exclusive",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use ployz_metrics::gather_metrics;

    use super::{ContainerResourceMetricsCollector, WorkloadResourceSnapshot};

    #[test]
    fn first_sample_initializes_cpu_baseline_and_zeroes_derived_cpu_gauges() {
        let mut collector = ContainerResourceMetricsCollector::default();
        let name = unique_name("first-sample");
        let now = Instant::now();

        collector.observe_snapshots(vec![snapshot(&name, 5.0, 64, 128)], now);

        let metrics = metrics_output();
        assert!(metrics.contains(&format!(
            "ployz_container_cpu_usage_seconds_total{{instance_id=\"{name}-instance\",machine_id=\"{name}-machine\",namespace=\"{name}-ns\",service=\"{name}-service\"}} 5"
        )));
        assert!(metrics.contains(&format!(
            "ployz_container_cpu_usage_cores{{instance_id=\"{name}-instance\",machine_id=\"{name}-machine\",namespace=\"{name}-ns\",service=\"{name}-service\"}} 0"
        )));
        assert!(metrics.contains(&format!(
            "ployz_container_cpu_usage_percent{{instance_id=\"{name}-instance\",machine_id=\"{name}-machine\",namespace=\"{name}-ns\",service=\"{name}-service\"}} 0"
        )));
    }

    #[test]
    fn second_sample_updates_cpu_rate_metrics() {
        let mut collector = ContainerResourceMetricsCollector::default();
        let name = unique_name("second-sample");
        let first = Instant::now();
        let second = first + Duration::from_secs(10);

        collector.observe_snapshots(vec![snapshot(&name, 10.0, 64, 128)], first);
        collector.observe_snapshots(vec![snapshot(&name, 14.0, 80, 128)], second);

        let metrics = metrics_output();
        assert!(metrics.contains(&format!(
            "ployz_container_cpu_usage_seconds_total{{instance_id=\"{name}-instance\",machine_id=\"{name}-machine\",namespace=\"{name}-ns\",service=\"{name}-service\"}} 14"
        )));
        assert!(metrics.contains(&format!(
            "ployz_container_cpu_usage_cores{{instance_id=\"{name}-instance\",machine_id=\"{name}-machine\",namespace=\"{name}-ns\",service=\"{name}-service\"}} 0.4"
        )));
        assert!(metrics.contains(&format!(
            "ployz_container_cpu_usage_percent{{instance_id=\"{name}-instance\",machine_id=\"{name}-machine\",namespace=\"{name}-ns\",service=\"{name}-service\"}} 40"
        )));
    }

    #[test]
    fn missing_replicas_are_removed_from_exported_series() {
        let mut collector = ContainerResourceMetricsCollector::default();
        let name = unique_name("removed-replica");
        let now = Instant::now();

        collector.observe_snapshots(vec![snapshot(&name, 3.0, 64, 128)], now);
        collector.observe_snapshots(Vec::new(), now + Duration::from_secs(15));

        let metrics = metrics_output();
        assert!(!metrics.contains(&format!("{name}-instance")));
    }

    #[test]
    fn cpu_counter_reset_removes_series_until_next_sample() {
        let mut collector = ContainerResourceMetricsCollector::default();
        let name = unique_name("counter-reset");
        let first = Instant::now();

        collector.observe_snapshots(vec![snapshot(&name, 8.0, 64, 128)], first);
        collector.observe_snapshots(
            vec![snapshot(&name, 2.0, 32, 128)],
            first + Duration::from_secs(15),
        );

        let metrics_after_reset = metrics_output();
        assert!(!metrics_after_reset.contains(&format!("{name}-instance")));

        collector.observe_snapshots(
            vec![snapshot(&name, 4.0, 32, 128)],
            first + Duration::from_secs(30),
        );

        let metrics = metrics_output();
        assert!(metrics.contains(&format!(
            "ployz_container_cpu_usage_seconds_total{{instance_id=\"{name}-instance\",machine_id=\"{name}-machine\",namespace=\"{name}-ns\",service=\"{name}-service\"}} 4"
        )));
        assert!(metrics.contains(&format!(
            "ployz_container_cpu_usage_cores{{instance_id=\"{name}-instance\",machine_id=\"{name}-machine\",namespace=\"{name}-ns\",service=\"{name}-service\"}} 0"
        )));
    }

    fn snapshot(
        name: &str,
        cpu_total_seconds: f64,
        memory_usage_bytes: u64,
        memory_limit_bytes: u64,
    ) -> WorkloadResourceSnapshot {
        WorkloadResourceSnapshot {
            namespace: format!("{name}-ns"),
            service: format!("{name}-service"),
            instance_id: format!("{name}-instance"),
            machine_id: format!("{name}-machine"),
            cpu_total_seconds,
            memory_usage_bytes,
            memory_limit_bytes,
        }
    }

    fn metrics_output() -> String {
        let body = gather_metrics().expect("metrics should encode");
        String::from_utf8(body).expect("metrics should be utf-8")
    }

    fn unique_name(prefix: &str) -> String {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{id}")
    }
}
