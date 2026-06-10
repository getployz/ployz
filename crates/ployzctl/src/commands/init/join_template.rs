use std::fs;
use std::io::Read;

use clap::Parser;
use ployz_core::ids::NodeId;
use ployz_core::install::{
    MachineJoinArtifact, MachineJoinArtifactBundleSpec, MachineJoinBundle, MachineJoinClusterName,
    MachineJoinMaterial, MachineJoinPloyzdArtifact, MachineJoinRuntimeNatsUrl,
    MachineJoinSecretDelivery, MachineJoinTemplate,
};
use ployz_core::nats_config::trusted_nats_for_first_node;

use crate::commands::{PloyzctlCliError, clap_error, invalid_value};

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
    let parsed = MachineJoinTemplateCli::try_parse_from(
        std::iter::once("init join-template".to_owned()).chain(args.iter().cloned()),
    )
    .map_err(clap_error)?;

    let trusted_first_node = NodeId::try_new(parsed.trusted_first_node)
        .map_err(|error| invalid_value("--trusted-first-node", error))?;
    let artifacts = read_artifact_spec(parsed.artifact_spec)?;

    Ok(MachineJoinTemplateCommand {
        join_bundle: MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name: MachineJoinClusterName::try_new(parsed.cluster)
                    .map_err(|error| invalid_value("--cluster", error))?,
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(parsed.runtime_nats_url)
                    .map_err(|error| invalid_value("--runtime-nats-url", error))?,
                trusted_nats: trusted_nats_for_first_node(trusted_first_node),
                ployzd: MachineJoinPloyzdArtifact {
                    version: artifacts.ployzd.version,
                    source: artifacts.ployzd.source,
                    sha256: artifacts.ployzd.sha256,
                    install_path: artifacts.ployzd.install_path,
                },
                ebpf_bytecode: MachineJoinArtifact {
                    version: artifacts.ebpf_bytecode.version,
                    source: artifacts.ebpf_bytecode.source,
                    sha256: artifacts.ebpf_bytecode.sha256,
                    install_path: artifacts.ebpf_bytecode.install_path,
                },
                ebpf_ctl: MachineJoinArtifact {
                    version: artifacts.ebpf_ctl.version,
                    source: artifacts.ebpf_ctl.source,
                    sha256: artifacts.ebpf_ctl.sha256,
                    install_path: artifacts.ebpf_ctl.install_path,
                },
            },
        },
        secret_delivery: read_secret_delivery(parsed.secret_delivery_file)?,
    })
}

#[derive(Debug, Parser)]
#[command(name = "init join-template")]
struct MachineJoinTemplateCli {
    #[arg(long)]
    cluster: String,
    #[arg(long)]
    runtime_nats_url: String,
    #[arg(long)]
    trusted_first_node: String,
    #[arg(long)]
    artifact_spec: String,
    #[arg(long)]
    secret_delivery_file: String,
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

fn read_artifact_spec(path: String) -> Result<MachineJoinArtifactBundleSpec, PloyzctlCliError> {
    let mut contents = String::new();
    if path == "-" {
        std::io::stdin()
            .read_to_string(&mut contents)
            .map_err(|error| PloyzctlCliError::InvalidValue {
                flag: "--artifact-spec",
                message: format!("cannot read stdin: {error}"),
            })?;
    } else {
        contents = fs::read_to_string(&path).map_err(|error| PloyzctlCliError::InvalidValue {
            flag: "--artifact-spec",
            message: format!("cannot read {path}: {error}"),
        })?;
    }
    serde_json::from_str(&contents).map_err(|error| PloyzctlCliError::InvalidValue {
        flag: "--artifact-spec",
        message: format!("invalid artifact spec json: {error}"),
    })
}
