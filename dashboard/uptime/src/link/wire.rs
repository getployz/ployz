//! The JSON shapes on the agent link. Only these derive serde; the domain types are parsed from
//! them at the boundary, so a malformed job or observation is dropped before it reaches a prober
//! or the engine.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::domain::{
    AgentName, CheckId, Detail, HttpCheckError, Job, NotProbed, Observation, Outcome, Phase,
    ProbeKey, ProbeKind, Region, Scheme, Target, Timings, UnixMs, http_error_str,
};

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct PullRequest {
    pub(crate) want: u32,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct PullResponse {
    pub(crate) jobs: Vec<WireJob>,
    pub(crate) server_now: i64,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct WireJob {
    pub(crate) check: String,
    pub(crate) region: String,
    pub(crate) scheduled_at: i64,
    pub(crate) kind: String,
    pub(crate) url: String,
    pub(crate) timeout_ms: u64,
    pub(crate) address: u32,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct ResultsRequest {
    pub(crate) batch_id: String,
    pub(crate) results: Vec<WireObservation>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct WireObservation {
    pub(crate) check: String,
    pub(crate) region: String,
    pub(crate) scheduled_at: i64,
    pub(crate) kind: String,
    pub(crate) started_at: i64,
    pub(crate) outcome: WireOutcome,
    pub(crate) timings: WireTimings,
    pub(crate) remote_ip: Option<String>,
    pub(crate) cert_not_after: Option<i64>,
    pub(crate) agent: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum WireOutcome {
    Http {
        status: u16,
    },
    Unreachable {
        phase: String,
        error: String,
        detail: String,
    },
    NotProbed {
        reason: String,
    },
}

impl Default for WireOutcome {
    fn default() -> Self {
        WireOutcome::NotProbed {
            reason: String::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct WireTimings {
    pub(crate) dns_ms: u32,
    pub(crate) connect_ms: u32,
    pub(crate) tls_ms: u32,
    pub(crate) ttfb_ms: u32,
    pub(crate) total_ms: u32,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct Malformed(&'static str);

impl WireJob {
    pub(crate) fn of(job: &Job) -> WireJob {
        WireJob {
            check: job.key.check.0.to_string(),
            region: job.key.region.as_str().to_owned(),
            scheduled_at: job.key.scheduled_at.0,
            kind: job.key.kind.as_str().to_owned(),
            url: url_of(&job.target),
            timeout_ms: u64::try_from(job.timeout.as_millis()).unwrap_or(u64::MAX),
            address: job.address,
        }
    }

    pub(crate) fn into_job(self) -> Result<Job, Malformed> {
        let key = key_of(&self.check, &self.region, self.scheduled_at, &self.kind)?;
        let target = Target::parse(&self.url).map_err(|_| Malformed("url"))?;
        Ok(Job {
            key,
            target: Arc::new(target),
            timeout: Duration::from_millis(self.timeout_ms),
            address: self.address,
        })
    }
}

impl WireObservation {
    pub(crate) fn of(observation: &Observation) -> WireObservation {
        let outcome = match &observation.outcome {
            Outcome::Http { status } => WireOutcome::Http { status: *status },
            Outcome::Unreachable {
                phase,
                error,
                detail,
            } => WireOutcome::Unreachable {
                phase: phase.as_str().to_owned(),
                error: http_error_str(*error).to_owned(),
                detail: detail.as_str().to_owned(),
            },
            Outcome::NotProbed(reason) => WireOutcome::NotProbed {
                reason: not_probed_str(*reason).to_owned(),
            },
        };
        let t = &observation.timings;
        WireObservation {
            check: observation.key.check.0.to_string(),
            region: observation.key.region.as_str().to_owned(),
            scheduled_at: observation.key.scheduled_at.0,
            kind: observation.key.kind.as_str().to_owned(),
            started_at: observation.started_at.0,
            outcome,
            timings: WireTimings {
                dns_ms: t.dns_ms,
                connect_ms: t.connect_ms,
                tls_ms: t.tls_ms,
                ttfb_ms: t.ttfb_ms,
                total_ms: t.total_ms,
            },
            remote_ip: observation.remote_ip.map(|ip| ip.to_string()),
            cert_not_after: observation.cert_not_after,
            agent: observation.agent.0.to_string(),
        }
    }

    pub(crate) fn into_observation(self) -> Result<Observation, Malformed> {
        let key = key_of(&self.check, &self.region, self.scheduled_at, &self.kind)?;
        let outcome = match self.outcome {
            WireOutcome::Http { status } => Outcome::Http { status },
            WireOutcome::Unreachable {
                phase,
                error,
                detail,
            } => Outcome::Unreachable {
                phase: phase_of(&phase)?,
                error: error_of(&error)?,
                detail: Detail::new(&detail),
            },
            WireOutcome::NotProbed { reason } => Outcome::NotProbed(not_probed_of(&reason)?),
        };
        let remote_ip = match self.remote_ip {
            None => None,
            Some(raw) => Some(raw.parse::<IpAddr>().map_err(|_| Malformed("remote_ip"))?),
        };
        if self.agent.is_empty() {
            return Err(Malformed("agent"));
        }
        let t = self.timings;
        Ok(Observation {
            key,
            started_at: UnixMs(self.started_at),
            outcome,
            timings: Timings {
                dns_ms: t.dns_ms,
                connect_ms: t.connect_ms,
                tls_ms: t.tls_ms,
                ttfb_ms: t.ttfb_ms,
                total_ms: t.total_ms,
            },
            remote_ip,
            cert_not_after: self.cert_not_after,
            agent: AgentName(self.agent.into()),
        })
    }
}

fn key_of(check: &str, region: &str, scheduled_at: i64, kind: &str) -> Result<ProbeKey, Malformed> {
    Ok(ProbeKey {
        check: CheckId(check.parse().map_err(|_| Malformed("check"))?),
        region: Region::parse(region).map_err(|_| Malformed("region"))?,
        scheduled_at: UnixMs(scheduled_at),
        kind: kind_of(kind)?,
    })
}

fn kind_of(kind: &str) -> Result<ProbeKind, Malformed> {
    match kind {
        "routine" => Ok(ProbeKind::Routine),
        "confirm" => Ok(ProbeKind::Confirm),
        "recheck" => Ok(ProbeKind::Recheck),
        _ => Err(Malformed("kind")),
    }
}

fn phase_of(phase: &str) -> Result<Phase, Malformed> {
    match phase {
        "dns" => Ok(Phase::Dns),
        "connect" => Ok(Phase::Connect),
        "tls" => Ok(Phase::Tls),
        "request" => Ok(Phase::Request),
        _ => Err(Malformed("phase")),
    }
}

fn error_of(error: &str) -> Result<HttpCheckError, Malformed> {
    match error {
        "connection_refused" => Ok(HttpCheckError::ConnectionRefused),
        "timed_out" => Ok(HttpCheckError::TimedOut),
        "other" => Ok(HttpCheckError::Other),
        _ => Err(Malformed("error")),
    }
}

fn not_probed_str(reason: NotProbed) -> &'static str {
    match reason {
        NotProbed::PrivateAddress => "private_address",
        NotProbed::Saturated => "saturated",
    }
}

fn not_probed_of(reason: &str) -> Result<NotProbed, Malformed> {
    match reason {
        "private_address" => Ok(NotProbed::PrivateAddress),
        "saturated" => Ok(NotProbed::Saturated),
        _ => Err(Malformed("reason")),
    }
}

fn url_of(target: &Target) -> String {
    let scheme = match target.scheme {
        Scheme::Http => "http",
        Scheme::Https => "https",
    };
    format!("{scheme}://{}{}", target.authority(), target.path_and_query)
}
