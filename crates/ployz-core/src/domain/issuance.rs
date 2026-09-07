//! Whether to contact the certificate authority for one Ingress Hostname.

use std::{
    net::IpAddr,
    time::{Duration, SystemTime},
};

use super::ClusterDnsVerdict;
use crate::IngressHost;

/// Which failure earned the shared backoff clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IssuanceFailure {
    DoesNotResolve,
    ResolvesElsewhere,
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

/// Whether DNS and the shared clock allow contacting the certificate authority.
///
/// Distinct from the daemon's rank / due-time `IssuanceAction` (`Order` / `Renew`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IssuanceGate {
    Nothing,
    Refuse(IssuanceClock),
    Order,
}

/// Decide whether to wait, refuse, or proceed. A resolve-verdict change drops resolve backoff.
///
/// Call this before every certificate-authority contact, including renewal.
#[must_use]
pub fn issuance_gate(
    clock: Option<IssuanceClock>,
    verdict: ClusterDnsVerdict,
    now: SystemTime,
    backoff_base: Duration,
    backoff_cap: Duration,
) -> IssuanceGate {
    let waiting = clock.is_some_and(|clock| clock.next_attempt_at() > now);
    let last_resolve = clock.and_then(|clock| match clock.last_failure() {
        IssuanceFailure::DoesNotResolve => Some(ClusterDnsVerdict::DoesNotResolve),
        IssuanceFailure::ResolvesElsewhere => Some(ClusterDnsVerdict::ResolvesElsewhere),
        IssuanceFailure::Authority => None,
    });
    let resolve_cleared = last_resolve.is_some_and(|last| last != verdict);
    if waiting && !resolve_cleared {
        return IssuanceGate::Nothing;
    }
    let last_failure = match verdict {
        ClusterDnsVerdict::PointsAtCluster => return IssuanceGate::Order,
        ClusterDnsVerdict::DoesNotResolve => IssuanceFailure::DoesNotResolve,
        ClusterDnsVerdict::ResolvesElsewhere => IssuanceFailure::ResolvesElsewhere,
    };
    IssuanceGate::Refuse(issuance_failure_clock(
        clock,
        last_failure,
        now,
        backoff_base,
        backoff_cap,
    ))
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

/// Why a hostname that misses this Cluster has no certificate.
#[must_use]
pub fn issuance_refusal_reason(
    hostname: &IngressHost,
    resolved: &[IpAddr],
    cluster_addresses: &[IpAddr],
) -> String {
    if resolved.is_empty() {
        format!(
            "Ingress Hostname {hostname} does not resolve; it should resolve to {}.",
            join_addresses(cluster_addresses)
        )
    } else {
        format!(
            "Ingress Hostname {hostname} resolves to {}; it should resolve to {}.",
            join_addresses(resolved),
            join_addresses(cluster_addresses)
        )
    }
}

fn join_addresses(addresses: &[IpAddr]) -> String {
    if addresses.is_empty() {
        return "this Cluster's Machine addresses (none are published)".into();
    }
    addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::{
        net::IpAddr,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::{
        IssuanceClock, IssuanceFailure, IssuanceGate, issuance_backoff, issuance_failure_clock,
        issuance_gate, issuance_refusal_reason,
    };
    use crate::{ClusterDnsVerdict, DEFAULT_BACKOFF_BASE, DEFAULT_BACKOFF_CAP, IngressHost};

    #[test]
    fn empty_row_orders_when_dns_points_at_the_cluster() {
        assert_eq!(
            decide(None, ClusterDnsVerdict::PointsAtCluster, now()),
            IssuanceGate::Order
        );
    }

    #[test]
    fn empty_row_refuses_when_dns_misses_the_cluster() {
        assert_eq!(
            decide(None, ClusterDnsVerdict::DoesNotResolve, now()),
            refuse(IssuanceFailure::DoesNotResolve, 1)
        );
        assert_eq!(
            decide(None, ClusterDnsVerdict::ResolvesElsewhere, now()),
            refuse(IssuanceFailure::ResolvesElsewhere, 1)
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
            decide(clock, ClusterDnsVerdict::PointsAtCluster, now()),
            IssuanceGate::Nothing
        );
        assert_eq!(
            decide(clock, ClusterDnsVerdict::DoesNotResolve, now()),
            IssuanceGate::Nothing
        );
    }

    #[test]
    fn unchanged_resolve_backoff_is_served_out() {
        assert_eq!(
            decide(
                Some(clock(
                    IssuanceFailure::DoesNotResolve,
                    1,
                    now() + Duration::from_secs(3600),
                )),
                ClusterDnsVerdict::DoesNotResolve,
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
                Some(clock(IssuanceFailure::DoesNotResolve, 1, later)),
                ClusterDnsVerdict::PointsAtCluster,
                now(),
            ),
            IssuanceGate::Order
        );
        assert_eq!(
            decide(
                Some(clock(IssuanceFailure::ResolvesElsewhere, 1, later)),
                ClusterDnsVerdict::PointsAtCluster,
                now(),
            ),
            IssuanceGate::Order
        );
    }

    #[test]
    fn resolve_verdict_change_refuses_without_waiting() {
        assert_eq!(
            decide(
                Some(clock(
                    IssuanceFailure::DoesNotResolve,
                    1,
                    now() + Duration::from_secs(6 * 60 * 60),
                )),
                ClusterDnsVerdict::ResolvesElsewhere,
                now(),
            ),
            refuse(IssuanceFailure::ResolvesElsewhere, 1)
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
                ClusterDnsVerdict::PointsAtCluster,
                now(),
            ),
            IssuanceGate::Order
        );
        assert_eq!(
            decide(
                Some(clock(
                    IssuanceFailure::DoesNotResolve,
                    4,
                    now() - Duration::from_secs(1),
                )),
                ClusterDnsVerdict::DoesNotResolve,
                now(),
            ),
            refuse(IssuanceFailure::DoesNotResolve, 5)
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
        let resolve = clock(IssuanceFailure::DoesNotResolve, 4, now());
        let elsewhere = IssuanceFailure::ResolvesElsewhere;
        assert_eq!(
            next_clock(None, IssuanceFailure::DoesNotResolve, now()),
            clock(IssuanceFailure::DoesNotResolve, 1, now() + delay(1))
        );
        assert_eq!(
            next_clock(Some(resolve), IssuanceFailure::DoesNotResolve, now()),
            clock(IssuanceFailure::DoesNotResolve, 5, now() + delay(5))
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

    #[test]
    fn refusal_reason_names_the_hostname_and_addresses() {
        let hostname = IngressHost::parse("app.example.com").unwrap();
        let cluster = addrs(["192.0.2.1", "192.0.2.2"]);
        let elsewhere = addrs(["198.51.100.10"]);

        assert_eq!(
            issuance_refusal_reason(&hostname, &[], &cluster),
            "Ingress Hostname app.example.com does not resolve; it should resolve to 192.0.2.1, 192.0.2.2."
        );
        assert_eq!(
            issuance_refusal_reason(&hostname, &elsewhere, &cluster),
            "Ingress Hostname app.example.com resolves to 198.51.100.10; it should resolve to 192.0.2.1, 192.0.2.2."
        );
        assert_eq!(
            issuance_refusal_reason(&hostname, &elsewhere, &[]),
            "Ingress Hostname app.example.com resolves to 198.51.100.10; it should resolve to this Cluster's Machine addresses (none are published)."
        );
    }

    fn decide(
        clock: Option<IssuanceClock>,
        verdict: ClusterDnsVerdict,
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

    fn refuse(last_failure: IssuanceFailure, failures: u32) -> IssuanceGate {
        IssuanceGate::Refuse(clock(last_failure, failures, now() + delay(failures)))
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

    fn addrs<const N: usize>(values: [&str; N]) -> Vec<IpAddr> {
        values
            .into_iter()
            .map(|value| value.parse().unwrap())
            .collect()
    }
}
