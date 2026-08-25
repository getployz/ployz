//! Concrete Zentinel deployment wiring for the Ingress Proxy.

use ployz_core::{IngressProxyBackend, MachineTarget, RequestedServiceSpec};

/// Qualified Zentinel release selected for new Clusters.
pub const ZENTINEL_IMAGE: &str = "ghcr.io/zentinelproxy/zentinel@sha256:ff012547034d13a7d8e6570679c897e4bba6bc702ec5bdd7bf70a7a04b4d6604";

#[must_use]
pub(super) fn service_spec(image: String, machines: Vec<MachineTarget>) -> RequestedServiceSpec {
    IngressProxyBackend::Zentinel
        .requested_service_spec(image, machines, None)
        .expect("Zentinel profile accepts no fragment")
}
