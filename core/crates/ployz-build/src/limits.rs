//! The numbers both ends of a remote Build must agree on. The client waits on the
//! Machine's clock, so the Machine's bounds and the client's deadlines come from
//! one place and cannot drift apart.

use std::time::Duration;

/// How long each side waits at the edges of a Build.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Budget for termination and cleanup after a Build is cancelled or times out.
    /// A client that asked for cancellation waits this long for the Machine's evidence.
    pub termination_grace: Duration,
    /// Longest queue or active timeout a Machine's Build policy may set.
    pub max_policy_timeout: Duration,
    /// How long a Machine may take to answer, or to say it has queued the Build.
    pub admission_wait: Duration,
}

impl Timing {
    /// How long a client waits for admission once the Machine has said it is queued:
    /// the longest queue timeout plus the usual answer time.
    #[must_use]
    pub const fn queued_admission_wait(&self) -> Duration {
        self.max_policy_timeout.saturating_add(self.admission_wait)
    }

    /// How long a client waits for the result of a Build admitted with `active_timeout`.
    #[must_use]
    pub const fn result_wait(&self, active_timeout: Duration) -> Duration {
        active_timeout.saturating_add(self.termination_grace)
    }
}

/// The shape bounds of one Build request.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub targets: usize,
    pub image_contexts: usize,
}

pub const TIMING: Timing = Timing {
    termination_grace: Duration::from_secs(70),
    max_policy_timeout: Duration::from_secs(86400),
    admission_wait: Duration::from_secs(10),
};

pub const LIMITS: Limits = Limits {
    targets: 128,
    image_contexts: 128,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_waits_pin_the_values_both_ends_agreed_on() {
        assert_eq!(TIMING.termination_grace, Duration::from_secs(70));
        assert_eq!(TIMING.max_policy_timeout, Duration::from_secs(86400));
        assert_eq!(TIMING.queued_admission_wait(), Duration::from_secs(86410));
        assert_eq!(
            TIMING.result_wait(Duration::from_secs(1800)),
            Duration::from_secs(1870)
        );
    }

    #[test]
    fn a_queued_client_outwaits_the_longest_queue_timeout() {
        assert!(TIMING.queued_admission_wait() > TIMING.max_policy_timeout);
        assert!(TIMING.queued_admission_wait() > TIMING.termination_grace);
    }
}
