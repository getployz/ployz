use std::fs;
use std::io::Read;

use clap::Args;
use ployz_core::install::{
    MachineJoinArtifactBundleSpec, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
    MachineJoinRuntimeNatsUrl, MachineJoinTemplate, MachineJoinTrustedNats, WrappedCaKey,
    WrappedCoreSeeds,
};
use ployz_core::nats_config::NatsCaCertificatePem;

use crate::commands::{PloyzctlCliError, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineJoinTemplateCommand {
    pub join_bundle: MachineJoinBundle,
}

impl MachineJoinTemplateCommand {
    #[must_use]
    pub fn template(&self) -> MachineJoinTemplate {
        MachineJoinTemplate {
            join_bundle: self.join_bundle.clone(),
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

pub(crate) fn machine_join_template_command(
    parsed: MachineJoinTemplateCli,
) -> Result<MachineJoinTemplateCommand, PloyzctlCliError> {
    let trusted_nats_ca = read_trusted_nats_ca(parsed.trusted_nats_ca_file)?;
    let recovery_key_wrapped = read_recovery_key(parsed.recovery_key_file)?;
    let core_seeds_wrapped = read_core_seeds(parsed.core_seeds_file)?;
    let artifacts = read_artifact_spec(parsed.artifact_spec)?;

    Ok(MachineJoinTemplateCommand {
        join_bundle: MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name: MachineJoinClusterName::try_new(parsed.cluster)
                    .map_err(|error| invalid_value("--cluster", error))?,
                dataplane_endpoint_supernet:
                    ployz_core::network::MachineEndpointSupernet::default_v1(),
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(parsed.runtime_nats_url)
                    .map_err(|error| invalid_value("--runtime-nats-url", error))?,
                trusted_nats: MachineJoinTrustedNats {
                    ca_pem: trusted_nats_ca,
                },
                recovery_key_wrapped,
                core_seeds_wrapped,
                ployzd: artifacts.ployzd,
                ebpf_bytecode: artifacts.ebpf_bytecode,
                ebpf_ctl: artifacts.ebpf_ctl,
                railpack: artifacts.railpack,
            },
        },
    })
}

#[derive(Debug, Args)]
pub(crate) struct MachineJoinTemplateCli {
    #[arg(long)]
    cluster: String,
    #[arg(long)]
    runtime_nats_url: String,
    #[arg(long)]
    trusted_nats_ca_file: String,
    /// The wrapped CA recovery key emitted at init (`ca-recovery.key`), delivered
    /// to joiners so they can be core-promoted.
    #[arg(long)]
    recovery_key_file: String,
    /// The wrapped core principal seeds emitted at init (`core-seeds.key`), so a
    /// promoted core reuses the old core's principals verbatim.
    #[arg(long)]
    core_seeds_file: String,
    #[arg(long)]
    artifact_spec: String,
}

fn read_recovery_key(path: String) -> Result<WrappedCaKey, PloyzctlCliError> {
    let bytes = fs::read(&path).map_err(|error| PloyzctlCliError::InvalidValue {
        flag: "--recovery-key-file",
        message: format!("cannot read {path}: {error}"),
    })?;
    Ok(WrappedCaKey::new(bytes))
}

fn read_core_seeds(path: String) -> Result<WrappedCoreSeeds, PloyzctlCliError> {
    let bytes = fs::read(&path).map_err(|error| PloyzctlCliError::InvalidValue {
        flag: "--core-seeds-file",
        message: format!("cannot read {path}: {error}"),
    })?;
    Ok(WrappedCoreSeeds::new(bytes))
}

fn read_trusted_nats_ca(path: String) -> Result<NatsCaCertificatePem, PloyzctlCliError> {
    let contents = fs::read_to_string(&path).map_err(|error| PloyzctlCliError::InvalidValue {
        flag: "--trusted-nats-ca-file",
        message: format!("cannot read {path}: {error}"),
    })?;
    NatsCaCertificatePem::try_new(contents).map_err(|error| PloyzctlCliError::InvalidValue {
        flag: "--trusted-nats-ca-file",
        message: error.to_string(),
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
