//! Whether to contact the certificate authority for one Ingress Hostname.

use std::time::{Duration, SystemTime};

use super::{ClusterRoute, HostnameVerdict, Refusal};

/// Which failure earned the shared backoff clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IssuanceFailure {
    Refused(Refusal),
    Authority,
}

/// Shared backoff clock after a refusal or an authority failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IssuanceClock {
    failures: u32,
    next_attempt_at: SystemTime,
    last_failure: IssuanceFailure,
}

impl IssuanceClock {
    /// `failures` is at least 1: a recorded clock means at least one attempt.
    #[must_use]
    pub fn new(failures: u32, next_attempt_at: SystemTime, last_failure: IssuanceFailure) -> Self {
        Self {
            failures: failures.max(1),
            next_attempt_at,
            last_failure,
        }
    }

    /// Recorded attempts under this failure. Always at least 1.
    #[must_use]
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// When the Cluster may try again.
    #[must_use]
    pub fn next_attempt_at(&self) -> SystemTime {
        self.next_attempt_at
    }

    /// Which failure earned this clock.
    #[must_use]
    pub fn last_failure(&self) -> IssuanceFailure {
        self.last_failure
    }
}

/// Whether the Hostname Verdict and the shared clock allow contacting the certificate authority.
///
/// Distinct from the daemon's rank / due-time `IssuanceAction` (`Order` / `Renew`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IssuanceGate {
    Nothing,
    Refuse {
        refusal: Refusal,
        clock: IssuanceClock,
    },
    Order(ClusterRoute),
}

/// Decide whether to wait, refuse, or proceed. A verdict change drops refusal backoff.
///
/// Call this before every certificate-authority contact, including renewal.
#[must_use]
pub fn issuance_gate(
    clock: Option<IssuanceClock>,
    verdict: HostnameVerdict,
    now: SystemTime,
    backoff_base: Duration,
    backoff_cap: Duration,
) -> IssuanceGate {
    let waiting = clock.is_some_and(|clock| clock.next_attempt_at() > now);
    let verdict_changed = clock.is_some_and(|clock| {
        matches!(clock.last_failure(), IssuanceFailure::Refused(last)
            if verdict != HostnameVerdict::Refused(last))
    });
    if waiting && !verdict_changed {
        return IssuanceGate::Nothing;
    }
    match verdict {
        HostnameVerdict::ReachesCluster(route) => IssuanceGate::Order(route),
        HostnameVerdict::Refused(refusal) => IssuanceGate::Refuse {
            refusal,
            clock: issuance_failure_clock(
                clock,
                IssuanceFailure::Refused(refusal),
                now,
                backoff_base,
                backoff_cap,
            ),
        },
    }
}

/// Delay after `failures` recorded attempts. `failures == 0` uses the base delay.
#[must_use]
pub fn issuance_backoff(failures: u32, base: Duration, cap: Duration) -> Duration {
    let shift = failures.saturating_sub(1).min(31);
    let seconds = base.as_secs().saturating_mul(1_u64 << shift);
    Duration::from_secs(seconds.min(cap.as_secs()))
}

/// Next shared clock after a refusal or an authority failure.
#[must_use]
pub fn issuance_failure_clock(
    clock: Option<IssuanceClock>,
    new_failure: IssuanceFailure,
    now: SystemTime,
    backoff_base: Duration,
    backoff_cap: Duration,
) -> IssuanceClock {
    let failures = match clock {
        Some(clock) if clock.last_failure() == new_failure => clock.failures().saturating_add(1),
        Some(_) | None => 1,
    };
    IssuanceClock::new(
        failures,
        now + issuance_backoff(failures, backoff_base, backoff_cap),
        new_failure,
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{
        IssuanceClock, IssuanceFailure, IssuanceGate, issuance_backoff, issuance_failure_clock,
        issuance_gate,
    };
    use crate::{
        ClusterRoute, DEFAULT_BACKOFF_BASE, DEFAULT_BACKOFF_CAP, HostnameVerdict, Refusal,
    };

    #[test]
    fn empty_row_orders_when_the_hostname_reaches_the_cluster() {
        for route in [ClusterRoute::Direct, ClusterRoute::ViaProxy] {
            assert_eq!(
                decide(None, HostnameVerdict::ReachesCluster(route), now()),
                IssuanceGate::Order(route)
            );
        }
    }

    #[test]
    fn empty_row_refuses_when_the_hostname_misses_the_cluster() {
        assert_eq!(
            decide(None, HostnameVerdict::Refused(Refusal::Unreachable), now()),
            refuse(Refusal::Unreachable, 1)
        );
        assert_eq!(
            decide(
                None,
                HostnameVerdict::Refused(Refusal::RedirectsToHttps),
                now()
            ),
            refuse(Refusal::RedirectsToHttps, 1)
        );
        assert_eq!(
            decide(
                None,
                HostnameVerdict::Refused(Refusal::DoesNotResolve),
                now()
            ),
            refuse(Refusal::DoesNotResolve, 1)
        );
        assert_eq!(
            decide(
                None,
                HostnameVerdict::Refused(Refusal::ReachesElsewhere),
                now()
            ),
            refuse(Refusal::ReachesElsewhere, 1)
        );
    }

    #[test]
    fn authority_backoff_is_served_out() {
        let clock = Some(clock(
            IssuanceFailure::Authority,
            1,
            now() + Duration::from_secs(3600),
        ));
        assert_eq!(
            decide(
                clock,
                HostnameVerdict::ReachesCluster(ClusterRoute::Direct),
                now()
            ),
            IssuanceGate::Nothing
        );
        assert_eq!(
            decide(
                clock,
                HostnameVerdict::Refused(Refusal::DoesNotResolve),
                now()
            ),
            IssuanceGate::Nothing
        );
    }

    #[test]
    fn unchanged_resolve_backoff_is_served_out() {
        assert_eq!(
            decide(
                Some(clock(
                    IssuanceFailure::Refused(Refusal::DoesNotResolve),
                    1,
                    now() + Duration::from_secs(3600),
                )),
                HostnameVerdict::Refused(Refusal::DoesNotResolve),
                now(),
            ),
            IssuanceGate::Nothing
        );
    }

    #[test]
    fn resolve_verdict_change_orders_without_waiting() {
        let later = now() + Duration::from_secs(6 * 60 * 60);
        assert_eq!(
            decide(
                Some(clock(
                    IssuanceFailure::Refused(Refusal::DoesNotResolve),
                    1,
                    later
                )),
                HostnameVerdict::ReachesCluster(ClusterRoute::Direct),
                now(),
            ),
            IssuanceGate::Order(ClusterRoute::Direct)
        );
        assert_eq!(
            decide(
                Some(clock(
                    IssuanceFailure::Refused(Refusal::ReachesElsewhere),
                    1,
                    later
                )),
                HostnameVerdict::ReachesCluster(ClusterRoute::Direct),
                now(),
            ),
            IssuanceGate::Order(ClusterRoute::Direct)
        );
    }

    #[test]
    fn resolve_verdict_change_refuses_without_waiting() {
        assert_eq!(
            decide(
                Some(clock(
                    IssuanceFailure::Refused(Refusal::DoesNotResolve),
                    1,
                    now() + Duration::from_secs(6 * 60 * 60),
                )),
                HostnameVerdict::Refused(Refusal::ReachesElsewhere),
                now(),
            ),
            refuse(Refusal::ReachesElsewhere, 1)
        );
    }

    #[test]
    fn expired_clock_retries() {
        assert_eq!(
            decide(
                Some(clock(
                    IssuanceFailure::Authority,
                    1,
                    now() - Duration::from_secs(1),
                )),
                HostnameVerdict::ReachesCluster(ClusterRoute::Direct),
                now(),
            ),
            IssuanceGate::Order(ClusterRoute::Direct)
        );
        assert_eq!(
            decide(
                Some(clock(
                    IssuanceFailure::Refused(Refusal::DoesNotResolve),
                    4,
                    now() - Duration::from_secs(1),
                )),
                HostnameVerdict::Refused(Refusal::DoesNotResolve),
                now(),
            ),
            refuse(Refusal::DoesNotResolve, 5)
        );
    }

    #[test]
    fn backoff_doubles_until_the_cap_and_never_stops() {
        assert_eq!(delay(0), DEFAULT_BACKOFF_BASE);
        assert_eq!(delay(1), Duration::from_secs(60));
        assert_eq!(delay(2), Duration::from_secs(120));
        assert_eq!(delay(3), Duration::from_secs(240));
        assert_eq!(delay(6), Duration::from_secs(1920));
        assert_eq!(delay(7), DEFAULT_BACKOFF_CAP);
        assert_eq!(delay(11), DEFAULT_BACKOFF_CAP);
        assert_eq!(delay(u32::MAX), DEFAULT_BACKOFF_CAP);
        assert_eq!(
            issuance_backoff(3, Duration::from_secs(10), Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn failure_clock_resets_resolve_and_keeps_authority() {
        let resolve = clock(IssuanceFailure::Refused(Refusal::DoesNotResolve), 4, now());
        let elsewhere = IssuanceFailure::Refused(Refusal::ReachesElsewhere);
        assert_eq!(
            next_clock(
                None,
                IssuanceFailure::Refused(Refusal::DoesNotResolve),
                now()
            ),
            clock(
                IssuanceFailure::Refused(Refusal::DoesNotResolve),
                1,
                now() + delay(1)
            )
        );
        assert_eq!(
            next_clock(
                Some(resolve),
                IssuanceFailure::Refused(Refusal::DoesNotResolve),
                now()
            ),
            clock(
                IssuanceFailure::Refused(Refusal::DoesNotResolve),
                5,
                now() + delay(5)
            )
        );
        assert_eq!(
            next_clock(Some(resolve), elsewhere, now()),
            clock(elsewhere, 1, now() + delay(1))
        );
        assert_eq!(
            next_clock(Some(resolve), IssuanceFailure::Authority, now()),
            clock(IssuanceFailure::Authority, 1, now() + delay(1))
        );
        assert_eq!(
            next_clock(
                Some(clock(IssuanceFailure::Authority, 3, now())),
                IssuanceFailure::Authority,
                now()
            ),
            clock(IssuanceFailure::Authority, 4, now() + delay(4))
        );
    }

    fn decide(
        clock: Option<IssuanceClock>,
        verdict: HostnameVerdict,
        now: SystemTime,
    ) -> IssuanceGate {
        issuance_gate(
            clock,
            verdict,
            now,
            DEFAULT_BACKOFF_BASE,
            DEFAULT_BACKOFF_CAP,
        )
    }

    fn next_clock(
        clock: Option<IssuanceClock>,
        new_failure: IssuanceFailure,
        now: SystemTime,
    ) -> IssuanceClock {
        issuance_failure_clock(
            clock,
            new_failure,
            now,
            DEFAULT_BACKOFF_BASE,
            DEFAULT_BACKOFF_CAP,
        )
    }

    fn delay(failures: u32) -> Duration {
        issuance_backoff(failures, DEFAULT_BACKOFF_BASE, DEFAULT_BACKOFF_CAP)
    }

    fn refuse(refusal: Refusal, failures: u32) -> IssuanceGate {
        IssuanceGate::Refuse {
            refusal,
            clock: clock(
                IssuanceFailure::Refused(refusal),
                failures,
                now() + delay(failures),
            ),
        }
    }

    fn clock(
        last_failure: IssuanceFailure,
        failures: u32,
        next_attempt_at: SystemTime,
    ) -> IssuanceClock {
        IssuanceClock::new(failures, next_attempt_at, last_failure)
    }

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }
}
