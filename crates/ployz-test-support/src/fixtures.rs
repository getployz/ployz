//! Canonical control-plane fixtures: the machine-join template/bundle, the
//! install artifact family, and deploy request builders shared by the
//! workspace test suites.

use ployz_core::deploy::{
    DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec, ImageReference, ReplicaCount,
};
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
    InstallSha256Digest, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
    MachineJoinRuntimeNatsUrl, MachineJoinTemplate, MachineJoinTrustedNats,
};
use ployz_core::machine_runtime::MachineDiskSpace;
use ployz_core::nats_config::NatsCaCertificatePem;
use ployz_core::state::ServingTargetEntry;

use crate::ids::{
    namespace_id, namespace_revision_entry_id, route_hostname, route_port, service_id,
};

/// A syntactically valid (not real) PEM literal for join-material fixtures.
pub const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n";

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
        desired_replicas: ReplicaCount::try_new(1).expect("valid replica count"),
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

/// Join material for cluster `prod` with the canonical three artifacts.
#[must_use]
pub fn machine_join_material(runtime_nats_url: &str, ca_pem: &str) -> MachineJoinMaterial {
    MachineJoinMaterial {
        cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
        dataplane_endpoint_supernet: ployz_core::dataplane::MachineEndpointSupernet::default_v1(),
        runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(runtime_nats_url)
            .expect("valid runtime nats url"),
        trusted_nats: MachineJoinTrustedNats {
            ca_pem: NatsCaCertificatePem::try_new(ca_pem).expect("valid ca pem"),
        },
        recovery_key_wrapped: ployz_core::install::WrappedCaKey::new(vec![1, 2, 3]),
        core_seeds_wrapped: ployz_core::install::WrappedCoreSeeds::new(vec![4, 5, 6]),
        ployzd: install_artifact("/tmp/ployzd", "/usr/local/bin/ployzd"),
        ebpf_bytecode: install_artifact(
            "/tmp/ployz-ebpf-tc",
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
        ),
        ebpf_ctl: install_artifact("/tmp/ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
    }
}

/// The static join bundle used where no live NATS fixture is involved.
#[must_use]
pub fn machine_join_bundle() -> MachineJoinBundle {
    MachineJoinBundle {
        material: machine_join_material("nats://127.0.0.1:7422", TEST_CA_PEM),
    }
}

/// The static join template counterpart of [`machine_join_bundle`].
#[must_use]
pub fn machine_join_template() -> MachineJoinTemplate {
    MachineJoinTemplate {
        join_bundle: machine_join_bundle(),
    }
}

/// The canonical unrouted deploy request: an api image with one replica.
#[must_use]
pub fn deploy_target(service: &str) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        services: vec![DeployServiceSpec {
            service_id: service_id(service),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            routes: Vec::new(),
        }],
    }
}

/// [`deploy_target`] with a gateway route attached.
#[must_use]
pub fn deploy_target_with_route(
    service: &str,
    hostname: &str,
    gateway_port: u16,
    endpoint_port: u16,
) -> DeployRequest {
    let mut request = deploy_target(service);
    let [service_spec] = request.services.as_mut_slice() else {
        panic!("deploy target fixture has one service");
    };
    service_spec.routes.push(DeployRoute {
        target: DeployRouteTarget::Hostname {
            hostname: route_hostname(hostname),
            port: route_port(gateway_port),
        },
        endpoint_port: route_port(endpoint_port),
    });
    request
}
