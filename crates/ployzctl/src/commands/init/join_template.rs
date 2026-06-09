use std::fs;

use ployz_core::ids::NodeId;
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
    MachineJoinBundle, MachineJoinClusterName, MachineJoinCoreIrohEndpoint,
    MachineJoinIrohPublicKey, MachineJoinMaterial, MachineJoinPloyzdArtifact,
    MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTemplate,
};
use ployz_core::nats_config::trusted_nats_for_first_node;

use crate::commands::{ArgCursor, PloyzctlCliError, invalid_value, required, set_once};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineJoinTemplateCommand {
    pub join_bundle: MachineJoinBundle,
    pub secret_delivery: MachineJoinSecretDelivery,
}

impl MachineJoinTemplateCommand {
    #[must_use]
    pub fn template(&self) -> MachineJoinTemplate {
        MachineJoinTemplate {
            join_bundle: self.join_bundle.clone(),
            secret_delivery: self.secret_delivery.clone(),
        }
    }

    #[must_use]
    pub fn render_json(&self) -> String {
        let mut json =
            serde_json::to_string_pretty(&self.template()).expect("join template serializes");
        json.push('\n');
        json
    }
}

pub fn parse_machine_join_template_command(
    args: &[String],
) -> Result<MachineJoinTemplateCommand, PloyzctlCliError> {
    let mut parsed = ParsedMachineJoinTemplateArgs::default();
    let mut args = ArgCursor::new(args);

    while !args.is_empty() {
        if let Some(value) = args.take_value("--cluster")? {
            set_once(&mut parsed.cluster_name, value, "--cluster")?;
            continue;
        }
        if let Some(value) = args.take_value("--runtime-nats-url")? {
            set_once(&mut parsed.runtime_nats_url, value, "--runtime-nats-url")?;
            continue;
        }
        if let Some(value) = args.take_value("--trusted-first-node")? {
            set_once(
                &mut parsed.trusted_first_node,
                value,
                "--trusted-first-node",
            )?;
            continue;
        }
        if let Some(value) = args.take_value("--core-iroh-public-key")? {
            set_once(
                &mut parsed.core_iroh_public_key,
                value,
                "--core-iroh-public-key",
            )?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-version")? {
            set_once(&mut parsed.ployzd_version, value, "--ployzd-version")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-source")? {
            set_once(&mut parsed.ployzd_source, value, "--ployzd-source")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-sha256")? {
            set_once(&mut parsed.ployzd_sha256, value, "--ployzd-sha256")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-install-path")? {
            set_once(
                &mut parsed.ployzd_install_path,
                value,
                "--ployzd-install-path",
            )?;
            continue;
        }
        if let Some(value) = args.take_value("--secret-delivery-file")? {
            set_once(
                &mut parsed.secret_delivery_file,
                value,
                "--secret-delivery-file",
            )?;
            continue;
        }
        return Err(args.unexpected());
    }

    parsed.into_command()
}

#[derive(Debug, Default)]
struct ParsedMachineJoinTemplateArgs {
    cluster_name: Option<String>,
    runtime_nats_url: Option<String>,
    trusted_first_node: Option<String>,
    core_iroh_public_key: Option<String>,
    ployzd_version: Option<String>,
    ployzd_source: Option<String>,
    ployzd_sha256: Option<String>,
    ployzd_install_path: Option<String>,
    secret_delivery_file: Option<String>,
}

impl ParsedMachineJoinTemplateArgs {
    fn into_command(self) -> Result<MachineJoinTemplateCommand, PloyzctlCliError> {
        let trusted_first_node =
            NodeId::try_new(required(self.trusted_first_node, "--trusted-first-node")?)
                .map_err(|error| invalid_value("--trusted-first-node", error))?;

        Ok(MachineJoinTemplateCommand {
            join_bundle: MachineJoinBundle {
                material: MachineJoinMaterial {
                    cluster_name: MachineJoinClusterName::try_new(required(
                        self.cluster_name,
                        "--cluster",
                    )?)
                    .map_err(|error| invalid_value("--cluster", error))?,
                    runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(required(
                        self.runtime_nats_url,
                        "--runtime-nats-url",
                    )?)
                    .map_err(|error| invalid_value("--runtime-nats-url", error))?,
                    trusted_nats: trusted_nats_for_first_node(trusted_first_node.clone()),
                    core_iroh: MachineJoinCoreIrohEndpoint {
                        node_id: trusted_first_node,
                        public_key: MachineJoinIrohPublicKey::try_new(required(
                            self.core_iroh_public_key,
                            "--core-iroh-public-key",
                        )?)
                        .map_err(|error| invalid_value("--core-iroh-public-key", error))?,
                    },
                    ployzd: MachineJoinPloyzdArtifact {
                        version: InstallArtifactVersion::try_new(required(
                            self.ployzd_version,
                            "--ployzd-version",
                        )?)
                        .map_err(|error| invalid_value("--ployzd-version", error))?,
                        source: InstallArtifactSource::try_new(required(
                            self.ployzd_source,
                            "--ployzd-source",
                        )?)
                        .map_err(|error| invalid_value("--ployzd-source", error))?,
                        sha256: InstallSha256Digest::try_new(required(
                            self.ployzd_sha256,
                            "--ployzd-sha256",
                        )?)
                        .map_err(|error| invalid_value("--ployzd-sha256", error))?,
                        install_path: AbsoluteInstallPath::try_new(required(
                            self.ployzd_install_path,
                            "--ployzd-install-path",
                        )?)
                        .map_err(|error| invalid_value("--ployzd-install-path", error))?,
                    },
                },
            },
            secret_delivery: read_secret_delivery(required(
                self.secret_delivery_file,
                "--secret-delivery-file",
            )?)?,
        })
    }
}

fn read_secret_delivery(path: String) -> Result<MachineJoinSecretDelivery, PloyzctlCliError> {
    let contents = fs::read_to_string(&path).map_err(|error| PloyzctlCliError::InvalidValue {
        flag: "--secret-delivery-file",
        message: format!("cannot read {path}: {error}"),
    })?;
    serde_json::from_str(&contents).map_err(|error| PloyzctlCliError::InvalidValue {
        flag: "--secret-delivery-file",
        message: format!("invalid secret delivery json: {error}"),
    })
}
