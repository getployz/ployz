use clap::{ArgAction, Args, ValueEnum};
use ployz_core::backup::{
    BackupRestoreSource, BackupTarget, RestoreStep, S3AddressingStyle, S3BackupRestoreSource,
    S3BackupTarget, single_core_restore_contract,
};
use ployz_core::ids::OperationId;
use ployz_sdk_types::{AcceptedOperation, BackupCreateRequest};

use crate::commands::{PloyzctlCliError, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupCreateCommand {
    pub operation_id: OperationId,
    pub target: BackupTarget,
}

impl BackupCreateCommand {
    #[must_use]
    pub fn into_request(self) -> BackupCreateRequest {
        BackupCreateRequest {
            operation_id: self.operation_id,
            target: self.target,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupCreateOutput {
    pub accepted: AcceptedOperation,
}

impl BackupCreateOutput {
    #[must_use]
    pub const fn from_accepted(accepted: AcceptedOperation) -> Self {
        Self { accepted }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nwatch ployzctl ops watch {}\n",
            self.accepted.operation_id.as_str(),
            self.accepted.operation_id.as_str()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRestorePlanCommand {
    pub source: BackupRestoreSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRestorePlanOutput {
    pub lines: Vec<String>,
}

impl BackupRestorePlanOutput {
    #[must_use]
    pub fn single_core() -> Self {
        Self {
            lines: single_core_restore_contract()
                .map(|step| restore_step_name(step).to_owned())
                .collect(),
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut output = String::from("single-core restore contract\n");
        for line in &self.lines {
            output.push_str("- ");
            output.push_str(line);
            output.push('\n');
        }
        output.push_str("machine observations repopulate after agents reconnect\n");
        output
    }
}

pub(crate) fn backup_create_command(
    parsed: BackupCreateCli,
) -> Result<BackupCreateCommand, PloyzctlCliError> {
    Ok(BackupCreateCommand {
        operation_id: OperationId::try_new(parsed.operation)
            .map_err(|error| invalid_value("--operation", error))?,
        target: BackupTarget::s3(S3BackupTarget::new(
            non_empty_string("--s3-bucket", parsed.s3_bucket)?,
            non_empty_string("--s3-prefix", parsed.s3_prefix)?,
            non_empty_string("--s3-region", parsed.s3_region)?,
            optional_non_empty_string("--s3-endpoint-url", parsed.s3_endpoint_url)?,
            parsed.s3_addressing_style.into(),
        )),
    })
}

#[derive(Debug, Args)]
pub(crate) struct BackupCreateCli {
    #[arg(long)]
    operation: String,
    #[arg(long)]
    s3_bucket: String,
    #[arg(long)]
    s3_prefix: String,
    #[arg(long)]
    s3_region: String,
    #[arg(long)]
    s3_endpoint_url: Option<String>,
    #[arg(long, value_enum, default_value_t = BackupS3AddressingStyleCli::VirtualHosted)]
    s3_addressing_style: BackupS3AddressingStyleCli,
}

pub(crate) fn backup_restore_command(
    parsed: BackupRestoreCli,
) -> Result<BackupRestorePlanCommand, PloyzctlCliError> {
    Ok(BackupRestorePlanCommand {
        source: BackupRestoreSource::s3(S3BackupRestoreSource::new(
            non_empty_string("--s3-bucket", parsed.s3_bucket)?,
            non_empty_string("--s3-manifest-key", parsed.s3_manifest_key)?,
            non_empty_string("--s3-region", parsed.s3_region)?,
            optional_non_empty_string("--s3-endpoint-url", parsed.s3_endpoint_url)?,
            parsed.s3_addressing_style.into(),
        )),
    })
}

#[derive(Debug, Args)]
pub(crate) struct BackupRestoreCli {
    #[arg(long, action = ArgAction::SetTrue, required = true)]
    plan: bool,
    #[arg(long)]
    s3_bucket: String,
    #[arg(long)]
    s3_manifest_key: String,
    #[arg(long)]
    s3_region: String,
    #[arg(long)]
    s3_endpoint_url: Option<String>,
    #[arg(long, value_enum, default_value_t = BackupS3AddressingStyleCli::VirtualHosted)]
    s3_addressing_style: BackupS3AddressingStyleCli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum BackupS3AddressingStyleCli {
    VirtualHosted,
    Path,
}

impl From<BackupS3AddressingStyleCli> for S3AddressingStyle {
    fn from(value: BackupS3AddressingStyleCli) -> Self {
        match value {
            BackupS3AddressingStyleCli::VirtualHosted => Self::VirtualHosted,
            BackupS3AddressingStyleCli::Path => Self::Path,
        }
    }
}

fn non_empty_string(flag: &'static str, value: String) -> Result<String, PloyzctlCliError> {
    if value.trim().is_empty() {
        return Err(invalid_value(flag, "must not be empty"));
    }
    Ok(value)
}

fn optional_non_empty_string(
    flag: &'static str,
    value: Option<String>,
) -> Result<Option<String>, PloyzctlCliError> {
    value.map(|value| non_empty_string(flag, value)).transpose()
}

const fn restore_step_name(step: RestoreStep) -> &'static str {
    match step {
        RestoreStep::RecreateControlPlaneAuthority => "recreate_control_plane_authority",
        RestoreStep::RestoreJetStreamState => "restore_jet_stream_state",
        RestoreStep::WaitForMachineReconnects => "wait_for_machine_reconnects",
        RestoreStep::RebuildObservationsFromReality => "rebuild_observations_from_reality",
    }
}
