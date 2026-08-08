//! Canonical deploy, runtime, and install-artifact fixtures shared by the
//! workspace test suites.

use ployz_core::deploy::{
    DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec, ImageReference, ReplicaCount,
};
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
    InstallSha256Digest,
};
use ployz_core::intent::ServingTargetEntry;
use ployz_core::machine::runtime::MachineDiskSpace;

use crate::ids::{
    namespace_id, namespace_revision_entry_id, route_hostname, route_port, service_id,
};

/// A syntactically valid sha256 hex digest for artifact fixtures.
pub const TEST_SHA256_DIGEST: &str =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[must_use]
pub const fn test_disk_space() -> MachineDiskSpace {
    MachineDiskSpace {
        available_bytes: 40,
        total_bytes: 100,
    }
}

#[must_use]
pub fn serving_target_entry(service: &str, entry: &str) -> ServingTargetEntry {
    serving_target_entry_in("default", service, entry)
}

#[must_use]
pub fn serving_target_entry_in(namespace: &str, service: &str, entry: &str) -> ServingTargetEntry {
    ServingTargetEntry {
        namespace_id: namespace_id(namespace),
        service_id: service_id(service),
        namespace_revision_entry_id: namespace_revision_entry_id(entry),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
        mode: ployz_core::deploy::ServiceMode::Replicated {
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
        },
        volume_names: Vec::new(),
    }
}

#[must_use]
pub fn install_artifact(source: &str, install_path: &str) -> InstallArtifactSpec {
    InstallArtifactSpec {
        version: InstallArtifactVersion::try_new("0.1.0").expect("valid artifact version"),
        source: InstallArtifactSource::try_new(source).expect("valid artifact source"),
        sha256: InstallSha256Digest::try_new(TEST_SHA256_DIGEST).expect("valid artifact digest"),
        install_path: AbsoluteInstallPath::try_new(install_path)
            .expect("valid artifact install path"),
    }
}

/// The canonical unrouted deploy request: an api image with one replica.
#[must_use]
pub fn deploy_target(service: &str) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id(service),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
            mode: ployz_core::deploy::ServiceMode::Replicated {
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            },
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

/// [`deploy_target`] with a gateway route attached.
#[must_use]
pub fn deploy_target_with_route(
    service: &str,
    hostname: &str,
    _gateway_port: u16,
    endpoint_port: u16,
) -> DeployRequest {
    let mut request = deploy_target(service);
    let [service_spec] = request.services.as_mut_slice() else {
        panic!("deploy target fixture has one service");
    };
    service_spec.routes.push(DeployRoute {
        target: DeployRouteTarget::Hostname {
            hostname: route_hostname(hostname),
        },
        endpoint_port: route_port(endpoint_port),
    });
    request
}
