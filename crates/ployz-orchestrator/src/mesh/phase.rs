use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Stopped,
    Starting,
    Provisioning,
    Bootstrapping,
    Running,
    Stopping,
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Provisioning => "provisioning",
            Self::Bootstrapping => "bootstrapping",
            Self::Running => "running",
            Self::Stopping => "stopping",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseEvent {
    UpRequested,
    NetworkReady,
    ComponentsStarted,
    ComponentFailed,
    SyncComplete,
    DetachRequested,
    DestroyRequested,
    TeardownComplete,
}

#[derive(Debug, Error)]
pub enum TransitionError {
    #[error("invalid transition: {from} + {event:?}")]
    Invalid { from: Phase, event: PhaseEvent },
    #[error("bootstrap timeout: {reason}")]
    BootstrapTimeout { reason: String },
}

pub fn transition(
    current: Phase,
    event: PhaseEvent,
) -> std::result::Result<Phase, TransitionError> {
    use Phase::*;
    use PhaseEvent::*;

    match (current, event) {
        (Stopped, UpRequested) => Ok(Starting),
        (Stopped, NetworkReady)
        | (Stopped, ComponentsStarted)
        | (Stopped, ComponentFailed)
        | (Stopped, SyncComplete)
        | (Stopped, DetachRequested)
        | (Stopped, TeardownComplete) => Err(TransitionError::Invalid {
            from: current,
            event,
        }),
        (Starting, NetworkReady) => Ok(Provisioning),
        (Starting, UpRequested)
        | (Starting, ComponentsStarted)
        | (Starting, SyncComplete)
        | (Starting, DetachRequested)
        | (Starting, TeardownComplete) => Err(TransitionError::Invalid {
            from: current,
            event,
        }),
        (Provisioning, ComponentsStarted) => Ok(Bootstrapping),
        (Starting | Provisioning | Bootstrapping, ComponentFailed) => Ok(Stopped),
        (Provisioning, UpRequested)
        | (Provisioning, NetworkReady)
        | (Provisioning, SyncComplete)
        | (Provisioning, DetachRequested)
        | (Provisioning, TeardownComplete) => Err(TransitionError::Invalid {
            from: current,
            event,
        }),
        (Bootstrapping, SyncComplete) => Ok(Running),
        (Bootstrapping, UpRequested)
        | (Bootstrapping, NetworkReady)
        | (Bootstrapping, ComponentsStarted)
        | (Bootstrapping, DetachRequested)
        | (Bootstrapping, TeardownComplete) => Err(TransitionError::Invalid {
            from: current,
            event,
        }),
        (Running, DetachRequested) => Ok(Stopped),
        (Running, UpRequested)
        | (Running, NetworkReady)
        | (Running, ComponentsStarted)
        | (Running, ComponentFailed)
        | (Running, SyncComplete)
        | (Running, TeardownComplete) => Err(TransitionError::Invalid {
            from: current,
            event,
        }),
        (Stopped | Starting | Running | Provisioning | Bootstrapping, DestroyRequested) => {
            Ok(Stopping)
        }
        (Stopping, TeardownComplete) => Ok(Stopped),
        (Stopping, UpRequested)
        | (Stopping, NetworkReady)
        | (Stopping, ComponentsStarted)
        | (Stopping, ComponentFailed)
        | (Stopping, SyncComplete)
        | (Stopping, DetachRequested)
        | (Stopping, DestroyRequested) => Err(TransitionError::Invalid {
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
        let p = transition(p, PhaseEvent::NetworkReady).expect("starting -> provisioning");
        assert_eq!(p, Phase::Provisioning);
        let p =
            transition(p, PhaseEvent::ComponentsStarted).expect("provisioning -> bootstrapping");
        assert_eq!(p, Phase::Bootstrapping);
        let p = transition(p, PhaseEvent::SyncComplete).expect("bootstrapping -> running");
        assert_eq!(p, Phase::Running);
    }

    #[test]
    fn component_failure_from_starting() {
        let p = transition(Phase::Stopped, PhaseEvent::UpRequested).expect("stopped -> starting");
        assert_eq!(
            transition(p, PhaseEvent::ComponentFailed).expect("starting -> stopped on fail"),
            Phase::Stopped
        );
    }

    #[test]
    fn component_failure_from_provisioning() {
        assert_eq!(
            transition(Phase::Provisioning, PhaseEvent::ComponentFailed)
                .expect("provisioning -> stopped"),
            Phase::Stopped
        );
    }

    #[test]
    fn component_failure_from_bootstrapping() {
        assert_eq!(
            transition(Phase::Bootstrapping, PhaseEvent::ComponentFailed)
                .expect("bootstrapping -> stopped"),
            Phase::Stopped
        );
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
    fn destroy_from_provisioning() {
        assert_eq!(
            transition(Phase::Provisioning, PhaseEvent::DestroyRequested)
                .expect("provisioning -> stopping"),
            Phase::Stopping
        );
    }

    #[test]
    fn destroy_from_bootstrapping() {
        assert_eq!(
            transition(Phase::Bootstrapping, PhaseEvent::DestroyRequested)
                .expect("bootstrapping -> stopping"),
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
