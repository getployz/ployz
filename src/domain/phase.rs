use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Stopped,
    Starting,
    BootstrappingNetwork,
    BootstrappingSync,
    Running,
    Stopping,
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::BootstrappingNetwork => "bootstrapping:network",
            Self::BootstrappingSync => "bootstrapping:sync",
            Self::Running => "running",
            Self::Stopping => "stopping",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseEvent {
    UpRequested,
    ComponentsStarted,
    ComponentFailed,
    NetworkConnected,
    SyncComplete,
    BootstrapTimeout,
    DetachRequested,
    DestroyRequested,
    TeardownComplete,
}

#[derive(Debug, Error)]
pub enum TransitionError {
    #[error("invalid transition: {from} + {event:?}")]
    Invalid { from: Phase, event: PhaseEvent },
    #[error("bootstrap timeout")]
    BootstrapTimeout,
}

pub fn transition(
    current: Phase,
    event: PhaseEvent,
) -> std::result::Result<Phase, TransitionError> {
    use Phase::*;
    use PhaseEvent::*;

    match (current, event) {
        (Stopped, UpRequested) => Ok(Starting),
        (Starting, ComponentsStarted) => Ok(BootstrappingNetwork),
        (Starting | BootstrappingNetwork | BootstrappingSync, ComponentFailed) => Ok(Stopped),
        (BootstrappingNetwork, NetworkConnected) => Ok(BootstrappingSync),
        (BootstrappingSync, SyncComplete) => Ok(Running),
        (BootstrappingNetwork | BootstrappingSync, BootstrapTimeout) => {
            Err(TransitionError::BootstrapTimeout)
        }
        (Running, DetachRequested) => Ok(Stopped),
        (Stopped | Running | BootstrappingNetwork | BootstrappingSync, DestroyRequested) => {
            Ok(Stopping)
        }
        (Stopping, TeardownComplete) => Ok(Stopped),
        _ => Err(TransitionError::Invalid {
            from: current,
            event,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_startup() {
        let p = transition(Phase::Stopped, PhaseEvent::UpRequested).expect("stopped -> starting");
        assert_eq!(p, Phase::Starting);
        let p = transition(p, PhaseEvent::ComponentsStarted)
            .expect("starting -> bootstrapping:network");
        assert_eq!(p, Phase::BootstrappingNetwork);
        let p = transition(p, PhaseEvent::NetworkConnected)
            .expect("bootstrapping:network -> bootstrapping:sync");
        assert_eq!(p, Phase::BootstrappingSync);
        let p = transition(p, PhaseEvent::SyncComplete).expect("bootstrapping:sync -> running");
        assert_eq!(p, Phase::Running);
    }

    #[test]
    fn component_failure_returns_to_stopped() {
        let p = transition(Phase::Stopped, PhaseEvent::UpRequested).expect("stopped -> starting");
        assert_eq!(
            transition(p, PhaseEvent::ComponentFailed).expect("starting -> stopped on fail"),
            Phase::Stopped
        );
    }

    #[test]
    fn component_failure_from_bootstrap_network() {
        assert_eq!(
            transition(Phase::BootstrappingNetwork, PhaseEvent::ComponentFailed)
                .expect("bootstrapping:network -> stopped"),
            Phase::Stopped
        );
    }

    #[test]
    fn component_failure_from_bootstrap_sync() {
        assert_eq!(
            transition(Phase::BootstrappingSync, PhaseEvent::ComponentFailed)
                .expect("bootstrapping:sync -> stopped"),
            Phase::Stopped
        );
    }

    #[test]
    fn bootstrap_timeout_from_network_phase() {
        let err = transition(Phase::BootstrappingNetwork, PhaseEvent::BootstrapTimeout)
            .expect_err("bootstrap timeout error");
        assert!(matches!(err, TransitionError::BootstrapTimeout));
    }

    #[test]
    fn bootstrap_timeout_from_sync_phase() {
        let err = transition(Phase::BootstrappingSync, PhaseEvent::BootstrapTimeout)
            .expect_err("bootstrap timeout error");
        assert!(matches!(err, TransitionError::BootstrapTimeout));
    }

    #[test]
    fn detach_from_running() {
        assert_eq!(
            transition(Phase::Running, PhaseEvent::DetachRequested)
                .expect("running -> stopped on detach"),
            Phase::Stopped
        );
    }

    #[test]
    fn destroy_from_running() {
        let p =
            transition(Phase::Running, PhaseEvent::DestroyRequested).expect("running -> stopping");
        assert_eq!(p, Phase::Stopping);
        assert_eq!(
            transition(p, PhaseEvent::TeardownComplete).expect("stopping -> stopped"),
            Phase::Stopped
        );
    }

    #[test]
    fn destroy_from_bootstrap_phases() {
        assert_eq!(
            transition(Phase::BootstrappingNetwork, PhaseEvent::DestroyRequested)
                .expect("bootstrapping:network -> stopping"),
            Phase::Stopping
        );
        assert_eq!(
            transition(Phase::BootstrappingSync, PhaseEvent::DestroyRequested)
                .expect("bootstrapping:sync -> stopping"),
            Phase::Stopping
        );
    }

    #[test]
    fn invalid_transition_errors() {
        assert!(matches!(
            transition(Phase::Stopped, PhaseEvent::ComponentsStarted).expect_err("invalid"),
            TransitionError::Invalid { .. }
        ));
        assert!(matches!(
            transition(Phase::Running, PhaseEvent::UpRequested).expect_err("invalid"),
            TransitionError::Invalid { .. }
        ));
    }
}
