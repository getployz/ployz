//! What the daemon decides about the containers on the ployz bridge, from the facts Docker
//! reports about them.

use ployz_core::RestartPolicy;

/// What to do with a member Docker refused as being between restarts.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Settle {
    /// Docker would run it again, so Ployz starts it again once it has moved.
    StopAndNote,
    /// It is exiting for good, so it stays stopped, as Docker would leave it.
    Stop,
    /// Whether Docker runs it again can't be told yet, or it has exited already:
    /// the refused operation is only retried.
    Leave,
}

/// Docker restarts a container it is restarting, and one exiting under `always`
/// or `unless-stopped`. One exiting under `on-failure` restarts only on a failing
/// exit code, which Docker records once it has exited, so Ployz doesn't stop it.
/// `policy` is `None` when Docker reports none.
#[must_use]
pub const fn settle(running: bool, restarting: bool, policy: Option<RestartPolicy>) -> Settle {
    if restarting {
        return Settle::StopAndNote;
    }
    if !running {
        return Settle::Leave;
    }
    match policy {
        Some(RestartPolicy::Always | RestartPolicy::UnlessStopped) => Settle::StopAndNote,
        Some(RestartPolicy::OnFailure { .. }) => Settle::Leave,
        None | Some(RestartPolicy::No) => Settle::Stop,
    }
}

/// Detached, or still on a dynamic address.
#[must_use]
pub const fn needs_pin(attached: bool, pinned: bool) -> bool {
    !attached || !pinned
}

/// Docker still counts a container between restarts as running, but it holds no
/// sandbox, so Docker refuses to detach it from the `network` or attach it. `message` is
/// the refusal's text.
#[must_use]
pub fn between_restarts(message: &str, network: &str) -> bool {
    message.ends_with(&format!("is not connected to network {network}"))
        || (message.starts_with("network sandbox for container ")
            && message.ends_with(" not found"))
}

/// Docker answers 404 for a container whose network it can't find too, and that
/// container is still there to start once its network is back.
#[must_use]
pub fn gone(status_code: u16, message: &str) -> bool {
    status_code == 404 && message.starts_with("No such container")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON_FAILURE: RestartPolicy = RestartPolicy::OnFailure {
        maximum_retry_count: Some(3),
    };

    #[test]
    fn a_member_between_restarts_is_stopped_and_noted_only_when_docker_would_run_it_again() {
        for policy in [
            None,
            Some(RestartPolicy::No),
            Some(RestartPolicy::Always),
            Some(RestartPolicy::UnlessStopped),
            Some(ON_FAILURE),
        ] {
            assert_eq!(settle(true, true, policy), Settle::StopAndNote);
            assert_eq!(settle(false, true, policy), Settle::StopAndNote);
        }
        assert_eq!(
            settle(true, false, Some(RestartPolicy::Always)),
            Settle::StopAndNote
        );
        assert_eq!(
            settle(true, false, Some(RestartPolicy::UnlessStopped)),
            Settle::StopAndNote
        );
        assert_eq!(settle(true, false, Some(RestartPolicy::No)), Settle::Stop);
        assert_eq!(settle(true, false, None), Settle::Stop);
        assert_eq!(settle(true, false, Some(ON_FAILURE)), Settle::Leave);
        for policy in [None, Some(RestartPolicy::Always), Some(ON_FAILURE)] {
            assert_eq!(settle(false, false, policy), Settle::Leave);
        }
    }

    #[test]
    fn a_member_needs_a_pin_when_it_is_detached_or_on_a_dynamic_address() {
        assert!(needs_pin(false, false));
        assert!(needs_pin(false, true));
        assert!(needs_pin(true, false));
        assert!(!needs_pin(true, true));
    }

    #[test]
    fn docker_refuses_a_container_between_restarts_both_ways() {
        assert!(between_restarts(
            "container 8a1b is not connected to network ployz",
            "ployz"
        ));
        assert!(between_restarts(
            "network sandbox for container 8a1b not found",
            "ployz"
        ));
        assert!(!between_restarts(
            "container 8a1b is not connected to network other",
            "ployz"
        ));
        assert!(!between_restarts("No such container: gone", "ployz"));
        assert!(!between_restarts("Address already in use", "ployz"));
    }

    #[test]
    fn only_a_missing_container_reads_as_gone_not_a_missing_network() {
        assert!(gone(404, "No such container: gone"));
        assert!(!gone(
            404,
            "failed to set up container networking: network 6d85cc7fe1c5 not found"
        ));
        assert!(!gone(500, "No such container: gone"));
    }
}
