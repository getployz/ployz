//! NATS Service API helpers.

use std::fmt;

pub const SERVICE_API_PING_SUBJECT: &str = "$SRV.PING";
pub const SERVICE_API_INFO_SUBJECT: &str = "$SRV.INFO";
pub const SERVICE_API_STATS_SUBJECT: &str = "$SRV.STATS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServiceSpec {
    pub id: String,
    pub name: &'static str,
    pub version: ServiceVersion,
    pub description: &'static str,
    pub metadata: ServiceMetadata,
    pub endpoints: Vec<NatsServiceEndpointSpec>,
}

impl NatsServiceSpec {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: &'static str,
        version: ServiceVersion,
        description: &'static str,
        metadata: ServiceMetadata,
        endpoints: Vec<NatsServiceEndpointSpec>,
    ) -> Self {
        Self {
            id: id.into(),
            name,
            version,
            description,
            metadata,
            endpoints,
        }
    }

    #[must_use]
    pub fn has_endpoint_subject(&self, subject: &str) -> bool {
        self.endpoints
            .iter()
            .any(|endpoint| endpoint.subject == subject)
    }

    #[must_use]
    pub fn ping(&self) -> ServicePing {
        ServicePing {
            id: self.id.clone(),
            name: self.name,
            version: self.version,
            metadata: self.metadata.clone(),
            endpoint_count: self.endpoints.len(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceMetadata {
    entries: Vec<ServiceMetadataEntry>,
}

impl ServiceMetadata {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_entries(entries: Vec<ServiceMetadataEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.as_str())
    }

    #[must_use]
    pub fn entries(&self) -> &[ServiceMetadataEntry] {
        &self.entries
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceMetadataEntry {
    pub key: &'static str,
    pub value: String,
}

impl ServiceMetadataEntry {
    #[must_use]
    pub fn new(key: &'static str, value: impl Into<String>) -> Self {
        Self {
            key,
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServiceEndpointSpec {
    pub name: &'static str,
    pub subject: String,
    pub execution: EndpointExecution,
}

impl NatsServiceEndpointSpec {
    #[must_use]
    pub fn new(
        name: &'static str,
        subject: impl Into<String>,
        execution: EndpointExecution,
    ) -> Self {
        Self {
            name,
            subject: subject.into(),
            execution,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointExecution {
    Query,
    AcceptsOperation,
    NodeRpc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl ServiceVersion {
    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for ServiceVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePing {
    pub id: String,
    pub name: &'static str,
    pub version: ServiceVersion,
    pub metadata: ServiceMetadata,
    pub endpoint_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceDiscoveryQuery<'a> {
    All,
    Service { name: &'a str },
}

impl ServiceDiscoveryQuery<'_> {
    #[must_use]
    pub fn subject(&self) -> String {
        match self {
            Self::All => SERVICE_API_PING_SUBJECT.to_owned(),
            Self::Service { name } => format!("{SERVICE_API_PING_SUBJECT}.{name}"),
        }
    }
}

#[must_use]
pub fn discover_services(
    services: &[NatsServiceSpec],
    query: ServiceDiscoveryQuery<'_>,
) -> Vec<ServicePing> {
    services
        .iter()
        .filter(|service| match query {
            ServiceDiscoveryQuery::All => true,
            ServiceDiscoveryQuery::Service { name } => service.name == name,
        })
        .map(NatsServiceSpec::ping)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsRequestFailure {
    NoResponders { subject: String },
    Timeout { subject: String },
}
