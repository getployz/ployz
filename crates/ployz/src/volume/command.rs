use clap::Args;
use ployz_core::deploy::{VolumeMaxSizeBytes, VolumeName, VolumeSpec};
use ployz_core::ids::{MachineId, NamespaceId, OperationId};
use ployz_core::intent::VolumeKind;
use ployz_sdk_types::{
    VolumeListRequest, VolumeListResult, VolumeRemoveRequest, VolumeSnapshot, VolumeStatus,
    VolumeTestimony,
};

use crate::commands::{PloyzctlCliError, invalid_value};
use crate::execution_support::{
    generate_client_volume_create_id, generate_client_volume_remove_id,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeCreateCommand {
    pub request: ployz_sdk_types::VolumeCreateRequest,
}

impl VolumeCreateCommand {
    #[must_use]
    pub fn into_request(self) -> ployz_sdk_types::VolumeCreateRequest {
        self.request
    }
}

pub(crate) fn volume_create_command(
    parsed: VolumeCreateCli,
) -> Result<VolumeCreateCommand, PloyzctlCliError> {
    let namespace_id = NamespaceId::try_new(parsed.namespace)
        .map_err(|error| invalid_value("<namespace>", error))?;
    let volume_name =
        VolumeName::try_new(parsed.volume).map_err(|error| invalid_value("<volume>", error))?;
    let machine_id =
        MachineId::try_new(parsed.machine).map_err(|error| invalid_value("--machine", error))?;
    let spec = match parsed.max_size {
        None => VolumeSpec::Plain,
        Some(max_size) => {
            let bytes = crate::deploy::compose::parse_byte_quantity(&max_size, "volume max size")
                .map_err(|error| invalid_value("--max-size", error))?;
            VolumeSpec::Provisioned {
                max_size_bytes: VolumeMaxSizeBytes::try_new(bytes)
                    .map_err(|error| invalid_value("--max-size", error))?,
            }
        }
    };
    let operation_id = generate_client_volume_create_id(&namespace_id, &volume_name)
        .map_err(|error| invalid_value("<volume>", error))?
        .operation_id;
    Ok(VolumeCreateCommand {
        request: ployz_sdk_types::VolumeCreateRequest {
            operation_id,
            namespace_id,
            volume_name,
            machine_id,
            spec,
        },
    })
}

#[derive(Debug, Args)]
pub(crate) struct VolumeCreateCli {
    namespace: String,
    volume: String,
    #[arg(long)]
    machine: String,
    #[arg(long)]
    max_size: Option<String>,
}

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
    pub volume: VolumeSnapshot,
}

impl VolumeRemoveConfirmation {
    #[must_use]
    pub fn prompt(&self) -> String {
        format!(
            "DATA LOSS WARNING: this permanently destroys volume {}/{} and all of its data.\nKind: {}.\nDataset: {}.\nEvidence: {}.\nType {}/{} to continue: ",
            self.volume.namespace_id.as_str(),
            self.volume.volume_name.as_str(),
            render_kind(&self.volume.kind),
            render_dataset(&self.volume.kind),
            render_confirmation_testimony(&self.volume),
            self.volume.namespace_id.as_str(),
            self.volume.volume_name.as_str(),
        )
    }

    #[must_use]
    pub fn confirmation(&self) -> String {
        format!(
            "{}/{}",
            self.volume.namespace_id.as_str(),
            self.volume.volume_name.as_str()
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
        let mut volumes = result.volumes;
        volumes.sort_by(|left, right| {
            (&left.namespace_id, &left.volume_name).cmp(&(&right.namespace_id, &right.volume_name))
        });
        Self { volumes }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut rows = vec![
            "NAMESPACE\tVOLUME\tMACHINE\tKIND\tDATASET\tREFERENCING_SERVICES\tUSED_BYTES\tLAST_WRITE_UNIX_SECONDS\tTESTIMONY\tSTATUS"
                .to_owned(),
        ];
        rows.extend(self.volumes.iter().map(|volume| {
            let status = match volume.status {
                VolumeStatus::InUse => "in-use",
                VolumeStatus::Orphaned => "ORPHANED",
            };
            let referencing_services = if volume.referencing_services.is_empty() {
                "-".to_owned()
            } else {
                volume
                    .referencing_services
                    .iter()
                    .map(|service| service.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let (used_bytes, last_write, testimony) = render_list_testimony(volume);
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                volume.namespace_id.as_str(),
                volume.volume_name.as_str(),
                volume.machine_id.as_str(),
                render_kind(&volume.kind),
                render_dataset(&volume.kind),
                referencing_services,
                used_bytes,
                last_write,
                testimony,
                status
            )
        }));
        rows.join("\n") + "\n"
    }
}

fn render_kind(kind: &VolumeKind) -> &'static str {
    match kind {
        VolumeKind::Plain => "plain",
        VolumeKind::Provisioned { .. } => "provisioned",
    }
}

fn render_dataset(kind: &VolumeKind) -> &str {
    match kind {
        VolumeKind::Plain => "-",
        VolumeKind::Provisioned { dataset, .. } => dataset.as_str(),
    }
}

fn render_list_testimony(volume: &VolumeSnapshot) -> (String, String, String) {
    match volume.testimony {
        VolumeTestimony::Available {
            used_bytes,
            last_write_unix_seconds,
        } => (
            used_bytes.to_string(),
            last_write_unix_seconds.to_string(),
            "available".to_owned(),
        ),
        VolumeTestimony::Unavailable => (
            "unknown".to_owned(),
            "unknown".to_owned(),
            "unavailable".to_owned(),
        ),
        VolumeTestimony::NoAnswer => (
            "unknown".to_owned(),
            "unknown".to_owned(),
            format!(
                "no-answer:machine-unreachable:{}",
                volume.machine_id.as_str()
            ),
        ),
    }
}

fn render_confirmation_testimony(volume: &VolumeSnapshot) -> String {
    match volume.testimony {
        VolumeTestimony::Available {
            used_bytes,
            last_write_unix_seconds,
        } => format!(
            "available from pinned machine {}; used bytes {}; last write unix seconds {}",
            volume.machine_id.as_str(),
            used_bytes,
            last_write_unix_seconds
        ),
        VolumeTestimony::Unavailable => format!(
            "pinned machine {} answered; volume facts unavailable; used bytes unknown; last write unix seconds unknown",
            volume.machine_id.as_str()
        ),
        VolumeTestimony::NoAnswer => format!(
            "no answer; pinned machine {} unreachable; used bytes unknown; last write unix seconds unknown",
            volume.machine_id.as_str()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{VolumeListOutput, VolumeRemoveConfirmation};
    use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes, VolumeName, ZfsPoolName};
    use ployz_core::ids::{MachineId, NamespaceId, ServiceId};
    use ployz_core::intent::VolumeKind;
    use ployz_sdk_types::{VolumeListResult, VolumeSnapshot, VolumeStatus, VolumeTestimony};

    fn snapshot(
        namespace: &str,
        volume: &str,
        machine: &str,
        kind: VolumeKind,
        services: &[&str],
        testimony: VolumeTestimony,
        status: VolumeStatus,
    ) -> VolumeSnapshot {
        VolumeSnapshot {
            namespace_id: NamespaceId::try_new(namespace).expect("valid namespace"),
            volume_name: VolumeName::try_new(volume).expect("valid volume"),
            machine_id: MachineId::try_new(machine).expect("valid machine"),
            kind,
            referencing_services: services
                .iter()
                .map(|service| ServiceId::try_new(*service).expect("valid service"))
                .collect(),
            testimony,
            status,
        }
    }

    fn provisioned(namespace: &str, volume: &str) -> VolumeKind {
        let namespace_id = NamespaceId::try_new(namespace).expect("valid namespace");
        let volume_name = VolumeName::try_new(volume).expect("valid volume");
        VolumeKind::Provisioned {
            dataset: DatasetName::for_volume(
                &ZfsPoolName::try_new("tank").expect("valid pool"),
                &namespace_id,
                &volume_name,
            )
            .expect("valid dataset"),
            max_size_bytes: VolumeMaxSizeBytes::try_new(8_192).expect("valid maximum"),
        }
    }

    #[test]
    fn volume_list_renders_stable_detailed_testimony_without_inventing_zeroes() {
        let output = VolumeListOutput::from_result(VolumeListResult {
            volumes: vec![
                snapshot(
                    "prod",
                    "silent",
                    "machine-c",
                    VolumeKind::Plain,
                    &[],
                    VolumeTestimony::NoAnswer,
                    VolumeStatus::Orphaned,
                ),
                snapshot(
                    "prod",
                    "data",
                    "machine-a",
                    provisioned("prod", "data"),
                    &["api", "worker"],
                    VolumeTestimony::Available {
                        used_bytes: 4_096,
                        last_write_unix_seconds: 1_700_000_000,
                    },
                    VolumeStatus::InUse,
                ),
                snapshot(
                    "prod",
                    "offline",
                    "machine-b",
                    VolumeKind::Plain,
                    &["api"],
                    VolumeTestimony::Unavailable,
                    VolumeStatus::InUse,
                ),
            ],
        });

        assert_eq!(
            output.render(),
            "NAMESPACE\tVOLUME\tMACHINE\tKIND\tDATASET\tREFERENCING_SERVICES\tUSED_BYTES\tLAST_WRITE_UNIX_SECONDS\tTESTIMONY\tSTATUS\nprod\tdata\tmachine-a\tprovisioned\ttank/ployz/volumes/ployz-n4-prod-v4-data\tapi,worker\t4096\t1700000000\tavailable\tin-use\nprod\toffline\tmachine-b\tplain\t-\tapi\tunknown\tunknown\tunavailable\tin-use\nprod\tsilent\tmachine-c\tplain\t-\t-\tunknown\tunknown\tno-answer:machine-unreachable:machine-c\tORPHANED\n"
        );
    }

    #[test]
    fn volume_remove_prompt_preserves_warning_and_type_lines_and_adds_available_evidence() {
        let confirmation = VolumeRemoveConfirmation {
            volume: snapshot(
                "prod",
                "data",
                "machine-a",
                provisioned("prod", "data"),
                &[],
                VolumeTestimony::Available {
                    used_bytes: 4_096,
                    last_write_unix_seconds: 1_700_000_000,
                },
                VolumeStatus::Orphaned,
            ),
        };

        assert_eq!(
            confirmation.prompt(),
            "DATA LOSS WARNING: this permanently destroys volume prod/data and all of its data.\nKind: provisioned.\nDataset: tank/ployz/volumes/ployz-n4-prod-v4-data.\nEvidence: available from pinned machine machine-a; used bytes 4096; last write unix seconds 1700000000.\nType prod/data to continue: "
        );
        assert_eq!(confirmation.confirmation(), "prod/data");
    }

    #[test]
    fn volume_remove_prompt_names_unreachable_pin_and_keeps_unknowns_honest() {
        let confirmation = VolumeRemoveConfirmation {
            volume: snapshot(
                "prod",
                "data",
                "machine-a",
                VolumeKind::Plain,
                &[],
                VolumeTestimony::NoAnswer,
                VolumeStatus::Orphaned,
            ),
        };

        let prompt = confirmation.prompt();
        assert!(prompt.contains("Dataset: -."));
        assert!(prompt.contains(
            "Evidence: no answer; pinned machine machine-a unreachable; used bytes unknown; last write unix seconds unknown."
        ));
        assert!(!prompt.contains("used bytes 0"));
        assert!(!prompt.contains("last write unix seconds 0"));
    }

    #[test]
    fn volume_remove_prompt_distinguishes_unavailable_facts_from_an_unreachable_machine() {
        let confirmation = VolumeRemoveConfirmation {
            volume: snapshot(
                "prod",
                "data",
                "machine-a",
                VolumeKind::Plain,
                &[],
                VolumeTestimony::Unavailable,
                VolumeStatus::Orphaned,
            ),
        };

        let prompt = confirmation.prompt();
        assert!(prompt.contains(
            "Evidence: pinned machine machine-a answered; volume facts unavailable; used bytes unknown; last write unix seconds unknown."
        ));
        assert!(!prompt.contains("unreachable"));
        assert!(!prompt.contains("no answer"));
    }
}
