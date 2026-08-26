//! Concrete Envoy deployment wiring for the Ingress Proxy.

use ployz_core::{IngressProxyBackend, MachineTarget, RequestedServiceSpec};

/// Qualified Envoy release selected for new Clusters.
pub const ENVOY_IMAGE: &str = "docker.io/envoyproxy/envoy@sha256:d59f7f5fa10cff6d5892b6c5e7df5c9297ddfb2c3683e33fbfb82da24de4fa66";

#[must_use]
pub(super) fn service_spec(image: String, machines: Vec<MachineTarget>) -> RequestedServiceSpec {
    IngressProxyBackend::Envoy
        .requested_service_spec(image, machines, None)
        .expect("Envoy profile accepts no fragment")
}
