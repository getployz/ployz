use std::net::SocketAddr;

use crate::{
    HttpGatewayError, HttpGatewayHandle, HttpGatewayResult, WireRoleMetrics, WireServingState,
    spawn_http_gateway,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayEngineKind {
    Hyper,
}

impl Default for GatewayEngineKind {
    fn default() -> Self {
        Self::Hyper
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayOptions {
    pub listen_addr: SocketAddr,
    pub engine: GatewayEngineKind,
}

impl GatewayOptions {
    #[must_use]
    pub fn new(listen_addr: SocketAddr) -> Self {
        Self {
            listen_addr,
            engine: GatewayEngineKind::default(),
        }
    }

    #[must_use]
    pub fn with_engine(mut self, engine: GatewayEngineKind) -> Self {
        self.engine = engine;
        self
    }
}

pub struct GatewayHandle {
    engine: GatewayEngineKind,
    inner: GatewayEngineHandle,
}

enum GatewayEngineHandle {
    Hyper(HttpGatewayHandle),
}

impl GatewayHandle {
    #[must_use]
    pub fn engine(&self) -> GatewayEngineKind {
        self.engine
    }

    #[must_use]
    pub fn listen_addr(&self) -> SocketAddr {
        match &self.inner {
            GatewayEngineHandle::Hyper(handle) => handle.listen_addr(),
        }
    }

    #[must_use]
    pub fn metrics(&self) -> WireRoleMetrics {
        match &self.inner {
            GatewayEngineHandle::Hyper(handle) => handle.metrics(),
        }
    }

    pub async fn shutdown(self) -> Result<(), HttpGatewayError> {
        match self.inner {
            GatewayEngineHandle::Hyper(handle) => handle.shutdown().await,
        }
    }
}

pub async fn spawn_gateway(
    options: GatewayOptions,
    state: WireServingState,
) -> HttpGatewayResult<GatewayHandle> {
    match options.engine {
        GatewayEngineKind::Hyper => {
            let handle = spawn_http_gateway(options.listen_addr, state).await?;
            Ok(GatewayHandle {
                engine: GatewayEngineKind::Hyper,
                inner: GatewayEngineHandle::Hyper(handle),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use super::{GatewayEngineKind, GatewayOptions};

    #[test]
    fn gateway_options_default_to_current_hyper_engine() {
        let listen_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

        let options = GatewayOptions::new(listen_addr);

        assert_eq!(options.listen_addr, listen_addr);
        assert_eq!(options.engine, GatewayEngineKind::Hyper);
    }
}
