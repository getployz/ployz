//! Supervised iroh tunnel service preparation.

use std::net::SocketAddr;

use ployz_nats::connect::NatsClientEndpoint;
use ployz_transport::iroh_endpoint::IrohEndpoint;
use ployz_transport::nats_tunnel::{CoreNatsTunnelConfig, EdgeNatsTunnelConfig, NatsTunnelConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedTunnelService {
    pub service_name: String,
    pub tunnel: NatsTunnelConfig,
    pub restart_policy: TunnelRestartPolicy,
}

impl PreparedTunnelService {
    #[must_use]
    pub fn core(service_name: impl Into<String>, nats_socket: SocketAddr) -> Self {
        Self {
            service_name: service_name.into(),
            tunnel: NatsTunnelConfig::Core(CoreNatsTunnelConfig::new(nats_socket)),
            restart_policy: TunnelRestartPolicy::Always {
                initial_backoff_ms: 250,
                max_backoff_ms: 5_000,
            },
        }
    }

    #[must_use]
    pub fn edge(
        service_name: impl Into<String>,
        local_listen: SocketAddr,
        core_endpoint: IrohEndpoint,
    ) -> Self {
        Self {
            service_name: service_name.into(),
            tunnel: NatsTunnelConfig::Edge(EdgeNatsTunnelConfig::new(local_listen, core_endpoint)),
            restart_policy: TunnelRestartPolicy::Always {
                initial_backoff_ms: 250,
                max_backoff_ms: 5_000,
            },
        }
    }

    #[must_use]
    pub fn local_client_endpoint(&self) -> NatsClientEndpoint {
        NatsClientEndpoint::from_socket(self.tunnel.local_socket())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelRestartPolicy {
    Always {
        initial_backoff_ms: u64,
        max_backoff_ms: u64,
    },
}
