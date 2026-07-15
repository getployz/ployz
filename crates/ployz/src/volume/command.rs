use clap::Args;
use ployz_core::deploy::VolumeName;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_sdk_types::{
    VolumeListRequest, VolumeListResult, VolumeRemoveRequest, VolumeSnapshot, VolumeStatus,
};

use crate::commands::{PloyzctlCliError, invalid_value};
use crate::execution_support::generate_client_volume_remove_id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeListCommand;

impl VolumeListCommand {
    #[must_use]
    pub const fn into_request(self) -> VolumeListRequest {
        VolumeListRequest {}
    }
}

pub(crate) fn volume_list_command(_: VolumeListCli) -> VolumeListCommand {
    VolumeListCommand
}

#[derive(Debug, Args)]
pub(crate) struct VolumeListCli {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRemoveCommand {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
    pub force: bool,
    pub detach: bool,
}

impl VolumeRemoveCommand {
    #[must_use]
    pub fn into_request(self) -> VolumeRemoveRequest {
        VolumeRemoveRequest {
            operation_id: self.operation_id,
            namespace_id: self.namespace_id,
            volume_name: self.volume_name,
        }
    }
}

pub(crate) fn volume_remove_command(
    parsed: VolumeRemoveCli,
) -> Result<VolumeRemoveCommand, PloyzctlCliError> {
    let namespace_id = NamespaceId::try_new(parsed.namespace)
        .map_err(|error| invalid_value("<namespace>", error))?;
    let volume_name =
        VolumeName::try_new(parsed.volume).map_err(|error| invalid_value("<volume>", error))?;
    let operation_id = generate_client_volume_remove_id(&namespace_id, &volume_name)
        .map_err(|error| invalid_value("<volume>", error))?
        .operation_id;
    Ok(VolumeRemoveCommand {
        operation_id,
        namespace_id,
        volume_name,
        force: parsed.force,
        detach: parsed.detach,
    })
}

#[derive(Debug, Args)]
pub(crate) struct VolumeRemoveCli {
    namespace: String,
    volume: String,
    #[arg(long, alias = "yes")]
    force: bool,
    #[arg(long)]
    detach: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRemoveConfirmation {
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
}

impl VolumeRemoveConfirmation {
    #[must_use]
    pub fn prompt(&self) -> String {
        format!(
            "DATA LOSS WARNING: this permanently destroys volume {}/{} and all of its data.\nType {}/{} to continue: ",
            self.namespace_id.as_str(),
            self.volume_name.as_str(),
            self.namespace_id.as_str(),
            self.volume_name.as_str(),
        )
    }

    #[must_use]
    pub fn confirmation(&self) -> String {
        format!(
            "{}/{}",
            self.namespace_id.as_str(),
            self.volume_name.as_str()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeListOutput {
    pub volumes: Vec<VolumeSnapshot>,
}

impl VolumeListOutput {
    #[must_use]
    pub fn from_result(result: VolumeListResult) -> Self {
        Self {
            volumes: result.volumes,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut rows = vec!["NAMESPACE\tVOLUME\tMACHINE\tSTATUS".to_owned()];
        rows.extend(self.volumes.iter().map(|volume| {
            let status = match volume.status {
                VolumeStatus::InUse => "in-use",
                VolumeStatus::Orphaned => "ORPHANED",
            };
            format!(
                "{}\t{}\t{}\t{}",
                volume.namespace_id.as_str(),
                volume.volume_name.as_str(),
                volume.machine_id.as_str(),
                status
            )
        }));
        rows.join("\n") + "\n"
    }
}
