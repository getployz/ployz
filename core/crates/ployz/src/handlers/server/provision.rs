//! What `server add` and `server init` read from the command line to provision a Machine.

use clap::ArgMatches;
use ployz_core::{Hint, MachineRelease, StoragePreparation};

use crate::failure::Failure;
use crate::provisioning::{self, ProvisionError};
use crate::ui;

/// Resolve Machine storage preparation once before provisioning or enrollment.
pub(crate) fn resolve_storage(matches: &ArgMatches) -> Result<StoragePreparation, Failure> {
    let storage = match matches.get_one::<StoragePreparation>("storage").cloned() {
        Some(storage) => storage,
        None if matches.get_flag("yes") || matches.get_flag("no-install") => {
            StoragePreparation::None
        }
        None => {
            let index = ui::select(
                "Storage preparation",
                &["zfs (recommended)", "none (Docker only, not recommended)"],
                0,
                || {
                    Failure::usage(
                        "Choosing storage needs a terminal; pass --storage zfs or --storage none.",
                    )
                    .hint(Hint::Retry(crate::cli::typed_with(&["--storage", "zfs"])))
                },
                "Cancelled. Nothing was installed.",
            )?;
            if index == 0 {
                StoragePreparation::Zfs { device: None }
            } else {
                StoragePreparation::None
            }
        }
    };
    if matches!(storage, StoragePreparation::Zfs { .. }) && matches.get_flag("no-install") {
        return Err(ProvisionError::ZfsWithoutInstaller.into());
    }
    provisioning::announce_storage(&storage);
    Ok(storage)
}

/// Prepare the remote Machine the command line names, at the release it names.
pub(super) async fn provision(
    matches: &ArgMatches,
    storage: &StoragePreparation,
) -> Result<(), ProvisionError> {
    let version = matches
        .get_one::<MachineRelease>("version")
        .expect("version has a default")
        .to_string();
    provisioning::provision_target(
        &crate::enrollment::plan::remote_target(matches),
        &version,
        storage,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_resolution_honors_explicit_and_safe_noninteractive_choices() {
        assert_eq!(
            storage_matches(["ployz", "server", "add", "root@host", "--storage", "zfs"]),
            StoragePreparation::Zfs { device: None }
        );
        assert_eq!(
            storage_matches([
                "ployz",
                "server",
                "add",
                "--standalone",
                "root@host",
                "--storage",
                "none"
            ]),
            StoragePreparation::None
        );
        assert_eq!(
            storage_matches(["ployz", "server", "add", "root@host", "--yes"]),
            StoragePreparation::None
        );
    }

    #[test]
    fn zfs_requires_the_installer() {
        let matches = crate::cli::command()
            .try_get_matches_from([
                "ployz",
                "server",
                "add",
                "root@host",
                "--storage",
                "zfs",
                "--no-install",
            ])
            .unwrap();
        assert_eq!(
            resolve_storage(
                matches
                    .subcommand_matches("server")
                    .unwrap()
                    .subcommand_matches("add")
                    .unwrap(),
            )
            .unwrap_err()
            .to_string(),
            "zfs storage preparation requires the installer; remove --no-install"
        );
    }

    fn storage_matches<const N: usize>(args: [&str; N]) -> StoragePreparation {
        let matches = crate::cli::command().try_get_matches_from(args).unwrap();
        let (_, matches) = matches
            .subcommand_matches("server")
            .unwrap()
            .subcommand()
            .unwrap();
        resolve_storage(matches).unwrap()
    }
}
