//! Stable human output for founding outcomes and refusals.

use ployz_core::corrosion::{MachineStorageSelection, StorageMode};
use ployz_core::founding::{FoundingRefusal, FoundingResult, PLAIN_STORAGE_FORFEIT};

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
        "{outcome} cluster {cluster_name} on machine {machine_name}.\n{storage_line}Next: ployz token create\n"
    )
}

#[must_use]
pub fn refusal_summary(refusal: &FoundingRefusal) -> String {
    match refusal {
        FoundingRefusal::InvalidRequest { reason } => {
            format!("Init refused: {reason}\n")
        }
        FoundingRefusal::ForeignState { repair_command, .. } => format!(
            "Init refused: this machine belongs to another cluster.\nRepair on the machine: {}\n",
            repair_command.as_str()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::corrosion::{MachineStorageSelectionReason, StorageMode};
    use ployz_core::founding::FoundingRepairCommand;
    use ployz_core::ids::ClusterId;

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
                "Found cluster lab on machine ares.\nStorage: plain — {PLAIN_STORAGE_FORFEIT}\nNext: ployz token create\n"
            )
        );
    }

    #[test]
    fn foreign_state_names_reset_without_authorizing_it() {
        let refusal = FoundingRefusal::ForeignState {
            requested_cluster_id: ClusterId::generate(),
            found_cluster_id: ClusterId::generate(),
            repair_command: FoundingRepairCommand::ResetMachine,
        };
        let output = refusal_summary(&refusal);
        assert_eq!(
            output,
            "Init refused: this machine belongs to another cluster.\nRepair on the machine: ployz machine reset\n"
        );
        assert!(!output.contains("--force"));
    }
}
