//! Docker event markers a Machine keeps beside its containers, and the live readings
//! `ployz metrics` takes from a Machine's Docker.
//!
//! Timestamps are Unix nanoseconds. A reading whose source was missing or unreadable
//! is `None` and absent from JSON, never zero.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{ContainerId, MachineId, QualifiedService, ServiceId};

/// Something Docker did to a Service container.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricMarker {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub service_id: ServiceId,
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
        /// Docker reported the kernel's OOM killer before this exit.
        #[serde(default)]
        oom: bool,
        /// Docker was asked to kill the container, as `docker stop` does,
        /// before this exit.
        #[serde(default)]
        stopped: bool,
    },
    /// Docker started the container again after it exited.
    Restart,
}

impl MetricMarker {
    /// Whether this marker is an exit nobody asked for: an OOM kill, or a
    /// non-zero exit that no stop or kill request explains. An exit whose code
    /// Docker did not report counts only when it was an OOM kill.
    #[must_use]
    pub fn is_crash(&self) -> bool {
        match self.kind {
            MarkerKind::Exit {
                exit_code,
                oom,
                stopped,
            } => oom || (exit_code.is_some_and(|code| code != 0) && !stopped),
            MarkerKind::Oom | MarkerKind::Restart => false,
        }
    }
}

/// `since_nanos` is inclusive and `until_nanos` exclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricRange {
    pub since_nanos: i64,
    pub until_nanos: i64,
}

/// Which containers a live reading covers on the answering Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetricScope {
    /// Every Ployz Service container.
    Machine,
    Service { service: QualifiedService },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MetricsLiveRequest {
    pub scope: MetricScope,
}

/// One reading of each running container the scope covers, from Docker's stats.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricsLiveReply {
    pub containers: Vec<LiveContainer>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveContainer {
    pub container_id: ContainerId,
    pub service: QualifiedService,
    /// CPUs busy over Docker's last sampling interval, such as 0.5 for half of one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_bytes: Option<u64>,
    /// The Machine's memory when the container has no limit of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_limit: Option<u64>,
    /// Bytes in and out since the container started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rx: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx: Option<u64>,
    /// Bytes read from and written to disk since the container started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_r: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_w: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine_id() -> MachineId {
        MachineId::parse("a".repeat(32)).unwrap()
    }

    fn container_id() -> ContainerId {
        ContainerId::parse("b".repeat(64)).unwrap()
    }

    fn service_id() -> ServiceId {
        ServiceId::parse("c".repeat(32)).unwrap()
    }

    fn service() -> QualifiedService {
        QualifiedService::parse("prod/web").unwrap()
    }

    #[test]
    fn a_marker_names_its_kind_beside_the_container_labels() {
        let marker = MetricMarker {
            machine_id: machine_id(),
            container_id: container_id(),
            service_id: service_id(),
            service: service(),
            deployment_id: Some("d1".into()),
            replica: "web-1".into(),
            ts: 7,
            kind: MarkerKind::Exit {
                exit_code: Some(137),
                oom: true,
                stopped: false,
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
                "oom": true,
                "stopped": false,
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
    fn an_exit_is_a_crash_only_when_nobody_asked_for_it() {
        let exit = |exit_code, oom, stopped| MetricMarker {
            machine_id: machine_id(),
            container_id: container_id(),
            service_id: service_id(),
            service: service(),
            deployment_id: None,
            replica: "web-1".into(),
            ts: 7,
            kind: MarkerKind::Exit {
                exit_code,
                oom,
                stopped,
            },
        };
        assert!(exit(Some(1), false, false).is_crash());
        assert!(exit(Some(137), true, false).is_crash());
        assert!(exit(None, true, true).is_crash());
        assert!(!exit(Some(0), false, false).is_crash());
        assert!(!exit(Some(143), false, true).is_crash());
        assert!(!exit(Some(137), false, true).is_crash());
        assert!(!exit(None, false, false).is_crash());
        let oom = MetricMarker {
            kind: MarkerKind::Oom,
            ..exit(None, false, false)
        };
        assert!(!oom.is_crash());
    }

    #[test]
    fn an_exit_written_before_crash_flags_reads_as_unflagged() {
        let json = serde_json::json!({
            "machine_id": "a".repeat(32),
            "container_id": "b".repeat(64),
            "service_id": "c".repeat(32),
            "service": "prod/web",
            "replica": "web-1",
            "ts": 7,
            "kind": "exit",
            "exit_code": 1,
        });
        let marker = serde_json::from_value::<MetricMarker>(json).unwrap();
        assert_eq!(
            marker.kind,
            MarkerKind::Exit {
                exit_code: Some(1),
                oom: false,
                stopped: false,
            }
        );
    }
}
