//! What a failed run found on its Servers, which a Failure carries and prints after the
//! outer command adds its context.

use ployz_core::{MachineId, QualifiedService, ServiceName};
use ployz_store::tools::LogText;

/// Additional execution evidence printed only after the outer command adds context.
#[derive(Debug, Default)]
pub(crate) struct Diagnostic {
    pub failed_machine: Option<MachineId>,
    /// How a new Container that replaced none failed to start on `failed_machine`. `None`
    /// when the outcome there is unknown, as after a lost connection or a listing that
    /// failed, or when an old Container was brought back in its place.
    pub start_failure: Option<StartFailure>,
    pub failures: Vec<Detail>,
    pub logs: Vec<LogTail>,
    pub compensation: Vec<Detail>,
}

/// How a Deploy's new Container failed to start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartFailure {
    /// Docker refused it a port something else holds, so it never ran.
    PortTaken,
    /// Its health check saw it stop, or never turn healthy. It may still run, or run again
    /// under Docker's restart policy, until it's gone from its Server.
    Unhealthy,
}

/// One distinct failed task or observed compensation action.
#[derive(Debug)]
pub(crate) struct Detail {
    pub message: String,
    pub causes: Vec<String>,
}

/// Scrubbed log evidence. `None` means no Container was involved, so there is no log
/// to show or withhold.
#[derive(Debug)]
pub(crate) struct LogTail {
    /// The runtime identity, which a rename keeps; JSON reports it.
    pub service: QualifiedService,
    /// The Service's name now, which the human header shows.
    pub name: ServiceName,
    pub machine: MachineId,
    pub server: String,
    pub log: Option<LogText>,
}

impl Diagnostic {
    pub(crate) fn json(&self) -> serde_json::Value {
        let detail =
            |value: &Detail| serde_json::json!({"message": value.message, "cause": value.causes});
        serde_json::json!({
            "failed_machine": self.failed_machine,
            "failures": self.failures.iter().map(detail).collect::<Vec<_>>(),
            "logs": self.logs.iter().map(|tail| serde_json::json!({"service": tail.service, "machine_id": tail.machine, "server": tail.server, "log": tail.log})).collect::<Vec<_>>(),
            "compensation": self.compensation.iter().map(detail).collect::<Vec<_>>(),
        })
    }
}
