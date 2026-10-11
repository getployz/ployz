//! A Service's status in one word, from what its Servers report. The canvas word and
//! the agent's status tool read the same rule.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What runs now, in one word.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Online,
    /// It runs, but fewer replicas serve than it asks for, or none serves yet.
    Degraded,
    Crashed,
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
    /// Exited with code 0.
    Stopped,
    /// Exited with an error, restarting, or any other state.
    Down,
}

/// What the Runtime Watch says of the Servers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Watch {
    /// The Environment has no Server.
    NoServers,
    /// No Server is connected.
    NoConnection,
    Unreachable,
    Connecting,
    /// The connection dropped. Evidence from before then is not current.
    Unavailable,
    /// Current evidence. `whole`: no Server is missing from it.
    Observed {
        whole: bool,
    },
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
    /// How many replicas it asks for, when known.
    pub desired_replicas: Option<u32>,
    /// A Deploy is rolling its replicas.
    pub deploying: bool,
}

/// A Service's status. It never guesses: without current evidence it says so.
#[must_use]
pub fn service_status(facts: &ServiceFacts<'_>) -> ServiceStatus {
    // A new Service a failed Deploy still started has containers: say what they do.
    if facts.new && facts.containers.is_none() || facts.empty_source {
        return ServiceStatus::NotDeployed;
    }
    match facts.watch {
        Watch::NoServers | Watch::NoConnection => ServiceStatus::NeedsAServer,
        Watch::Unreachable | Watch::Connecting | Watch::Unavailable => {
            ServiceStatus::CantReachServers
        }
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
        } else {
            ServiceStatus::NotRunning
        };
    }
    let serving = containers
        .iter()
        .filter(|container| **container == Container::Serving)
        .count();
    let short = facts
        .desired_replicas
        .is_some_and(|desired| u32::try_from(serving).is_ok_and(|serving| serving < desired));
    // A replica missing from partial evidence may serve on the Server that didn't report.
    if serving == 0 || whole && !facts.deploying && short {
        ServiceStatus::Degraded
    } else {
        ServiceStatus::Online
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
