//! First-node install argument parsing.

use std::ffi::OsString;
use std::path::PathBuf;

use ployz_core::ids::NodeId;
use ployz_core::install::MachineBootstrapUrl;
use ployz_core::roles::FirstNodeGateway;

use crate::artifacts::{ArtifactSource, ArtifactVersion, PloyzdArtifactTarget, Sha256Digest};
use crate::cli::KeeperCliError;
use crate::steps::FirstNodeInstallTarget;
use crate::systemd::NatsServerUnitTarget;

pub(crate) fn parse_first_node_install_args(
    args: &[OsString],
) -> Result<FirstNodeInstallTarget, KeeperCliError> {
    let mut parsed = FirstNodeInstallArgs::new();
    let mut rest = args;

    while !rest.is_empty() {
        match rest {
            [flag, tail @ ..] if flag == "--gateway" => {
                parsed.set_gateway()?;
                rest = tail;
            }
            [flag] if is_value_flag(flag) => {
                return Err(KeeperCliError::MissingValue {
                    flag: flag.to_string_lossy().into_owned(),
                });
            }
            [flag, value, tail @ ..] if is_value_flag(flag) => {
                parsed.set_value(flag, value)?;
                rest = tail;
            }
            [unknown, ..] => {
                return Err(KeeperCliError::UnexpectedArgument {
                    value: unknown.to_string_lossy().into_owned(),
                });
            }
            [] => unreachable!("loop exits for empty args"),
        }
    }

    parsed.into_target()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FirstNodeInstallArgs {
    node: Option<OsString>,
    gateway: FirstNodeGateway,
    ployzd_version: Option<OsString>,
    ployzd_source: Option<OsString>,
    ployzd_sha256: Option<OsString>,
    ployzd_install_path: Option<OsString>,
    nats_binary: Option<OsString>,
    nats_config: Option<OsString>,
    machine_bootstrap_url: Option<OsString>,
}

impl FirstNodeInstallArgs {
    fn new() -> Self {
        Self {
            node: None,
            gateway: FirstNodeGateway::Skip,
            ployzd_version: None,
            ployzd_source: None,
            ployzd_sha256: None,
            ployzd_install_path: None,
            nats_binary: None,
            nats_config: None,
            machine_bootstrap_url: None,
        }
    }

    fn set_gateway(&mut self) -> Result<(), KeeperCliError> {
        if self.gateway == FirstNodeGateway::Install {
            return Err(KeeperCliError::DuplicateArgument { flag: "--gateway" });
        }
        self.gateway = FirstNodeGateway::Install;
        Ok(())
    }

    fn set_value(&mut self, flag: &OsString, value: &OsString) -> Result<(), KeeperCliError> {
        if flag == "--node" {
            return set_once(&mut self.node, value, "--node");
        }
        if flag == "--ployzd-version" {
            return set_once(&mut self.ployzd_version, value, "--ployzd-version");
        }
        if flag == "--ployzd-source" {
            return set_once(&mut self.ployzd_source, value, "--ployzd-source");
        }
        if flag == "--ployzd-sha256" {
            return set_once(&mut self.ployzd_sha256, value, "--ployzd-sha256");
        }
        if flag == "--ployzd-install-path" {
            return set_once(
                &mut self.ployzd_install_path,
                value,
                "--ployzd-install-path",
            );
        }
        if flag == "--nats-binary" {
            return set_once(&mut self.nats_binary, value, "--nats-binary");
        }
        if flag == "--nats-config" {
            return set_once(&mut self.nats_config, value, "--nats-config");
        }
        if flag == "--machine-bootstrap-url" {
            return set_once(
                &mut self.machine_bootstrap_url,
                value,
                "--machine-bootstrap-url",
            );
        }

        Err(KeeperCliError::UnexpectedArgument {
            value: flag.to_string_lossy().into_owned(),
        })
    }

    fn into_target(self) -> Result<FirstNodeInstallTarget, KeeperCliError> {
        let node = parse_node_id(required(self.node, "--node")?)?;
        let ployzd_artifact = PloyzdArtifactTarget::new(
            parse_version(required(self.ployzd_version, "--ployzd-version")?)?,
            parse_artifact_source(required(self.ployzd_source, "--ployzd-source")?)?,
            parse_digest(required(self.ployzd_sha256, "--ployzd-sha256")?)?,
            PathBuf::from(required(self.ployzd_install_path, "--ployzd-install-path")?),
        )?;
        let nats_server_unit = NatsServerUnitTarget::new(
            PathBuf::from(required(self.nats_binary, "--nats-binary")?),
            PathBuf::from(required(self.nats_config, "--nats-config")?),
        )?;

        let mut target = FirstNodeInstallTarget::new(node, ployzd_artifact, self.gateway)
            .with_nats_server_unit(nats_server_unit);
        if let Some(url) = self.machine_bootstrap_url {
            target = target.with_machine_bootstrap_url(parse_machine_bootstrap_url(url)?);
        }
        Ok(target)
    }
}

fn set_once(
    slot: &mut Option<OsString>,
    value: &OsString,
    flag: &'static str,
) -> Result<(), KeeperCliError> {
    if slot.is_some() {
        return Err(KeeperCliError::DuplicateArgument { flag });
    }
    *slot = Some(value.clone());
    Ok(())
}

fn required(value: Option<OsString>, flag: &'static str) -> Result<OsString, KeeperCliError> {
    value.ok_or(KeeperCliError::MissingRequiredArgument { flag })
}

fn parse_node_id(value: OsString) -> Result<NodeId, KeeperCliError> {
    NodeId::try_new(utf8_value(value, "--node")?).map_err(KeeperCliError::from)
}

fn parse_version(value: OsString) -> Result<ArtifactVersion, KeeperCliError> {
    ArtifactVersion::try_new(utf8_value(value, "--ployzd-version")?).map_err(KeeperCliError::from)
}

fn parse_digest(value: OsString) -> Result<Sha256Digest, KeeperCliError> {
    Sha256Digest::try_new(utf8_value(value, "--ployzd-sha256")?).map_err(KeeperCliError::from)
}

fn parse_artifact_source(value: OsString) -> Result<ArtifactSource, KeeperCliError> {
    match value.to_str() {
        Some(source) if source.starts_with("https://") || source.starts_with("http://") => {
            ArtifactSource::try_new(source.to_owned()).map_err(KeeperCliError::from)
        }
        Some(_) | None => {
            ArtifactSource::from_local_path(PathBuf::from(value)).map_err(KeeperCliError::from)
        }
    }
}

fn parse_machine_bootstrap_url(value: OsString) -> Result<MachineBootstrapUrl, KeeperCliError> {
    MachineBootstrapUrl::try_new(utf8_value(value, "--machine-bootstrap-url")?)
        .map_err(KeeperCliError::from)
}

fn utf8_value(value: OsString, flag: &'static str) -> Result<String, KeeperCliError> {
    value
        .into_string()
        .map_err(|_value| KeeperCliError::InvalidUtf8Argument { flag })
}

fn is_value_flag(value: &OsString) -> bool {
    value == "--node"
        || value == "--ployzd-version"
        || value == "--ployzd-source"
        || value == "--ployzd-sha256"
        || value == "--ployzd-install-path"
        || value == "--nats-binary"
        || value == "--nats-config"
        || value == "--machine-bootstrap-url"
}

#[cfg(test)]
mod tests {
    use ployz_core::roles::FirstNodeGateway;

    use super::parse_first_node_install_args;
    use crate::cli::KeeperCliError;

    #[test]
    fn parses_first_node_install_args_in_any_order() {
        let target = parse_first_node_install_args(&[
            "--gateway".into(),
            "--nats-config".into(),
            "/etc/nats/nats-server.conf".into(),
            "--nats-binary".into(),
            "/usr/local/bin/nats-server".into(),
            "--ployzd-install-path".into(),
            "/usr/local/bin/ployzd".into(),
            "--ployzd-sha256".into(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            "--ployzd-source".into(),
            "/tmp/ployzd".into(),
            "--ployzd-version".into(),
            "0.1.0".into(),
            "--node".into(),
            "node_1".into(),
        ])
        .expect("valid first-node install args parse");

        assert_eq!(target.node_id.as_str(), "node_1");
        assert_eq!(target.gateway, FirstNodeGateway::Install);
    }

    #[test]
    fn rejects_duplicate_first_node_install_args() {
        assert!(matches!(
            parse_first_node_install_args(&[
                "--node".into(),
                "node_1".into(),
                "--node".into(),
                "node_2".into(),
            ]),
            Err(KeeperCliError::DuplicateArgument { flag }) if flag == "--node"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn keeps_local_artifact_source_as_os_path() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let source = std::ffi::OsString::from_vec(b"/tmp/ployz-\xFF".to_vec());

        let target = parse_first_node_install_args(&[
            "--node".into(),
            "node_1".into(),
            "--ployzd-version".into(),
            "0.1.0".into(),
            "--ployzd-source".into(),
            source.clone(),
            "--ployzd-sha256".into(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            "--ployzd-install-path".into(),
            "/usr/local/bin/ployzd".into(),
            "--nats-binary".into(),
            "/usr/local/bin/nats-server".into(),
            "--nats-config".into(),
            "/etc/nats/nats-server.conf".into(),
        ])
        .expect("non-UTF-8 local source path is valid");

        let crate::artifacts::ArtifactSourceView::LocalPath(local_source) =
            target.ployzd_artifact.source.view()
        else {
            panic!("source is local");
        };
        assert_eq!(local_source.as_os_str().as_bytes(), source.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_directory_install_target() {
        use std::os::unix::ffi::OsStringExt;

        let install_path = std::ffi::OsString::from_vec(b"/tmp/ployz-\xFF/".to_vec());

        assert!(
            parse_first_node_install_args(&[
                "--node".into(),
                "node_1".into(),
                "--ployzd-version".into(),
                "0.1.0".into(),
                "--ployzd-source".into(),
                "/tmp/ployzd".into(),
                "--ployzd-sha256".into(),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                "--ployzd-install-path".into(),
                install_path,
                "--nats-binary".into(),
                "/usr/local/bin/nats-server".into(),
                "--nats-config".into(),
                "/etc/nats/nats-server.conf".into(),
            ])
            .is_err()
        );
    }
}
