//! DNS projection runtime.

use ployz_core::ops::RouteHostname;

use crate::projection::ProjectionState;

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

pub type DnsProjectionState = ProjectionState<DnsProjection, DnsProjectionError>;

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
        DnsProjectionUpdate::SourceAvailable(input) => {
            DnsProjectionState::Current(project_dns(input))
        }
        DnsProjectionUpdate::SourceInvalid(error) => previous.source_failed(error),
        DnsProjectionUpdate::SourceUnavailable => previous.source_unavailable(),
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
