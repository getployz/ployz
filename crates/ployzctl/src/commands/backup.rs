use ployz_core::backup::{RestoreStep, single_core_restore_contract};
use ployz_core::ids::OperationId;
use ployz_core::ops::OperationIdempotencyKey;
use ployz_sdk_types::{AcceptedOperation, BackupCreateRequest};

use crate::commands::{ArgCursor, PloyzctlCliError, invalid_value, required, set_once};

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
    let mut operation_id = None;
    let mut idempotency_key = None;
    let mut args = ArgCursor::new(args);

    while !args.is_empty() {
        if let Some(value) = args.take_value("--operation")? {
            let parsed =
                OperationId::try_new(value).map_err(|error| invalid_value("--operation", error))?;
            set_once(&mut operation_id, parsed, "--operation")?;
            continue;
        }
        if let Some(value) = args.take_value("--idempotency-key")? {
            let parsed = OperationIdempotencyKey::try_new(value)
                .map_err(|error| invalid_value("--idempotency-key", error))?;
            set_once(&mut idempotency_key, parsed, "--idempotency-key")?;
            continue;
        }
        return Err(args.unexpected());
    }

    Ok(BackupCreateCommand {
        operation_id: required(operation_id, "--operation")?,
        idempotency_key: required(idempotency_key, "--idempotency-key")?,
    })
}

pub fn parse_backup_restore_command(
    args: &[String],
) -> Result<BackupRestorePlanCommand, PloyzctlCliError> {
    let mut plan = false;
    let mut args = ArgCursor::new(args);

    while !args.is_empty() {
        if args.take_flag("--plan") {
            if plan {
                return Err(PloyzctlCliError::DuplicateArgument { flag: "--plan" });
            }
            plan = true;
            continue;
        }
        return Err(args.unexpected());
    }

    if !plan {
        return Err(PloyzctlCliError::MissingRequiredArgument { flag: "--plan" });
    }

    Ok(BackupRestorePlanCommand)
}

const fn restore_step_name(step: RestoreStep) -> &'static str {
    match step {
        RestoreStep::RecreateControlPlaneAuthority => "recreate_control_plane_authority",
        RestoreStep::RestoreJetStreamState => "restore_jet_stream_state",
        RestoreStep::WaitForNodeReconnects => "wait_for_node_reconnects",
        RestoreStep::RebuildObservationsFromReality => "rebuild_observations_from_reality",
    }
}
