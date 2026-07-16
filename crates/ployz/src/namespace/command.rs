use clap::Args;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_sdk_types::{NamespaceRemoveRequest, VolumeListResult};

use crate::commands::{PloyzctlCliError, invalid_value};
use crate::execution_support::generate_client_namespace_remove_id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRemoveCommand {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub force: bool,
    pub detach: bool,
}

impl NamespaceRemoveCommand {
    #[must_use]
    pub fn into_request(self) -> NamespaceRemoveRequest {
        NamespaceRemoveRequest {
            operation_id: self.operation_id,
            namespace_id: self.namespace_id,
        }
    }
}

pub(crate) fn namespace_remove_command(
    parsed: NamespaceRemoveCli,
) -> Result<NamespaceRemoveCommand, PloyzctlCliError> {
    let namespace_id = NamespaceId::try_new(parsed.namespace)
        .map_err(|error| invalid_value("<namespace>", error))?;
    let operation_id = generate_client_namespace_remove_id(&namespace_id)
        .map_err(|error| invalid_value("<namespace>", error))?
        .operation_id;
    Ok(NamespaceRemoveCommand {
        operation_id,
        namespace_id,
        force: parsed.force,
        detach: parsed.detach,
    })
}

#[derive(Debug, Args)]
pub(crate) struct NamespaceRemoveCli {
    namespace: String,
    #[arg(long)]
    force: bool,
    #[arg(long)]
    detach: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRemoveConfirmation {
    pub namespace_id: NamespaceId,
    pub volume_backed_services: Vec<String>,
}

impl NamespaceRemoveConfirmation {
    #[must_use]
    pub fn from_volume_list(namespace_id: NamespaceId, result: VolumeListResult) -> Self {
        let mut volume_backed_services = result
            .volumes
            .into_iter()
            .filter(|volume| volume.namespace_id == namespace_id)
            .flat_map(|volume| volume.referencing_services)
            .map(|service| service.as_str().to_owned())
            .collect::<Vec<_>>();
        volume_backed_services.sort_unstable();
        volume_backed_services.dedup();
        Self {
            namespace_id,
            volume_backed_services,
        }
    }

    #[must_use]
    pub fn prompt(&self) -> String {
        let services = if self.volume_backed_services.is_empty() {
            "none recorded".to_owned()
        } else {
            self.volume_backed_services.join(", ")
        };
        format!(
            "This removes service containers and Route Bindings for namespace {}.\nVolume data is preserved. Volume-backed services: {}.\nType {} to continue: ",
            self.namespace_id.as_str(),
            services,
            self.namespace_id.as_str()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::NamespaceRemoveConfirmation;
    use ployz_core::deploy::VolumeName;
    use ployz_core::ids::{MachineId, NamespaceId, ServiceId};
    use ployz_core::intent::VolumeKind;
    use ployz_sdk_types::{VolumeListResult, VolumeSnapshot, VolumeStatus, VolumeTestimony};

    fn snapshot(namespace: &str, volume: &str, services: &[&str]) -> VolumeSnapshot {
        VolumeSnapshot {
            namespace_id: NamespaceId::try_new(namespace).expect("valid namespace"),
            volume_name: VolumeName::try_new(volume).expect("valid volume"),
            machine_id: MachineId::try_new("machine-a").expect("valid machine"),
            kind: VolumeKind::Plain,
            referencing_services: services
                .iter()
                .map(|service| ServiceId::try_new(*service).expect("valid service"))
                .collect(),
            testimony: VolumeTestimony::NoAnswer,
            status: VolumeStatus::InUse,
        }
    }

    #[test]
    fn namespace_remove_confirmation_uses_exact_namespace_and_sorted_deduplicated_services() {
        let namespace_id = NamespaceId::try_new("prod").expect("valid namespace");
        let confirmation = NamespaceRemoveConfirmation::from_volume_list(
            namespace_id,
            VolumeListResult {
                volumes: vec![
                    snapshot("other", "data", &["ignored"]),
                    snapshot("prod", "cache", &["worker", "api"]),
                    snapshot("prod", "data", &["api", "web"]),
                ],
            },
        );

        assert_eq!(
            confirmation.volume_backed_services,
            ["api", "web", "worker"]
        );
        assert_eq!(
            confirmation.prompt(),
            "This removes service containers and Route Bindings for namespace prod.\nVolume data is preserved. Volume-backed services: api, web, worker.\nType prod to continue: "
        );
    }
}
