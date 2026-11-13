//! What a person types for the operator commands, parsed into the Selectors, bounds and
//! ports the engine in `operator.rs` works with.

use std::str::FromStr;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use ployz_core::{ContainerSelector, ServiceSelector};
use thiserror::Error;

/// The forms `--since` and `--until` accept, as the help names them.
pub const TIME_FORMATS: &str =
    "10m, 2h, 7d, 2026-01-31, 2026-01-31T10:30:00, an RFC 3339 timestamp or Unix seconds";

/// A value the person typed was not one the command takes.
#[derive(Debug, Error)]
pub enum InputError {
    #[error(transparent)]
    Value(#[from] ployz_core::ValueError),
    #[error("cannot attach a terminal exec to non-terminal stdin; use -T")]
    TtyRequiresStdin,
    #[error("invalid Service log selector {0:?}")]
    InvalidServiceSelector(String),
    #[error("invalid log tail {0:?}: expected a non-negative integer or all")]
    InvalidTail(String),
    #[error(
        "invalid time {value:?} for --{flag}: expected a relative duration, RFC 3339 date, or Unix timestamp"
    )]
    InvalidTime { flag: &'static str, value: String },
    #[error("invalid proxy port: expected [LOCAL_PORT:]REMOTE_PORT")]
    InvalidProxyPort,
    #[error("invalid local port {0:?}: expected 0-65535")]
    InvalidLocalPort(String),
    #[error("invalid remote port {0:?}: expected 1-65535")]
    InvalidRemotePort(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceArg {
    pub service: ServiceSelector,
    pub containers: Vec<ContainerSelector>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProxyPorts {
    pub local: u16,
    pub remote: u16,
}

/// Detached exec, or attached exec with an optional TTY.
///
/// Detach cannot request a TTY; resolve CLI `--detach`/`-T` through
/// [`ExecMode::resolve`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecMode {
    Detached,
    Attached { tty: bool },
}

impl ExecMode {
    /// Collapse CLI `--detach`/`-T` and terminal probes into a mode that cannot
    /// request a TTY while detached.
    ///
    /// `-T` is ignored when `detach` is set. Attached TTY still requires
    /// terminal stdin.
    ///
    /// # Errors
    ///
    /// Returns [`InputError::TtyRequiresStdin`] when an attached TTY is
    /// requested without terminal stdin.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "CLI detach/-T plus stdin/stdout terminal probes collapse into ExecMode"
    )]
    pub fn resolve(
        detach: bool,
        no_tty: bool,
        stdout_terminal: bool,
        stdin_terminal: bool,
    ) -> Result<Self, InputError> {
        if detach {
            return Ok(Self::Detached);
        }
        // TODO: preserve the Compose-style stdout-driven TTY rule.
        let tty = stdout_terminal && !no_tty;
        if tty && !stdin_terminal {
            return Err(InputError::TtyRequiresStdin);
        }
        Ok(Self::Attached { tty })
    }
}

pub fn parse_service_args(values: &[String]) -> Result<Vec<ServiceArg>, InputError> {
    let mut parsed: Vec<ServiceArg> = Vec::new();
    for value in values {
        let value = value.trim();
        let (service, container) = value
            .split_once(':')
            .map_or((value, None), |(service, container)| {
                (service, Some(container))
            });
        if service.is_empty() || container.is_some_and(str::is_empty) {
            return Err(InputError::InvalidServiceSelector(value.to_owned()));
        }
        let service = ServiceSelector::parse(service)?;
        let container = container.map(ContainerSelector::parse).transpose()?;
        if let Some(existing) = parsed.iter_mut().find(|arg| arg.service == service) {
            match container {
                None => existing.containers.clear(),
                Some(container) if !existing.containers.is_empty() => {
                    if !existing.containers.iter().any(|value| value == &container) {
                        existing.containers.push(container);
                    }
                }
                Some(_) => {}
            }
        } else {
            parsed.push(ServiceArg {
                service,
                containers: container.into_iter().collect(),
            });
        }
    }
    Ok(parsed)
}

pub fn parse_tail(value: &str) -> Result<i32, InputError> {
    if value == "all" {
        return Ok(-1);
    }
    value
        .parse::<i32>()
        .ok()
        .filter(|tail| *tail >= 0)
        .ok_or_else(|| InputError::InvalidTail(value.to_owned()))
}

/// Parse a `--since` / `--until` value into absolute Unix seconds.
///
/// Empty means no bound. A non-empty value must be a relative duration, RFC 3339
/// date/time, local date or date-time, or Unix timestamp.
///
/// # Errors
///
/// Returns [`InputError::InvalidTime`], naming `flag`, when the value is not one
/// of those forms.
pub fn parse_log_time(
    flag: &'static str,
    value: &str,
    now_unix_seconds: i64,
) -> Result<Option<i64>, InputError> {
    if value.is_empty() {
        return Ok(None);
    }
    let timestamp = value
        .parse::<i64>()
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|time| time.timestamp())
        })
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .and_then(|time| Local.from_local_datetime(&time).single())
                .map(|time| time.timestamp())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .and_then(|time| Local.from_local_datetime(&time).single())
                .map(|time| time.timestamp())
        })
        .or_else(|| {
            go_duration(value)
                .and_then(|duration| i64::try_from(duration.as_secs()).ok())
                .map(|seconds| now_unix_seconds - seconds)
        });
    timestamp.map(Some).ok_or_else(|| InputError::InvalidTime {
        flag,
        value: value.to_owned(),
    })
}

/// Go duration syntax plus days, such as `1m30s`, `500ms` or `30d`.
pub(crate) fn go_duration(value: &str) -> Option<std::time::Duration> {
    let mut remaining = value;
    let mut seconds = 0_f64;
    while !remaining.is_empty() {
        let number_end =
            remaining.find(|character: char| !character.is_ascii_digit() && character != '.')?;
        let number = f64::from_str(&remaining[..number_end]).ok()?;
        remaining = &remaining[number_end..];
        let unit_end = remaining
            .find(|character: char| character.is_ascii_digit() || character == '.')
            .unwrap_or(remaining.len());
        let (unit, rest) = remaining.split_at(unit_end);
        let multiplier = match unit {
            "d" => 86_400_f64,
            "h" => 3_600_f64,
            "m" => 60_f64,
            "s" => 1_f64,
            "ms" => 0.001_f64,
            "us" | "µs" => 0.000_001_f64,
            "ns" => 0.000_000_001_f64,
            _ => return None,
        };
        seconds += number * multiplier;
        remaining = rest;
    }
    std::time::Duration::try_from_secs_f64(seconds).ok()
}

pub fn parse_proxy_ports(value: &str) -> Result<ProxyPorts, InputError> {
    let (local, remote) = value.split_once(':').map_or(("0", value), |parts| parts);
    if remote.contains(':') {
        return Err(InputError::InvalidProxyPort);
    }
    let local = local
        .parse::<u16>()
        .map_err(|_| InputError::InvalidLocalPort(local.to_owned()))?;
    let remote = remote
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| InputError::InvalidRemotePort(remote.to_owned()))?;
    Ok(ProxyPorts { local, remote })
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
