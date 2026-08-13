//! Stable human output for founding outcomes and refusals.

use ployz_core::corrosion::{MachineStorageSelection, StorageMode};
use ployz_core::founding::{FoundingRefusal, FoundingResult, PLAIN_STORAGE_FORFEIT};

use crate::init::on_host::OnHostSuccess;
use crate::mesh::context::{OperatorContextError, SshContextHandoff};

#[must_use]
pub fn success_summary(
    result: FoundingResult,
    cluster_name: &str,
    machine_name: &str,
    storage: &MachineStorageSelection,
) -> String {
    let outcome = match result {
        FoundingResult::Found => "Found",
        FoundingResult::Resumed => "Resumed",
        FoundingResult::NoOp => "Already founded",
    };
    let storage_line = match storage.mode {
        StorageMode::Plain => format!("Storage: plain — {PLAIN_STORAGE_FORFEIT}\n"),
        StorageMode::Zfs => "Storage: zfs\n".to_owned(),
    };
    format!(
        "{outcome} cluster {cluster_name} on machine {machine_name}.\n{storage_line}Next: ployz token create <name>\n"
    )
}

#[must_use]
pub fn refusal_summary(refusal: &FoundingRefusal) -> String {
    match refusal {
        FoundingRefusal::InvalidRequest { reason } => {
            format!("Init refused: {reason}\n")
        }
        FoundingRefusal::IncompleteDoorMaterial { repair_command } => format!(
            "Init refused: cluster door TLS material is incomplete.\nRepair on the machine: {}\n",
            repair_command.as_str()
        ),
        FoundingRefusal::ForeignState { repair_command, .. } => format!(
            "Init refused: this machine belongs to another cluster.\nRepair on the machine: {}\n",
            repair_command.as_str()
        ),
    }
}

pub fn ssh_context_handoff(success: &OnHostSuccess) -> Result<String, OperatorContextError> {
    SshContextHandoff {
        cluster_id: success.cluster_id.clone(),
        provider: success.provider,
        machine_transport: success.machine_transport.clone(),
    }
    .encode_handoff()
}

#[must_use]
pub fn context_handoff_unavailable(target: &str) -> String {
    format!(
        "The remote ployz is too old to save this laptop's cluster context. Upgrade ployz on {target}, then run:\n  ployz init {target}\n  ployz machine ls --target {target}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::corrosion::{MachineStorageSelectionReason, StorageMode};
    use ployz_core::founding::FoundingRepairCommand;
    use ployz_core::ids::ClusterName;

    #[test]
    fn all_success_outcomes_name_the_next_primitive() {
        for (result, prefix) in [
            (FoundingResult::Found, "Found"),
            (FoundingResult::Resumed, "Resumed"),
            (FoundingResult::NoOp, "Already founded"),
        ] {
            let output = success_summary(
                result,
                "lab",
                "ares",
                &MachineStorageSelection {
                    mode: StorageMode::Plain,
                    reason: MachineStorageSelectionReason::Default,
                },
            );
            assert!(output.starts_with(prefix));
            assert!(output.contains("Next: ployz token create"));
            assert!(!output.contains("namespace"));
            assert!(output.contains(PLAIN_STORAGE_FORFEIT));
        }
    }

    #[test]
    fn found_plain_summary_is_stable() {
        assert_eq!(
            success_summary(
                FoundingResult::Found,
                "lab",
                "ares",
                &MachineStorageSelection {
                    mode: StorageMode::Plain,
                    reason: MachineStorageSelectionReason::Flag,
                },
            ),
            format!(
                "Found cluster lab on machine ares.\nStorage: plain — {PLAIN_STORAGE_FORFEIT}\nNext: ployz token create <name>\n"
            )
        );
    }

    #[test]
    fn foreign_state_names_reset_without_authorizing_it() {
        let refusal = FoundingRefusal::ForeignState {
            requested_cluster_id: ClusterName::try_new("requested").expect("cluster"),
            found_cluster_id: ClusterName::try_new("found").expect("cluster"),
            repair_command: FoundingRepairCommand::ResetMachine,
        };
        let output = refusal_summary(&refusal);
        assert_eq!(
            output,
            "Init refused: this machine belongs to another cluster.\nRepair on the machine: ployz machine reset\n"
        );
        assert!(!output.contains("--force"));
    }

    #[test]
    fn incomplete_door_material_names_reset_without_silently_repairing() {
        let output = refusal_summary(&FoundingRefusal::IncompleteDoorMaterial {
            repair_command: FoundingRepairCommand::ResetMachine,
        });

        assert_eq!(
            output,
            "Init refused: cluster door TLS material is incomplete.\nRepair on the machine: ployz machine reset\n"
        );
    }

    #[test]
    fn release_skew_names_the_exact_recovery_pair() {
        assert_eq!(
            context_handoff_unavailable("root@machine.example"),
            "The remote ployz is too old to save this laptop's cluster context. Upgrade ployz on root@machine.example, then run:\n  ployz init root@machine.example\n  ployz machine ls --target root@machine.example"
        );
    }
}
