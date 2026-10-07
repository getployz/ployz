//! Observed ingress endpoints after a direct Deployment.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use ployz_core::{DeployOperation, HttpProtocol, PortPublication, ReplacementOperation};

pub(super) fn endpoints_footer(completed: &[DeployOperation]) -> String {
    let mut by_service = BTreeMap::<_, BTreeSet<_>>::new();
    for operation in completed {
        let spec = match operation {
            DeployOperation::RunContainer { spec, .. } => spec,
            DeployOperation::ReplaceContainer(ReplacementOperation { spec, .. }) => spec,
            DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::PrepareVolumes { .. }
            | DeployOperation::RemoveVolume { .. } => continue,
        };
        for port in &spec.ports {
            let PortPublication::Ingress {
                hostname,
                container_port,
                load_balancer_port,
                http_protocol,
            } = port
            else {
                continue;
            };
            let (scheme, default_port) = match http_protocol {
                HttpProtocol::Https => ("https", 443),
                HttpProtocol::Http => ("http", 80),
            };
            let mut url = format!("{scheme}://{hostname}");
            if load_balancer_port.get() != default_port {
                let _ = write!(url, ":{load_balancer_port}");
            }
            by_service
                .entry((&spec.name, container_port))
                .or_default()
                .insert(url);
        }
    }
    let mut out = String::new();
    for ((service, port), endpoints) in by_service {
        let _ = writeln!(out, "\n{service} → :{port}");
        for url in endpoints {
            let _ = writeln!(out, "  {url}");
        }
    }
    out
}

#[cfg(test)]
#[path = "render_success_tests.rs"]
mod success_tests;
