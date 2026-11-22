//! A Service's status in one word, from what its Servers report, as the agent's
//! status tool words it. The dashboard's canvas words it by a rule of its own.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What runs now, in one word.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Online,
    /// It runs, but none of its containers serves yet.
    Degraded,
    Crashed,
    /// Nothing runs or crashed, and a Server stopped one of its containers on request.
    Stopped,
    /// It should run and nothing does, or every container exited cleanly.
    NotRunning,
    /// It has never deployed, or has no source to run.
    NotDeployed,
    /// No current evidence says what runs: the Servers can't be reached, are still
    /// answering, or one is missing from the evidence.
    CantReachServers,
    NeedsAServer,
}

/// How one of a Service's containers is doing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Container {
    /// Running and passing its healthcheck, or running without one.
    Serving,
    /// Running, but it has not passed its healthcheck yet.
    Running,
    /// Exited with code 0, created, or paused.
    Idle,
    /// Exited after its Server stopped it on request, whatever its exit code.
    Stopped,
    /// Exited with an error, restarting, or any other state.
    Down,
}

/// What the Runtime Watch says of the Servers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Watch {
    /// The Environment has no Server.
    NoServers,
    /// No Server answered.
    Unreachable,
    /// Current evidence. `whole`: no Server is missing from it.
    Observed { whole: bool },
}

/// What a Service's status is decided from.
#[derive(Clone, Copy, Debug)]
pub struct ServiceFacts<'a> {
    /// The Service is new: no Deploy has created it yet.
    pub new: bool,
    /// The Service has no source to run.
    pub empty_source: bool,
    pub watch: Watch,
    /// Its containers in the evidence; `None` when the evidence names none of it.
    pub containers: Option<&'a [Container]>,
}

/// A Service's status. It never guesses: without current evidence it says so.
#[must_use]
pub fn service_status(facts: &ServiceFacts<'_>) -> ServiceStatus {
    // A new Service a failed Deploy still started has containers: say what they do.
    if facts.new && facts.containers.is_none() || facts.empty_source {
        return ServiceStatus::NotDeployed;
    }
    match facts.watch {
        Watch::NoServers => ServiceStatus::NeedsAServer,
        Watch::Unreachable => ServiceStatus::CantReachServers,
        Watch::Observed { whole } => evidence(facts, whole),
    }
}

fn evidence(facts: &ServiceFacts<'_>, whole: bool) -> ServiceStatus {
    let Some(containers) = facts.containers else {
        // A Server missing from the evidence may run it.
        return if whole {
            ServiceStatus::NotRunning
        } else {
            ServiceStatus::CantReachServers
        };
    };
    let running = containers
        .iter()
        .filter(|container| matches!(container, Container::Serving | Container::Running))
        .count();
    if running == 0 {
        return if containers.contains(&Container::Down) {
            ServiceStatus::Crashed
        } else if containers.contains(&Container::Stopped) {
            ServiceStatus::Stopped
        } else {
            ServiceStatus::NotRunning
        };
    }
    if containers.contains(&Container::Serving) {
        ServiceStatus::Online
    } else {
        ServiceStatus::Degraded
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
