//! The way back a Deploy refusal names for a Volume with no writer. The Machine that refused
//! reports only what it saw (`ployz_core::no_writer_observation`); this crate owns the
//! commands, so it adds them.

use ployz_core::{KnownCopy, NoWriterCase, Operator, no_writer_observation};

/// The refusal a plain Deploy of Volume `name` reads when no Server can mount its writer:
/// what was observed, then the way back `operator` can take. Moves are Cloud's: the CLI alone
/// is told that stopping one needs Cloud. With no Move under way, the CLI alone is told to
/// restore, and that `ployz volume rm` starts over empty.
#[must_use]
pub(crate) fn no_writer_message(name: &str, copies: &[KnownCopy], operator: Operator) -> String {
    let observed = no_writer_observation(name, copies);
    match (NoWriterCase::of(copies), operator) {
        (NoWriterCase::WriterElsewhere(_), _) => observed,
        (NoWriterCase::MidMove, Operator::Cloud) => {
            format!("{observed}; ployz volume release {name} stops it")
        }
        (NoWriterCase::MidMove, Operator::Cli) => {
            format!("{observed}; stopping it needs Ployz Cloud")
        }
        (NoWriterCase::NoCopy, Operator::Cloud) => format!(
            "{observed}. If a Server you removed still holds its data, add that Server back, then run ployz volume restore {name} --from <server>"
        ),
        (NoWriterCase::NoCopy, Operator::Cli) => format!(
            "{observed}. If a Server you removed still holds its data, add that Server back, then run ployz volume restore {name} --from <server>. {}",
            start_over(name)
        ),
        (NoWriterCase::OneCopy(only), Operator::Cloud) => format!(
            "{observed}. Make it the writer: ployz volume restore {name} --from {}",
            only.machine
        ),
        (NoWriterCase::OneCopy(only), Operator::Cli) => format!(
            "{observed}. Make it the writer: ployz volume restore {name} --from {}. {}",
            only.machine,
            start_over(name)
        ),
        // Copies carry no age, so the operator picks which one to keep. They carry no slot
        // role either, so no removal line is printed: run late, it could delete a live mirror.
        (NoWriterCase::SeveralCopies, Operator::Cloud) => format!(
            "{observed}. Restore needs a single copy: keep the newest, delete each other one from the Volume's page in Ployz Cloud, which shows whether it is a mirror or an old copy, then run ployz volume restore {name} --from <server you kept>"
        ),
        (NoWriterCase::SeveralCopies, Operator::Cli) => format!(
            "{observed}. Restore needs a single copy: keep the newest, remove each other Server from the Cluster with ployz server rm, then run ployz volume restore {name} --from <server you kept>. {}",
            start_over(name)
        ),
    }
}

/// The standalone way out that loses the data.
fn start_over(name: &str) -> String {
    format!(
        "To start over with an empty Volume instead, run ployz volume rm {name}, which deletes its data, then add it again and deploy"
    )
}

#[cfg(test)]
mod tests {
    use ployz_core::{CopyRole, MachineName};

    use super::*;

    fn name(machine: &str) -> MachineName {
        MachineName::parse(machine).unwrap()
    }

    fn copy(machine: &str, role: CopyRole) -> KnownCopy {
        KnownCopy {
            machine: name(machine),
            role,
        }
    }

    #[test]
    fn no_writer_message_restores_a_single_copy_and_mirrors_need_removing_first() {
        assert_eq!(
            no_writer_message("data", &[copy("fsn-2", CopyRole::Slot)], Operator::Cloud),
            "Volume data has no writer; fsn-2 holds its only copy. Make it the writer: ployz volume restore data --from fsn-2"
        );
        assert_eq!(
            no_writer_message(
                "data",
                &[
                    copy("fsn-2", CopyRole::Slot),
                    copy("hel-1", CopyRole::Slot),
                    copy("nbg-3", CopyRole::Slot),
                ],
                Operator::Cloud
            ),
            "Volume data has no writer; it is held as fsn-2 (copy), hel-1 (copy), nbg-3 (copy). Restore needs a single copy: keep the newest, delete each other one from the Volume's page in Ployz Cloud, which shows whether it is a mirror or an old copy, then run ployz volume restore data --from <server you kept>"
        );
        let several = no_writer_message(
            "data",
            &[copy("fsn-2", CopyRole::Slot), copy("hel-1", CopyRole::Slot)],
            Operator::Cloud,
        );
        assert!(!several.contains("mirror rm"), "{several}");
        assert_eq!(
            no_writer_message("data", &[], Operator::Cloud),
            "Volume data was deployed, but no Server in this Cluster holds a copy of it. If a Server you removed still holds its data, add that Server back, then run ployz volume restore data --from <server>"
        );
    }

    #[test]
    fn no_writer_message_without_cloud_offers_restore_and_starting_empty() {
        assert_eq!(
            no_writer_message("data", &[copy("fsn-2", CopyRole::Slot)], Operator::Cli),
            "Volume data has no writer; fsn-2 holds its only copy. Make it the writer: ployz volume restore data --from fsn-2. To start over with an empty Volume instead, run ployz volume rm data, which deletes its data, then add it again and deploy"
        );
        assert_eq!(
            no_writer_message("data", &[], Operator::Cli),
            "Volume data was deployed, but no Server in this Cluster holds a copy of it. If a Server you removed still holds its data, add that Server back, then run ployz volume restore data --from <server>. To start over with an empty Volume instead, run ployz volume rm data, which deletes its data, then add it again and deploy"
        );
        assert_eq!(
            no_writer_message(
                "data",
                &[copy("fsn-2", CopyRole::Slot), copy("hel-1", CopyRole::Slot)],
                Operator::Cli
            ),
            "Volume data has no writer; it is held as fsn-2 (copy), hel-1 (copy). Restore needs a single copy: keep the newest, remove each other Server from the Cluster with ployz server rm, then run ployz volume restore data --from <server you kept>. To start over with an empty Volume instead, run ployz volume rm data, which deletes its data, then add it again and deploy"
        );
        assert_eq!(
            no_writer_message("data", &[copy("hel-1", CopyRole::Switching)], Operator::Cli),
            "Volume data is mid-Move; it is held as hel-1 (switching); stopping it needs Ployz Cloud"
        );
        for copies in [
            vec![],
            vec![copy("fsn-2", CopyRole::Slot)],
            vec![copy("fsn-2", CopyRole::Slot), copy("hel-1", CopyRole::Slot)],
            vec![copy("hel-1", CopyRole::Switching)],
        ] {
            let message = no_writer_message("data", &copies, Operator::Cli);
            for cloud_only in ["volume release", "Volume's page"] {
                assert!(!message.contains(cloud_only), "{copies:?}: {message}");
            }
        }
        let writer_elsewhere = [copy("hel-1", CopyRole::Writer)];
        assert_eq!(
            no_writer_message("data", &writer_elsewhere, Operator::Cli),
            no_writer_message("data", &writer_elsewhere, Operator::Cloud)
        );
    }

    #[test]
    fn no_writer_message_sends_a_mid_move_volume_to_its_move_not_to_restore() {
        for copies in [
            vec![copy("hel-1", CopyRole::Switching)],
            vec![
                copy("fsn-2", CopyRole::Slot),
                copy("hel-1", CopyRole::Switching),
            ],
        ] {
            let message = no_writer_message("data", &copies, Operator::Cloud);
            assert!(
                message.starts_with("Volume data is mid-Move; it is held as "),
                "{message}"
            );
            assert!(message.contains("hel-1 (switching)"), "{message}");
            assert!(
                message.ends_with("; ployz volume release data stops it"),
                "{message}"
            );
            assert!(!message.contains("restore"), "{message}");
        }
    }

    #[test]
    fn no_writer_message_names_a_writer_elsewhere_without_a_restore_line() {
        assert_eq!(
            no_writer_message(
                "data",
                &[
                    copy("fsn-2", CopyRole::Slot),
                    copy("hel-1", CopyRole::Writer)
                ],
                Operator::Cloud
            ),
            "Volume data's writer is on hel-1, so this Server cannot mount it"
        );
    }

    #[test]
    fn no_writer_message_starts_with_what_the_machine_observed() {
        let cases = [
            vec![],
            vec![copy("fsn-2", CopyRole::Slot)],
            vec![copy("fsn-2", CopyRole::Slot), copy("hel-1", CopyRole::Slot)],
            vec![copy("hel-1", CopyRole::Switching)],
            vec![copy("hel-1", CopyRole::Writer)],
        ];
        for copies in cases {
            let observed = no_writer_observation("data", &copies);
            for operator in [Operator::Cloud, Operator::Cli] {
                assert!(
                    no_writer_message("data", &copies, operator).starts_with(&observed),
                    "{observed}"
                );
            }
        }
    }
}
