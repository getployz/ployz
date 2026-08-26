//! Concrete Envoy deployment wiring for the Ingress Proxy.

use ployz_core::{IngressProxyBackend, MachineTarget, RequestedServiceSpec};

/// Qualified Envoy release selected for new Clusters.
pub const ENVOY_IMAGE: &str = "docker.io/envoyproxy/envoy@sha256:a707c3821b4cecb5db43d8e86e983e0f57b81010fefbabc01feeb071fb8cc08e";

#[must_use]
pub(super) fn service_spec(image: String, machines: Vec<MachineTarget>) -> RequestedServiceSpec {
    IngressProxyBackend::Envoy
        .requested_service_spec(image, machines, None)
        .expect("Envoy profile accepts no fragment")
}
