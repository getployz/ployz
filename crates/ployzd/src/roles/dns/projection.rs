//! DNS projection read-model.

use ployz_core::ops::RouteHostname;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProjectionInput {
    pub records: Vec<DnsRecordSet>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsRecordSet {
    pub hostname: RouteHostname,
    pub answers: Vec<DnsAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DnsAnswer {
    Ipv4(Ipv4Addr),
    Ipv6(Ipv6Addr),
}

impl DnsAnswer {
    pub fn try_new(value: impl Into<String>) -> Result<Self, DnsAnswerError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DnsAnswerError::Empty);
        }

        match value.parse::<IpAddr>() {
            Ok(IpAddr::V4(address)) => Ok(Self::Ipv4(address)),
            Ok(IpAddr::V6(address)) => Ok(Self::Ipv6(address)),
            Err(_) => Err(DnsAnswerError::Invalid { value }),
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Ipv4(address) => address.to_string(),
            Self::Ipv6(address) => address.to_string(),
        }
    }

    #[must_use]
    pub const fn from_ip(value: IpAddr) -> Self {
        match value {
            IpAddr::V4(address) => Self::Ipv4(address),
            IpAddr::V6(address) => Self::Ipv6(address),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsAnswerError {
    Empty,
    Invalid { value: String },
}

impl fmt::Display for DnsAnswerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("DNS answer is empty"),
            Self::Invalid { value } => write!(formatter, "DNS answer is invalid: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProjection {
    pub records: Vec<DnsRecordSet>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsProjectionUpdate {
    SourceAvailable(DnsProjectionInput),
    SourceInvalid(DnsProjectionError),
    SourceUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProjectionState {
    pub last_good: Option<DnsProjection>,
    pub last_error: Option<DnsProjectionError>,
}

impl DnsProjectionState {
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            last_good: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsProjectionError {
    InvalidSource { message: String },
}

#[must_use]
pub fn apply_dns_update(
    previous: DnsProjectionState,
    update: DnsProjectionUpdate,
) -> DnsProjectionState {
    match update {
        DnsProjectionUpdate::SourceAvailable(input) => DnsProjectionState {
            last_good: Some(project_dns(input)),
            last_error: None,
        },
        DnsProjectionUpdate::SourceInvalid(error) => DnsProjectionState {
            last_error: Some(error),
            ..previous
        },
        DnsProjectionUpdate::SourceUnavailable => {
            if previous.last_good.is_some() {
                if previous.last_error.is_some() {
                    return previous;
                }
                DnsProjectionState {
                    last_error: Some(DnsProjectionError::InvalidSource {
                        message: "DNS source unavailable".to_owned(),
                    }),
                    ..previous
                }
            } else {
                previous
            }
        }
    }
}

#[must_use]
pub fn project_dns(input: DnsProjectionInput) -> DnsProjection {
    let mut records_by_hostname: BTreeMap<RouteHostname, BTreeSet<DnsAnswer>> = BTreeMap::new();
    for record in input.records {
        records_by_hostname
            .entry(record.hostname)
            .or_default()
            .extend(record.answers);
    }

    let records = records_by_hostname
        .into_iter()
        .map(|(hostname, answers)| DnsRecordSet {
            hostname,
            answers: answers.into_iter().collect(),
        })
        .collect();

    DnsProjection { records }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProjector {
    state: DnsProjectionState,
    answers: DnsAnswerTable,
}

impl DnsProjector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: DnsProjectionState::unavailable(),
            answers: DnsAnswerTable::empty(),
        }
    }

    #[must_use]
    pub const fn state(&self) -> &DnsProjectionState {
        &self.state
    }

    #[must_use]
    pub const fn answers(&self) -> &DnsAnswerTable {
        &self.answers
    }

    pub fn apply_source_update(&mut self, update: DnsProjectionUpdate) -> DnsProjectorTick {
        let previous = std::mem::replace(&mut self.state, DnsProjectionState::unavailable());
        self.state = apply_dns_update(previous, update);

        if let Some(projection) = projection_to_serve(&self.state) {
            self.answers.replace(projection.clone());
        }

        DnsProjectorTick {
            state: self.state.clone(),
            served: self.answers.current().cloned(),
            serving: dns_serving_state_from_projection_state(&self.state),
        }
    }
}

impl Default for DnsProjector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProjectorTick {
    pub state: DnsProjectionState,
    pub served: Option<DnsProjection>,
    pub serving: DnsServingState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsServingState {
    Current {
        record_count: usize,
    },
    LastKnownGood {
        record_count: usize,
        error: DnsProjectionError,
    },
    Unavailable {
        error: Option<DnsProjectionError>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsAnswerTable {
    current: Option<DnsProjection>,
}

impl DnsAnswerTable {
    #[must_use]
    pub const fn empty() -> Self {
        Self { current: None }
    }

    #[must_use]
    pub const fn current(&self) -> Option<&DnsProjection> {
        self.current.as_ref()
    }

    #[must_use]
    pub fn records(&self) -> &[DnsRecordSet] {
        self.current
            .as_ref()
            .map(|projection| projection.records.as_slice())
            .unwrap_or(&[])
    }

    #[must_use]
    pub fn answers_for(&self, hostname: &RouteHostname) -> &[DnsAnswer] {
        self.records()
            .iter()
            .find(|record| &record.hostname == hostname)
            .map(|record| record.answers.as_slice())
            .unwrap_or(&[])
    }

    fn replace(&mut self, projection: DnsProjection) {
        self.current = Some(projection);
    }
}

fn projection_to_serve(state: &DnsProjectionState) -> Option<&DnsProjection> {
    state.last_good.as_ref()
}

fn dns_serving_state_from_projection_state(state: &DnsProjectionState) -> DnsServingState {
    match (&state.last_good, &state.last_error) {
        (Some(projection), None) => DnsServingState::Current {
            record_count: projection.records.len(),
        },
        (Some(projection), Some(error)) => DnsServingState::LastKnownGood {
            record_count: projection.records.len(),
            error: error.clone(),
        },
        (None, Some(error)) => DnsServingState::Unavailable {
            error: Some(error.clone()),
        },
        (None, None) => DnsServingState::Unavailable { error: None },
    }
}
