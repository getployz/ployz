use clap::{ArgAction, Parser};
use ployz_core::backup::{RestoreStep, single_core_restore_contract};
use ployz_core::ids::OperationId;
use ployz_core::ops::OperationIdempotencyKey;
use ployz_sdk_types::{AcceptedOperation, BackupCreateRequest};

use crate::commands::{PloyzctlCliError, clap_error, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupCreateCommand {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
}

impl BackupCreateCommand {
    #[must_use]
    pub fn into_request(self) -> BackupCreateRequest {
        BackupCreateRequest {
            operation_id: self.operation_id,
            idempotency_key: self.idempotency_key,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupRestorePlanCommand;

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
        output.push_str("node observations repopulate after agents reconnect\n");
        output
    }
}

pub fn parse_backup_create_command(
    args: &[String],
) -> Result<BackupCreateCommand, PloyzctlCliError> {
    let parsed = BackupCreateCli::try_parse_from(
        std::iter::once("backup create".to_owned()).chain(args.iter().cloned()),
    )
    .map_err(clap_error)?;

    Ok(BackupCreateCommand {
        operation_id: OperationId::try_new(parsed.operation)
            .map_err(|error| invalid_value("--operation", error))?,
        idempotency_key: OperationIdempotencyKey::try_new(parsed.idempotency_key)
            .map_err(|error| invalid_value("--idempotency-key", error))?,
    })
}

#[derive(Debug, Parser)]
#[command(name = "backup create")]
struct BackupCreateCli {
    #[arg(long)]
    operation: String,
    #[arg(long)]
    idempotency_key: String,
}

pub fn parse_backup_restore_command(
    args: &[String],
) -> Result<BackupRestorePlanCommand, PloyzctlCliError> {
    BackupRestoreCli::try_parse_from(
        std::iter::once("backup restore".to_owned()).chain(args.iter().cloned()),
    )
    .map_err(clap_error)?;

    Ok(BackupRestorePlanCommand)
}

#[derive(Debug, Parser)]
#[command(name = "backup restore")]
struct BackupRestoreCli {
    #[arg(long, action = ArgAction::SetTrue, required = true)]
    plan: bool,
}

const fn restore_step_name(step: RestoreStep) -> &'static str {
    match step {
        RestoreStep::RecreateControlPlaneAuthority => "recreate_control_plane_authority",
        RestoreStep::RestoreJetStreamState => "restore_jet_stream_state",
        RestoreStep::WaitForNodeReconnects => "wait_for_node_reconnects",
        RestoreStep::RebuildObservationsFromReality => "rebuild_observations_from_reality",
    }
}
