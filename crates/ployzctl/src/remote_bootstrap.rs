//! Building blocks for remote machine bootstrap over SSH.
//!
//! `ployzctl machine init USER@HOST` builds the existing first-node install
//! spec internally from a release manifest, prepares the machine over SSH,
//! and runs the real installer in first-node mode. `ployzctl machine add
//! USER@HOST` reuses the same installer-source vocabulary for the join mode.
//! Everything here is pure: manifest parsing, spec/template construction,
//! and POSIX remote command rendering. The SSH execution itself lives in
//! [`crate::runtime`].

use std::fmt;
use std::net::IpAddr;

use ployz_core::ids::NodeId;
use ployz_core::install::{
    AbsoluteInstallPath, FirstNodeInstallArtifacts, FirstNodeInstallSpec, InstallArtifactSource,
    InstallArtifactSpec, InstallArtifactVersion, InstallSha256Digest, MachineBootstrapUrl,
    MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial, MachineJoinRuntimeNatsUrl,
    MachineJoinTemplate, MachineJoinTrustedNats, NatsMachineMaterialPaths, NatsServerInstallSpec,
};
use ployz_core::nats_config::NatsCaCertificatePem;
use ployz_core::roles::InstallRolePolicy;

use crate::shell::shell_quote;

/// Release version the quick start installs when `--version` is not given.
/// Must match the `default_version` in `scripts/ployz.sh`.
pub const DEFAULT_RELEASE_VERSION: &str = "v0.0.1-alpha.1";
/// The default installer the remote machine pipes through `sh`.
pub const DEFAULT_BOOTSTRAP_URL: &str = "https://ployz.sh";
/// Default cluster name recorded in the machine-join template.
pub const DEFAULT_CLUSTER_NAME: &str = "ployz";
/// The NATS server release the first node runs when the machine does not
/// already provide `/usr/local/bin/nats-server` (matches the proof recipe).
pub const PINNED_NATS_SERVER_VERSION: &str = "2.14.2";
/// The cluster control-plane NATS port on every machine.
pub const MACHINE_NATS_PORT: u16 = 4222;

pub const REMOTE_PLOYZD_INSTALL_PATH: &str = "/usr/local/bin/ployzd";
pub const REMOTE_EBPF_BYTECODE_INSTALL_PATH: &str = "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc";
pub const REMOTE_EBPF_CTL_INSTALL_PATH: &str = "/usr/local/bin/ployz-ebpf-ctl";
pub const REMOTE_NATS_SERVER_BINARY: &str = "/usr/local/bin/nats-server";
pub const REMOTE_NATS_SERVER_CONFIG: &str = "/etc/nats/nats-server.conf";
pub const REMOTE_JOIN_TEMPLATE_PATH: &str = "/etc/ployz/machine-join-template.json";
pub const REMOTE_FIRST_NODE_SPEC_PATH: &str = "/etc/ployz/first-node-install.json";

/// Syntactically valid placeholder CA: the join template must parse when
/// `ployzd-control` first starts, before the keeper mints the cluster CA.
/// It is replaced with the real CA right after the installer finishes.
pub const PLACEHOLDER_CA_PEM: &str =
    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n";

/// How the remote machine obtains the installer script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineInstallerSource {
    /// Product path: `curl -fsSL -- <url> | sh -s -- ...`.
    BootstrapUrl(MachineBootstrapUrl),
    /// Test/automation seam: an installer script already present on the
    /// remote machine, run as `sh <path> ...`.
    RemoteScript(String),
}

/// Maps `uname -m` output to the release platform arch slug.
pub fn release_arch_slug(uname_machine: &str) -> Result<&'static str, RemoteBootstrapError> {
    match uname_machine.trim() {
        "x86_64" | "amd64" => Ok("amd64"),
        "aarch64" | "arm64" => Ok("arm64"),
        other => Err(RemoteBootstrapError::UnsupportedArchitecture {
            value: other.to_owned(),
        }),
    }
}

/// The default release manifest URL for a Linux machine of the given arch,
/// mirroring `scripts/ployz.sh`.
#[must_use]
pub fn default_release_manifest_url(version: &str, arch_slug: &str) -> String {
    let release_tag = if version.starts_with('v') {
        version.to_owned()
    } else {
        format!("v{version}")
    };
    format!(
        "https://github.com/getployz/ployz/releases/download/{release_tag}/ployz-release-linux-{arch_slug}.env"
    )
}

/// One verified release artifact resolved from the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifact {
    pub source: InstallArtifactSource,
    pub sha256: InstallSha256Digest,
}

/// The machine artifacts `machine init` needs from a release manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactManifest {
    pub version: InstallArtifactVersion,
    pub ployzd: ReleaseArtifact,
    pub ebpf_bytecode: ReleaseArtifact,
    pub ebpf_ctl: ReleaseArtifact,
}

/// Parses the `KEY=VALUE` release manifest the installer also consumes.
/// Missing or invalid keys name the key and the manifest URL (R14-adjacent:
/// the failure tells the operator which release file is broken).
pub fn parse_release_manifest(
    text: &str,
    manifest_url: &str,
) -> Result<ReleaseArtifactManifest, RemoteBootstrapError> {
    let value_of = |key: &str| -> Result<&str, RemoteBootstrapError> {
        text.lines()
            .find_map(|line| {
                line.strip_prefix(key)
                    .and_then(|rest| rest.strip_prefix('='))
            })
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| RemoteBootstrapError::ManifestMissingKey {
                key: key.to_owned(),
                manifest_url: manifest_url.to_owned(),
            })
    };
    let invalid = |key: &str, message: String| RemoteBootstrapError::ManifestInvalidValue {
        key: key.to_owned(),
        manifest_url: manifest_url.to_owned(),
        message,
    };
    let artifact = |name: &str| -> Result<ReleaseArtifact, RemoteBootstrapError> {
        let url_key = format!("{name}_URL");
        let sha_key = format!("{name}_SHA256");
        Ok(ReleaseArtifact {
            source: InstallArtifactSource::try_new(value_of(&url_key)?)
                .map_err(|error| invalid(&url_key, error.to_string()))?,
            sha256: InstallSha256Digest::try_new(value_of(&sha_key)?)
                .map_err(|error| invalid(&sha_key, error.to_string()))?,
        })
    };

    Ok(ReleaseArtifactManifest {
        version: InstallArtifactVersion::try_new(value_of("PLOYZ_VERSION")?)
            .map_err(|error| invalid("PLOYZ_VERSION", error.to_string()))?,
        ployzd: artifact("PLOYZD")?,
        ebpf_bytecode: artifact("PLOYZ_EBPF_TC")?,
        ebpf_ctl: artifact("PLOYZ_EBPF_CTL")?,
    })
}

fn install_path(value: &str) -> AbsoluteInstallPath {
    AbsoluteInstallPath::try_new(value).expect("well-known install path is absolute")
}

fn artifact_spec(
    artifact: &ReleaseArtifact,
    version: &InstallArtifactVersion,
    path: &str,
) -> InstallArtifactSpec {
    InstallArtifactSpec {
        version: version.clone(),
        source: artifact.source.clone(),
        sha256: artifact.sha256.clone(),
        install_path: install_path(path),
    }
}

/// Builds the existing first-node install spec from the derived identity,
/// the role policy, and the resolved release artifacts. This is exactly the
/// spec `ployzctl init --run-keeper-install` would have been handed by the
/// proof harness — the operator just never sees it (KTD1).
#[must_use]
pub fn build_first_node_install_spec(
    node_id: NodeId,
    roles: InstallRolePolicy,
    node_public_ip: Option<IpAddr>,
    bootstrap_url: MachineBootstrapUrl,
    manifest: &ReleaseArtifactManifest,
    nats_server_sha256: InstallSha256Digest,
) -> FirstNodeInstallSpec {
    let InstallRolePolicy { gateway, dns } = roles;
    FirstNodeInstallSpec {
        node_id,
        gateway,
        dns,
        node_public_ip,
        machine_bootstrap_url: Some(bootstrap_url),
        machine_join_template_file: Some(install_path(REMOTE_JOIN_TEMPLATE_PATH)),
        artifacts: FirstNodeInstallArtifacts {
            ployzd: artifact_spec(
                &manifest.ployzd,
                &manifest.version,
                REMOTE_PLOYZD_INSTALL_PATH,
            ),
            ebpf_bytecode: artifact_spec(
                &manifest.ebpf_bytecode,
                &manifest.version,
                REMOTE_EBPF_BYTECODE_INSTALL_PATH,
            ),
            ebpf_ctl: artifact_spec(
                &manifest.ebpf_ctl,
                &manifest.version,
                REMOTE_EBPF_CTL_INSTALL_PATH,
            ),
            nats_server: NatsServerInstallSpec {
                version: InstallArtifactVersion::try_new(PINNED_NATS_SERVER_VERSION)
                    .expect("pinned nats-server version is non-empty"),
                source: InstallArtifactSource::try_new(REMOTE_NATS_SERVER_BINARY)
                    .expect("nats-server binary path is absolute"),
                sha256: nats_server_sha256,
                binary: install_path(REMOTE_NATS_SERVER_BINARY),
                config: install_path(REMOTE_NATS_SERVER_CONFIG),
            },
        },
    }
}

/// The cluster NATS URL joined machines (and the local operator) dial.
pub fn runtime_nats_url(host: &str) -> Result<MachineJoinRuntimeNatsUrl, RemoteBootstrapError> {
    MachineJoinRuntimeNatsUrl::try_new(format!("tls://{host}:{MACHINE_NATS_PORT}")).map_err(
        |error| RemoteBootstrapError::InvalidRuntimeNatsUrl {
            host: host.to_owned(),
            message: error.to_string(),
        },
    )
}

/// Builds the machine-join template `ployzd-control` serves join bundles
/// from. Written once with [`PLACEHOLDER_CA_PEM`] before install and
/// re-rendered with the keeper-minted CA afterwards.
pub fn build_machine_join_template(
    cluster_name: MachineJoinClusterName,
    runtime_nats_url: MachineJoinRuntimeNatsUrl,
    ca_pem: &str,
    manifest: &ReleaseArtifactManifest,
) -> Result<MachineJoinTemplate, RemoteBootstrapError> {
    let ca_pem = NatsCaCertificatePem::try_new(ca_pem).map_err(|error| {
        RemoteBootstrapError::InvalidClusterCa {
            message: error.to_string(),
        }
    })?;
    Ok(MachineJoinTemplate {
        join_bundle: MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name,
                runtime_nats_url,
                trusted_nats: MachineJoinTrustedNats { ca_pem },
                ployzd: artifact_spec(
                    &manifest.ployzd,
                    &manifest.version,
                    REMOTE_PLOYZD_INSTALL_PATH,
                ),
                ebpf_bytecode: artifact_spec(
                    &manifest.ebpf_bytecode,
                    &manifest.version,
                    REMOTE_EBPF_BYTECODE_INSTALL_PATH,
                ),
                ebpf_ctl: artifact_spec(
                    &manifest.ebpf_ctl,
                    &manifest.version,
                    REMOTE_EBPF_CTL_INSTALL_PATH,
                ),
            },
        },
    })
}

// ---------------------------------------------------------------------------
// Remote command rendering (POSIX-shaped, bounded by the SSH layer)
// ---------------------------------------------------------------------------

/// Reads the remote machine architecture for manifest resolution.
pub const READ_ARCH_COMMAND: &str = "uname -m";

/// Restarts the (disposable) control role so it serves join bundles with the
/// keeper-minted cluster CA.
pub const RESTART_CONTROL_COMMAND: &str = "systemctl restart ployzd-control";

/// Fetches the release manifest on the remote machine (the machine needs
/// `curl` for the installer anyway, and test manifests may be `file://`
/// URLs only resolvable inside the machine).
#[must_use]
pub fn fetch_manifest_command(manifest_url: &str) -> String {
    format!("curl -fsSL -- {}", shell_quote(manifest_url))
}

/// Ensures `/usr/local/bin/nats-server` exists (downloading the pinned
/// upstream release when absent) and prints its sha256 for the install spec.
#[must_use]
pub fn ensure_nats_server_command() -> String {
    format!(
        r#"set -eu
if [ ! -x {binary} ]; then
  arch="$(uname -m)"
  case "$arch" in
    x86_64 | amd64) nats_arch=amd64 ;;
    aarch64 | arm64) nats_arch=arm64 ;;
    *) echo "unsupported architecture: $arch" >&2; exit 1 ;;
  esac
  curl -fsSL -o /tmp/ployz-nats-server.tar.gz "https://github.com/nats-io/nats-server/releases/download/v{version}/nats-server-v{version}-linux-${{nats_arch}}.tar.gz"
  tar -xzf /tmp/ployz-nats-server.tar.gz -C /tmp
  install -m 0755 "/tmp/nats-server-v{version}-linux-${{nats_arch}}/nats-server" {binary}
  rm -rf /tmp/ployz-nats-server.tar.gz "/tmp/nats-server-v{version}-linux-${{nats_arch}}"
fi
sha256sum {binary}"#,
        binary = REMOTE_NATS_SERVER_BINARY,
        version = PINNED_NATS_SERVER_VERSION,
    )
}

/// Parses the digest out of `sha256sum <path>` output.
pub fn parse_sha256_output(stdout: &str) -> Result<InstallSha256Digest, RemoteBootstrapError> {
    let Some(line) = stdout.lines().rev().find(|line| !line.trim().is_empty()) else {
        return Err(RemoteBootstrapError::InvalidSha256Output {
            output: stdout.to_owned(),
        });
    };
    let Some(digest) = line.split_whitespace().next() else {
        return Err(RemoteBootstrapError::InvalidSha256Output {
            output: stdout.to_owned(),
        });
    };
    InstallSha256Digest::try_new(digest).map_err(|_| RemoteBootstrapError::InvalidSha256Output {
        output: stdout.to_owned(),
    })
}

/// Writes a small config file on the remote machine (creates the parent
/// directory). The content travels shell-quoted on the command line, which
/// is fine for the spec/template JSON this flow writes.
#[must_use]
pub fn write_remote_file_command(path: &str, contents: &str) -> String {
    let parent = std::path::Path::new(path)
        .parent()
        .and_then(|parent| parent.to_str())
        .unwrap_or("/");
    format!(
        "mkdir -p {} && printf '%s' {} > {}",
        shell_quote(parent),
        shell_quote(contents),
        shell_quote(path)
    )
}

/// Reads one remote file (CA, seeds) back to the operator machine.
#[must_use]
pub fn read_remote_file_command(path: &str) -> String {
    format!("cat {}", shell_quote(path))
}

/// Well-known remote NATS material paths the keeper install leaves behind.
#[must_use]
pub fn remote_material_paths() -> NatsMachineMaterialPaths {
    NatsMachineMaterialPaths::in_default_state_dir()
}

/// The remote command that runs the installer in first-node mode.
#[must_use]
pub fn first_node_installer_command(
    installer: &MachineInstallerSource,
    manifest_url: &str,
    version: &str,
) -> String {
    let env = format!(
        "PLOYZ_VERSION={} PLOYZ_RELEASE_MANIFEST_URL={}",
        shell_quote(version),
        shell_quote(manifest_url)
    );
    match installer {
        MachineInstallerSource::BootstrapUrl(url) => format!(
            "curl -fsSL -- {} | {env} sh -s -- --first-node-spec {}",
            shell_quote(url.as_str()),
            shell_quote(REMOTE_FIRST_NODE_SPEC_PATH)
        ),
        MachineInstallerSource::RemoteScript(path) => format!(
            "{env} sh {} --first-node-spec {}",
            shell_quote(path),
            shell_quote(REMOTE_FIRST_NODE_SPEC_PATH)
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteBootstrapError {
    UnsupportedArchitecture {
        value: String,
    },
    ManifestMissingKey {
        key: String,
        manifest_url: String,
    },
    ManifestInvalidValue {
        key: String,
        manifest_url: String,
        message: String,
    },
    InvalidSha256Output {
        output: String,
    },
    InvalidRuntimeNatsUrl {
        host: String,
        message: String,
    },
    InvalidClusterCa {
        message: String,
    },
}

impl fmt::Display for RemoteBootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedArchitecture { value } => write!(
                formatter,
                "remote machine architecture {value:?} is unsupported (ployz machines support amd64 and arm64)"
            ),
            Self::ManifestMissingKey { key, manifest_url } => write!(
                formatter,
                "release manifest {manifest_url} is missing {key}"
            ),
            Self::ManifestInvalidValue {
                key,
                manifest_url,
                message,
            } => write!(
                formatter,
                "release manifest {manifest_url} has an invalid {key}: {message}"
            ),
            Self::InvalidSha256Output { output } => write!(
                formatter,
                "remote sha256sum output is not a digest: {output:?}"
            ),
            Self::InvalidRuntimeNatsUrl { host, message } => write!(
                formatter,
                "cannot build the cluster NATS URL from host {host:?}: {message}"
            ),
            Self::InvalidClusterCa { message } => {
                write!(formatter, "collected cluster CA is invalid: {message}")
            }
        }
    }
}

impl std::error::Error for RemoteBootstrapError {}
