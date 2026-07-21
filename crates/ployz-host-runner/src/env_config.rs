use std::path::Path;

use crate::plan::FirstMachineInstallTarget;
use crate::release_manifest::{
    ReleaseManifest, default_release_manifest_url, persisted_release_manifest_url,
    read_release_manifest_text,
};
use ployz_core::ids::MachineId;
use ployz_core::install::{
    AbsoluteInstallPath, DEFAULT_MACHINE_BOOTSTRAP_URL, FirstMachineInstallSpec, HostPortAssurance,
    MachineBootstrapUrl, MachineJoinClusterName, MachineJoinRuntimeNatsUrl,
};
use ployz_core::roles::GatewayRole;

use crate::runtime::{
    PLOYZ_GATEWAY_ENV, PLOYZ_HOST_PORTS_ASSURED_EXTERNALLY_ENV, PLOYZ_MACHINE_BOOTSTRAP_URL_ENV,
    PLOYZ_MACHINE_ID_ENV, PLOYZ_MACHINE_JOIN_CLUSTER_NAME_ENV, PLOYZ_MACHINE_JOIN_NATS_URL_ENV,
    PLOYZ_MACHINE_PUBLIC_IP_ENV, PLOYZ_RELEASE_MANIFEST_URL_ENV,
};

pub(crate) fn load_versioned_release_manifest(url: &str) -> Result<ReleaseManifest, String> {
    let contents = read_release_manifest_text(url)?;
    ReleaseManifest::parse(&contents).map_err(|error| error.to_string())
}

pub(crate) fn local_core_target_from_env() -> Result<FirstMachineInstallTarget, String> {
    let manifest = load_local_release_manifest()?;
    let machine_id = env_machine_id(PLOYZ_MACHINE_ID_ENV)?;
    let runtime_nats_url = env_runtime_nats_url(PLOYZ_MACHINE_JOIN_NATS_URL_ENV)?;
    let cluster_name = env_cluster_name(PLOYZ_MACHINE_JOIN_CLUSTER_NAME_ENV)?;
    let bootstrap_url = optional_env(PLOYZ_MACHINE_BOOTSTRAP_URL_ENV)
        .map(MachineBootstrapUrl::try_new)
        .transpose()
        .map_err(|error| format!("{PLOYZ_MACHINE_BOOTSTRAP_URL_ENV} is invalid: {error}"))?
        .or_else(|| MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL).ok());

    let install = FirstMachineInstallSpec {
        machine_id,
        dataplane_endpoint_supernet: ployz_core::network::MachineEndpointSupernet::default_v1(),
        gateway: env_gateway_role(PLOYZ_GATEWAY_ENV)?,
        host_port_assurance: env_host_port_assurance()?,
        machine_public_ip: local_core_machine_public_ip_from_env()?,
        machine_bootstrap_url: bootstrap_url,
        machine_join_template_file: Some(default_machine_join_template_file()?),
        machine_join_cluster_name: cluster_name,
        machine_join_runtime_nats_url: runtime_nats_url,
        artifacts: manifest.install_artifacts()?,
    };

    crate::cli::first_machine_install_target_from_spec(install).map_err(|error| error.to_string())
}

pub(crate) fn default_machine_join_template_file() -> Result<AbsoluteInstallPath, String> {
    AbsoluteInstallPath::try_new("/etc/ployz/machine-join-template.json")
        .map_err(|error| error.to_string())
}

fn load_local_release_manifest() -> Result<ReleaseManifest, String> {
    let url = local_release_manifest_url(Path::new("/etc/ployz/release.env"));
    let contents = read_release_manifest_text(&url)?;
    ReleaseManifest::parse(&contents).map_err(|error| error.to_string())
}

pub(crate) fn local_release_manifest_url(release_env_path: &Path) -> String {
    release_manifest_url_from_sources(
        optional_env(PLOYZ_RELEASE_MANIFEST_URL_ENV),
        release_env_path,
    )
}

fn release_manifest_url_from_sources(
    env_manifest_url: Option<String>,
    release_env_path: &Path,
) -> String {
    env_manifest_url
        .or_else(|| persisted_release_manifest_url(release_env_path).ok())
        .unwrap_or_else(default_release_manifest_url)
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_machine_id(name: &str) -> Result<MachineId, String> {
    let value = optional_env(name).ok_or_else(|| format!("{name} is required"))?;
    MachineId::try_new(value.clone())
        .map_err(|error| format!("{name}={value:?} is invalid: {error}"))
}

fn env_runtime_nats_url(name: &str) -> Result<MachineJoinRuntimeNatsUrl, String> {
    let value = optional_env(name).ok_or_else(|| format!("{name} is required"))?;
    MachineJoinRuntimeNatsUrl::try_new(value.clone())
        .map_err(|error| format!("{name}={value:?} is invalid: {error}"))
}

fn env_cluster_name(name: &str) -> Result<MachineJoinClusterName, String> {
    let value = optional_env(name).ok_or_else(|| format!("{name} is required"))?;
    MachineJoinClusterName::try_new(value.clone())
        .map_err(|error| format!("{name}={value:?} is invalid: {error}"))
}

fn env_gateway_role(name: &str) -> Result<GatewayRole, String> {
    match optional_env(name).as_deref() {
        None | Some("install") => Ok(GatewayRole::Install),
        Some("skip") => Ok(GatewayRole::Skip),
        Some(value) => Err(format!("{name}={value:?} must be install or skip")),
    }
}

fn env_host_port_assurance() -> Result<HostPortAssurance, String> {
    match optional_env(PLOYZ_HOST_PORTS_ASSURED_EXTERNALLY_ENV).as_deref() {
        None => Ok(HostPortAssurance::Keeper),
        Some("1") => Ok(HostPortAssurance::External),
        Some(value) => Err(format!(
            "{PLOYZ_HOST_PORTS_ASSURED_EXTERNALLY_ENV}={value:?} must be 1 when set"
        )),
    }
}

fn local_core_machine_public_ip_from_env() -> Result<Option<std::net::IpAddr>, String> {
    let Some(value) = optional_env(PLOYZ_MACHINE_PUBLIC_IP_ENV) else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|error| format!("{PLOYZ_MACHINE_PUBLIC_IP_ENV}={value:?} is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{default_machine_join_template_file, release_manifest_url_from_sources};

    #[test]
    fn local_core_bootstrap_writes_machine_join_template_at_default_path() {
        assert_eq!(
            default_machine_join_template_file()
                .expect("default machine join template path is valid")
                .as_str(),
            "/etc/ployz/machine-join-template.json"
        );
    }

    #[test]
    fn local_release_manifest_url_uses_persisted_release_when_env_is_absent() {
        let root = tempfile::tempdir().expect("tempdir");
        let release_env = root.path().join("release.env");
        fs::write(
            &release_env,
            "PLOYZ_RELEASE_MANIFEST_URL=https://example.test/ployz-release-linux-amd64.env\n",
        )
        .expect("write release env");

        assert_eq!(
            release_manifest_url_from_sources(None, &release_env),
            "https://example.test/ployz-release-linux-amd64.env"
        );
        assert_eq!(
            release_manifest_url_from_sources(
                Some("https://override.test/manifest.env".to_owned()),
                &release_env
            ),
            "https://override.test/manifest.env"
        );
    }
}
