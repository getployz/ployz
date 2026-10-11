//! The checker's vocabulary: ids, regions, probe identity, what an agent saw and how the control
//! plane judged it.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

pub use ployz_core::HttpCheckError;
use ployz_core::check_output_line;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct CheckId(pub Uuid);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct OrgId(pub Uuid);

/// Minted by the control plane when the detector decides to open, so the close names the exact
/// episode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct IncidentId(pub Uuid);

/// `uptime_check.revision`. CRUD bumps it on any url, interval, timeout, expect or enabled change.
/// Every incident write carries the revision the detector judged at, and Postgres refuses a stale
/// one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Revision(pub i32);

/// Milliseconds since the Unix epoch, in the control plane's clock. Agents correct to it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct UnixMs(pub i64);

impl UnixMs {
    #[must_use]
    pub fn plus(self, by: Duration) -> UnixMs {
        UnixMs(
            self.0
                .saturating_add(i64::try_from(by.as_millis()).unwrap_or(i64::MAX)),
        )
    }

    /// Time since `earlier`, zero if `earlier` is later.
    #[must_use]
    pub fn since(self, earlier: UnixMs) -> Duration {
        Duration::from_millis(u64::try_from(self.0.saturating_sub(earlier.0)).unwrap_or(0))
    }
}

/// A Cloud probe region: a Railway region Cloud probes from. Never a Machine's location.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Region(Arc<str>);

impl Region {
    /// # Errors
    /// The name is empty or holds anything but `[a-z0-9-]`.
    pub fn parse(name: &str) -> Result<Region, BadRegion> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(BadRegion(name.to_owned()));
        }
        Ok(Region(name.into()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Region name {0:?} must be non-empty lowercase letters, digits and dashes.")]
pub struct BadRegion(String);

/// How many observations make a decision, in both directions. Derived from the configured region
/// count, never the live count: losing three of four regions must not let one vantage point open or
/// close an incident.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Quorum {
    /// Cloud: this many distinct regions agree and none disagree.
    DistinctRegions(u8),
    /// Self-host with one region: this many probes from it, `spacing` apart.
    OneRegion { probes: u8, spacing: Duration },
}

impl Quorum {
    #[must_use]
    pub fn for_configured(regions: usize) -> Quorum {
        if regions >= 2 {
            Quorum::DistinctRegions(2)
        } else {
            Quorum::OneRegion {
                probes: 2,
                spacing: Duration::from_secs(30),
            }
        }
    }
}

/// Probe interval, 30..=3600 seconds, the `uptime_check` CHECK re-asserted at the boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Interval(u32);

impl Interval {
    /// # Errors
    /// `secs` is outside 30..=3600.
    pub fn parse(secs: u32) -> Result<Interval, BadInterval> {
        if (30..=3600).contains(&secs) {
            Ok(Interval(secs))
        } else {
            Err(BadInterval(secs))
        }
    }

    #[must_use]
    pub fn as_duration(self) -> Duration {
        Duration::from_secs(u64::from(self.0))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Interval of {0}s is outside 30..=3600.")]
pub struct BadInterval(u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ProbeKind {
    /// The one probe per check per interval, from one region, rotating through the check's live
    /// regions.
    Routine,
    /// A confirmation probe after a routine failure.
    Confirm,
    /// A recovery probe while the check is unreachable.
    Recheck,
}

/// A probe's identity: the result table's primary key and the dedupe key. The control plane mints
/// it and the agent echoes it.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ProbeKey {
    pub check: CheckId,
    pub region: Region,
    pub scheduled_at: UnixMs,
    pub kind: ProbeKind,
}

/// What an agent saw. Nothing here is interpreted: no verdict, no cause.
#[derive(Clone, Debug)]
pub struct Observation {
    pub key: ProbeKey,
    /// When the probe actually started, in the control plane's clock.
    pub started_at: UnixMs,
    pub outcome: Outcome,
    pub timings: Timings,
    /// The pinned address dialled.
    pub remote_ip: Option<IpAddr>,
    /// From the leaf certificate when a TLS handshake completed, in Unix seconds.
    pub cert_not_after: Option<i64>,
    pub agent: AgentName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// An HTTP response head arrived.
    Http { status: u16 },
    /// No HTTP response, and the phase that failed.
    Unreachable {
        phase: Phase,
        error: HttpCheckError,
        detail: Detail,
    },
    /// The agent declined to probe. Never counts as a failure, never opens or closes anything.
    NotProbed(NotProbed),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Dns,
    Connect,
    Tls,
    Request,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotProbed {
    /// At least one resolved answer was not a global address. One bad answer refuses the whole
    /// target, since rebinding tricks mix good and bad answers.
    PrivateAddress,
    /// The agent is at its global or per-address in-flight cap.
    Saturated,
}

/// The first non-blank line of an error, cut to 120 characters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Detail(String);

impl Detail {
    #[must_use]
    pub fn new(text: &str) -> Detail {
        Detail(check_output_line(text))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Milliseconds per phase, 0 for a phase not reached.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Timings {
    pub dns_ms: u32,
    pub connect_ms: u32,
    pub tls_ms: u32,
    pub ttfb_ms: u32,
    pub total_ms: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    Pass,
    Fail,
    /// We could not or would not probe. Counts neither way.
    Inconclusive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentName(pub Arc<str>);

/// An observation matched to a check the control plane holds, and judged there. Org and verdict
/// never come from the agent.
#[derive(Clone, Debug)]
pub struct Judged {
    pub org: OrgId,
    pub verdict: Verdict,
    pub observation: Observation,
}
