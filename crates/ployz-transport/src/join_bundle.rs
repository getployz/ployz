//! Redactable bootstrap material for joining a node to a NATS domain.

use std::fmt;

use ployz_core::ids::NodeId;

use crate::iroh_endpoint::IrohEndpoint;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinBundle {
    pub node_id: NodeId,
    pub domain: String,
    nats_credentials: NatsCredentials,
    pub core_endpoints: Vec<IrohEndpoint>,
}

impl JoinBundle {
    #[must_use]
    pub fn new(
        node_id: NodeId,
        domain: impl Into<String>,
        nats_credentials: NatsCredentials,
        core_endpoints: Vec<IrohEndpoint>,
    ) -> Self {
        Self {
            node_id,
            domain: domain.into(),
            nats_credentials,
            core_endpoints,
        }
    }

    #[must_use]
    pub fn redacted(&self) -> RedactedJoinBundle {
        RedactedJoinBundle {
            node_id: self.node_id.as_str().to_owned(),
            domain: self.domain.clone(),
            nats_credentials: "<redacted>".to_owned(),
            core_endpoint_count: self.core_endpoints.len(),
        }
    }

    #[must_use]
    pub fn nats_credentials_secret(&self) -> &str {
        self.nats_credentials.secret()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct NatsCredentials(String);

impl NatsCredentials {
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for NatsCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedJoinBundle {
    pub node_id: String,
    pub domain: String,
    pub nats_credentials: String,
    pub core_endpoint_count: usize,
}
