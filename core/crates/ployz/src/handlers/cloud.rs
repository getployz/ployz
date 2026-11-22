//! `ployz cloud reset`.

use clap::{ArgMatches, Command};
use ployz_core::Hint;

use super::{Error, config_path, leaf_matches, runtime};
use crate::cli::{base, switch};
use crate::ui;

pub(super) fn group(command: Command) -> Command {
    command.about("Manage Cloud")
}

pub(super) fn reset_command(command: Command) -> Command {
    base(command, "Give up your Organization's founding that never finished")
                .long_about("Give up your Organization's founding that never finished, so the next ployz server add founds the Cluster again. Stop or erase the founding Server first. Needs `ployz login`.")
                .arg(
                    switch("yes", Some('y'))
                        .help("Confirm the founding Server is stopped or erased"),
                )
}

pub(super) fn reset(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    if !matches.get_flag("yes") {
        return Err(Error::usage(
            "cloud reset gives up the unfinished founding; stop or erase the founding Server, then confirm with --yes",
        )
        .hint(Hint::Retry("ployz cloud reset --yes".into())));
    }
    let store = crate::cloud::login::CredentialStore::beside(&config_path(matches)?);
    runtime()?.block_on(async {
        // PLOYZ_TOKEN or this device's sign-in, as every Cloud command: one Organization.
        let credential = crate::cloud::account::from_env(&store).await?;
        let organization = crate::cloud::account::acting_in(&credential).await?;
        let _: serde_json::Value = crate::cloud::account::call(
            &credential,
            reqwest::Method::POST,
            "cloud/reset",
            Some(&serde_json::json!({
                "organizationId": organization.id,
                "confirmedFounderStoppedOrErased": true,
            })),
        )
        .await?;
        ui::stream(format_args!("Reset the founding of {}.", organization.slug));

        crate::ui::emit(&serde_json::json!({ "reset": true, "organization": organization }))
    })
}
