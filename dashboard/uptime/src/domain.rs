//! The checker's vocabulary: ids, regions, probe identity, what an agent saw and how the control
//! plane judged it.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

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
    pub fn now() -> UnixMs {
        match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            Ok(since) => UnixMs(i64::try_from(since.as_millis()).unwrap_or(i64::MAX)),
            Err(before) => {
                UnixMs(-i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX))
            }
        }
    }

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

    #[must_use]
    pub fn to_system_time(self) -> SystemTime {
        let by = Duration::from_millis(self.0.unsigned_abs());
        if self.0 >= 0 {
            SystemTime::UNIX_EPOCH + by
        } else {
            SystemTime::UNIX_EPOCH - by
        }
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

    /// Lets the end-to-end test run a 30 s schedule in one second. Absent from release builds.
    #[cfg(test)]
    pub(crate) fn unchecked(secs: u32) -> Interval {
        Interval(secs)
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

impl ProbeKind {
    /// The `uptime_result.kind` and evidence spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeKind::Routine => "routine",
            ProbeKind::Confirm => "confirm",
            ProbeKind::Recheck => "recheck",
        }
    }
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

/// A probe target, from `uptime_check.url`. The checker never derives a hostname; it only probes
/// the one the row names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    pub scheme: Scheme,
    pub host: Host,
    pub port: u16,
    pub path_and_query: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scheme {
    Http,
    Https,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Host {
    Name(String),
    /// Still vetted like a resolved answer before any dial.
    Ip(IpAddr),
}

impl Target {
    /// The rules CRUD enforces in TS, re-asserted where the checker reads `uptime_check.url`: http
    /// or https, port 80 or 443 (explicit or default), no userinfo, no fragment, at most 2048
    /// bytes. The host is lowercased, and an IP literal becomes [`Host::Ip`].
    ///
    /// # Errors
    /// The URL breaks one of those rules.
    pub fn parse(url: &str) -> Result<Target, BadTarget> {
        let bad = |why: &'static str| BadTarget { why };
        if url.len() > 2048 {
            return Err(bad("is longer than 2048 bytes"));
        }
        // http::Uri drops a fragment silently instead of refusing it.
        if url.contains('#') {
            return Err(bad("has a fragment"));
        }
        let uri: http::Uri = url.parse().map_err(|_| bad("is not a valid URL"))?;
        let scheme = match uri.scheme_str() {
            Some("http") => Scheme::Http,
            Some("https") => Scheme::Https,
            _ => return Err(bad("is not http or https")),
        };
        let authority = uri.authority().ok_or(bad("has no host"))?;
        if authority.as_str().contains('@') {
            return Err(bad("has userinfo"));
        }
        // port_u16 is None for a port that overflows u16 as well as for no port, so the text decides.
        let port_text = &authority.as_str()[authority.host().len()..];
        let port = match (authority.port_u16(), scheme) {
            (Some(port @ (80 | 443)), _) => port,
            (Some(_), _) => return Err(bad("uses a port other than 80 or 443")),
            (None, _) if port_text.len() > 1 => return Err(bad("has an invalid port")),
            (None, Scheme::Http) => 80,
            (None, Scheme::Https) => 443,
        };
        let host = authority.host();
        let literal = host.strip_prefix('[').and_then(|h| h.strip_suffix(']'));
        let host = match literal.unwrap_or(host).parse::<IpAddr>() {
            Ok(ip) => Host::Ip(ip),
            Err(_) if literal.is_some() => return Err(bad("has a malformed IPv6 literal")),
            Err(_) if host.is_empty() => return Err(bad("has no host")),
            Err(_) => Host::Name(host.to_ascii_lowercase()),
        };
        let path_and_query = uri
            .path_and_query()
            .map_or_else(|| "/".to_owned(), |p| p.as_str().to_owned());
        Ok(Target {
            scheme,
            host,
            port,
            path_and_query,
        })
    }

    /// `host` or `host:port`, the port only when it isn't the scheme's default.
    #[must_use]
    pub fn authority(&self) -> String {
        let host = match &self.host {
            Host::Name(name) => name.clone(),
            Host::Ip(IpAddr::V6(ip)) => format!("[{ip}]"),
            Host::Ip(IpAddr::V4(ip)) => ip.to_string(),
        };
        let default_port = match self.scheme {
            Scheme::Http => 80,
            Scheme::Https => 443,
        };
        if self.port == default_port {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("The URL {why}.")]
pub struct BadTarget {
    why: &'static str,
}

/// The pass rule. One today, and the verdict is computed in the control plane, so a new rule never
/// redeploys agents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expect {
    /// Any HTTP response below 400 passes, 3xx included.
    Below400,
}

impl Expect {
    /// # Errors
    /// `name` is not a rule this build knows.
    pub fn parse(name: &str) -> Result<Expect, BadExpect> {
        match name {
            "below_400" => Ok(Expect::Below400),
            _ => Err(BadExpect(name.to_owned())),
        }
    }

    #[must_use]
    pub fn judge(self, outcome: &Outcome) -> Verdict {
        match (self, outcome) {
            (Expect::Below400, Outcome::Http { status }) if *status < 400 => Verdict::Pass,
            (Expect::Below400, Outcome::Http { .. } | Outcome::Unreachable { .. }) => Verdict::Fail,
            (Expect::Below400, Outcome::NotProbed(_)) => Verdict::Inconclusive,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Expect rule {0:?} is unknown to this build.")]
pub struct BadExpect(String);

/// An enabled check as the control plane holds it. Disabled checks are not held at all.
#[derive(Clone, Debug)]
pub struct Check {
    pub id: CheckId,
    pub org: OrgId,
    pub revision: Revision,
    /// Snapshot for the alert payload only.
    pub name: Arc<str>,
    /// As stored, for the alert payload and incident evidence.
    pub url: Arc<str>,
    pub target: Arc<Target>,
    pub interval: Interval,
    /// Shorter than the interval.
    pub timeout: Duration,
    pub expect: Expect,
}

/// One unit of work for an agent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Job {
    pub key: ProbeKey,
    pub target: Arc<Target>,
    pub timeout: Duration,
    /// Which vetted address to dial, as an index mod the answer count. The control plane rotates
    /// it per slot, so a hostname behind several ingress Machines gets each of them probed.
    pub address: u32,
}

/// What an agent saw. Nothing here is interpreted: no verdict, no cause.
#[derive(Clone, Debug, Eq, PartialEq)]
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

impl Phase {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Dns => "dns",
            Phase::Connect => "connect",
            Phase::Tls => "tls",
            Phase::Request => "request",
        }
    }
}

/// The spelling core's serde gives [`HttpCheckError`], which the TS types read through ts-rs.
#[must_use]
pub fn http_error_str(error: HttpCheckError) -> &'static str {
    match error {
        HttpCheckError::ConnectionRefused => "connection_refused",
        HttpCheckError::TimedOut => "timed_out",
        HttpCheckError::Other => "other",
    }
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

impl Verdict {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Inconclusive => "inconclusive",
        }
    }
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

#[cfg(test)]
#[path = "domain_tests.rs"]
mod tests;
