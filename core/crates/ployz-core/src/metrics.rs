//! Metric rows a Machine samples into its Observe Store, and the history query
//! that reads them back.
//!
//! Timestamps are Unix nanoseconds. Sampled counters are raw cumulative values
//! as the kernel reports them; readers turn them into rates. A field whose
//! source was missing or unreadable is `None` and absent from JSON, never zero.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{ContainerId, MachineId, QualifiedService, ServiceId};

/// One container's counters at one instant.
///
/// Identity comes from the container's labels, so a sample stays attributed
/// to its Service after the container is gone.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricSample {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    /// Absent for containers no Service owns, such as Ployz system containers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub service_id: Option<ServiceId>,
    /// The Service's Namespace and name, which is how history selects it.
    /// Absent exactly when `service_id` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub service: Option<QualifiedService>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub deployment_id: Option<String>,
    pub replica: String,
    pub ts: i64,
    /// Cumulative CPU time in microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cpu_usec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mem_bytes: Option<u64>,
    /// Absent when the container has no memory limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mem_limit: Option<u64>,
    /// Cumulative bytes in and out over every interface but loopback. The
    /// mesh and public split is a Machine metric; see `HostSample`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rx: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tx: Option<u64>,
    /// Cumulative bytes read from and written to disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub io_r: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub io_w: Option<u64>,
}

/// The Machine's own totals at one instant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct HostSample {
    pub machine_id: MachineId,
    pub ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cpu: Option<CpuJiffies>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub memory: Option<HostMemory>,
    /// Keyed by interface name; empty when the interface table was unreadable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub interfaces: BTreeMap<String, InterfaceBytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub disk: Option<DiskBreakdown>,
}

/// Something Docker did to a Service container, kept beside its samples.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricMarker {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub service_id: ServiceId,
    /// The Service's Namespace and name, which is how history selects it.
    pub service: QualifiedService,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub deployment_id: Option<String>,
    pub replica: String,
    /// When Docker reported the event.
    pub ts: i64,
    #[serde(flatten)]
    #[ts(flatten)]
    pub kind: MarkerKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MarkerKind {
    /// The kernel's OOM killer killed a process in the container.
    Oom,
    /// The container's main process exited.
    Exit {
        /// Absent when Docker did not say.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        exit_code: Option<i64>,
    },
    /// Docker started the container again after it exited.
    Restart,
}

/// Cumulative jiffies across all CPUs. Idle includes iowait.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct CpuJiffies {
    pub busy: u64,
    pub idle: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct HostMemory {
    pub total: u64,
    pub available: u64,
}

/// Cumulative bytes through one network interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct InterfaceBytes {
    pub rx: u64,
    pub tx: u64,
}

/// Where the Machine's disk bytes went. A part is absent when it was not
/// measured, such as Volumes on a Machine without ZFS.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DiskBreakdown {
    pub total: u64,
    pub free: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub volumes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mirror_slots: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub observe_store: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub other: Option<u64>,
}

/// Metric history for one Machine or one Service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricHistoryRequest {
    pub scope: MetricScope,
    pub range: MetricRange,
    /// Bucket width. Absent lets the Machine pick about 300 buckets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub step_nanos: Option<i64>,
}

impl MetricHistoryRequest {
    /// # Errors
    ///
    /// Names the first field outside the contract.
    pub fn validate(&self) -> Result<(), String> {
        if self.range.since_nanos >= self.range.until_nanos {
            return Err("since_nanos must be before until_nanos".into());
        }
        if self.step_nanos.is_some_and(|step| step <= 0) {
            return Err("step_nanos must be positive".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetricScope {
    /// The answering Machine's host totals.
    Machine,
    /// Every container of one Service on the answering Machine, summed.
    Service { service: QualifiedService },
}

/// `since_nanos` is inclusive and `until_nanos` exclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricRange {
    pub since_nanos: i64,
    pub until_nanos: i64,
}

/// Buckets in time order, plus the spans that have no data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricHistoryReply {
    pub points: MetricPoints,
    /// Bucket width used. It can be wider than the request asked, to keep the
    /// reply at most 4,000 buckets, or to fit stored 5 minute rollups.
    pub step_nanos: i64,
    #[serde(default)]
    pub gaps: Vec<MetricGap>,
    /// OOM kills, exits and restarts inside the range, oldest first. A
    /// Service sees its own; a Machine sees every Service's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<MetricMarker>,
}

/// One list of points, shaped by the request's scope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MetricPoints {
    Machine(Vec<HostPoint>),
    Service(Vec<ServicePoint>),
}

/// One bucket of a Service. Byte and CPU fields are per-second rates; memory
/// fields are bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ServicePoint {
    /// Bucket start.
    pub ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cpu_usec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mem_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mem_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rx: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tx: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub io_r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub io_w: Option<f64>,
}

/// One bucket of a Machine's host totals.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct HostPoint {
    /// Bucket start.
    pub ts: i64,
    /// Fraction of all CPUs busy, from 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cpu_busy: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub memory: Option<HostMemory>,
    /// Per-second rates keyed by interface name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub interfaces: BTreeMap<String, InterfaceRate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub disk: Option<DiskBreakdown>,
}

/// Bytes per second through one network interface.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct InterfaceRate {
    pub rx: f64,
    pub tx: f64,
}

/// A span of the requested range with no points.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricGap {
    pub from: i64,
    pub to: i64,
    pub reason: MetricGapReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MetricGapReason {
    /// No sample landed in the span, for example while the daemon was down.
    NotSampled,
    /// The store did not answer within the query deadline.
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MetricSample {
        MetricSample {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            container_id: ContainerId::parse("b".repeat(64)).unwrap(),
            service_id: Some(ServiceId::parse("c".repeat(32)).unwrap()),
            service: Some(QualifiedService::parse("prod/web").unwrap()),
            deployment_id: Some("d1".into()),
            replica: "web-1".into(),
            ts: 1_760_000_000_000_000_000,
            cpu_usec: Some(u64::MAX),
            mem_bytes: Some(10),
            mem_limit: Some(20),
            rx: Some(1),
            tx: Some(2),
            io_r: Some(5),
            io_w: Some(6),
        }
    }

    #[test]
    fn metric_rows_round_trip_through_json() {
        let sample = sample();
        let json = serde_json::to_string(&sample).unwrap();
        assert_eq!(serde_json::from_str::<MetricSample>(&json).unwrap(), sample);

        let host = HostSample {
            machine_id: sample.machine_id,
            ts: sample.ts,
            cpu: Some(CpuJiffies { busy: 7, idle: 8 }),
            memory: Some(HostMemory {
                total: 100,
                available: 40,
            }),
            interfaces: BTreeMap::from([("eth0".into(), InterfaceBytes { rx: 9, tx: 10 })]),
            disk: Some(DiskBreakdown {
                total: 1_000,
                free: 400,
                volumes: Some(300),
                mirror_slots: Some(100),
                observe_store: Some(50),
                other: Some(150),
            }),
        };
        let json = serde_json::to_string(&host).unwrap();
        assert_eq!(serde_json::from_str::<HostSample>(&json).unwrap(), host);

        let request = MetricHistoryRequest {
            scope: MetricScope::Service {
                service: sample.service.clone().unwrap(),
            },
            range: MetricRange {
                since_nanos: 0,
                until_nanos: 300,
            },
            step_nanos: Some(1),
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json.pointer("/scope/kind"), Some(&"service".into()));
        assert_eq!(json.pointer("/scope/service"), Some(&"prod/web".into()));
        assert_eq!(
            serde_json::from_value::<MetricHistoryRequest>(json).unwrap(),
            request
        );

        let reply = MetricHistoryReply {
            points: MetricPoints::Service(vec![ServicePoint {
                ts: 0,
                cpu_usec: Some(1.5),
                mem_bytes: Some(10),
                mem_limit: None,
                rx: Some(0.25),
                tx: None,
                io_r: None,
                io_w: None,
            }]),
            step_nanos: 100,
            gaps: vec![MetricGap {
                from: 100,
                to: 300,
                reason: MetricGapReason::NotSampled,
            }],
            markers: vec![MetricMarker {
                machine_id: sample.machine_id,
                container_id: sample.container_id,
                service_id: sample.service_id.unwrap(),
                service: sample.service.unwrap(),
                deployment_id: None,
                replica: "web-1".into(),
                ts: 150,
                kind: MarkerKind::Restart,
            }],
        };
        let json = serde_json::to_value(&reply).unwrap();
        assert_eq!(json.pointer("/gaps/0/reason"), Some(&"not_sampled".into()));
        assert_eq!(json.pointer("/markers/0/kind"), Some(&"restart".into()));
        assert_eq!(
            serde_json::from_value::<MetricHistoryReply>(json).unwrap(),
            reply
        );

        let quiet = MetricHistoryReply {
            markers: Vec::new(),
            ..reply
        };
        let json = serde_json::to_value(&quiet).unwrap();
        assert!(json.get("markers").is_none());
        assert_eq!(
            serde_json::from_value::<MetricHistoryReply>(json).unwrap(),
            quiet
        );
    }

    #[test]
    fn missing_sources_stay_absent_in_json() {
        let sample = MetricSample {
            service_id: None,
            service: None,
            cpu_usec: None,
            io_w: None,
            ..sample()
        };
        let json = serde_json::to_value(&sample).unwrap();
        for field in ["service_id", "service", "cpu_usec", "io_w"] {
            assert!(json.get(field).is_none(), "{field} must be absent: {json}");
        }
        assert_eq!(json.get("io_r"), Some(&5.into()));
        assert_eq!(
            serde_json::from_value::<MetricSample>(json).unwrap(),
            sample
        );

        let host = HostSample {
            machine_id: sample.machine_id,
            ts: 0,
            cpu: None,
            memory: None,
            interfaces: BTreeMap::new(),
            disk: Some(DiskBreakdown {
                total: 1_000,
                free: 400,
                volumes: None,
                mirror_slots: None,
                observe_store: Some(50),
                other: None,
            }),
        };
        let json = serde_json::to_value(&host).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "machine_id": "a".repeat(32),
                "ts": 0,
                "disk": { "total": 1_000, "free": 400, "observe_store": 50 },
            })
        );
    }

    #[test]
    fn a_marker_names_its_kind_beside_the_container_labels() {
        let sample = sample();
        let marker = MetricMarker {
            machine_id: sample.machine_id,
            container_id: sample.container_id,
            service_id: sample.service_id.unwrap(),
            service: sample.service.unwrap(),
            deployment_id: Some("d1".into()),
            replica: "web-1".into(),
            ts: 7,
            kind: MarkerKind::Exit {
                exit_code: Some(137),
            },
        };
        let json = serde_json::to_value(&marker).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "machine_id": "a".repeat(32),
                "container_id": "b".repeat(64),
                "service_id": "c".repeat(32),
                "service": "prod/web",
                "deployment_id": "d1",
                "replica": "web-1",
                "ts": 7,
                "kind": "exit",
                "exit_code": 137,
            })
        );
        assert_eq!(
            serde_json::from_value::<MetricMarker>(json).unwrap(),
            marker
        );

        let oom = MetricMarker {
            kind: MarkerKind::Oom,
            ..marker
        };
        let json = serde_json::to_value(&oom).unwrap();
        assert_eq!(json.get("kind"), Some(&"oom".into()));
        assert!(json.get("exit_code").is_none());
        assert_eq!(serde_json::from_value::<MetricMarker>(json).unwrap(), oom);
    }

    #[test]
    fn history_requests_need_a_forward_range_and_positive_step() {
        let mut request = MetricHistoryRequest {
            scope: MetricScope::Machine,
            range: MetricRange {
                since_nanos: 10,
                until_nanos: 10,
            },
            step_nanos: None,
        };
        assert!(request.validate().is_err());
        request.range.until_nanos = 11;
        assert_eq!(request.validate(), Ok(()));
        request.step_nanos = Some(0);
        assert!(request.validate().is_err());
    }
}
