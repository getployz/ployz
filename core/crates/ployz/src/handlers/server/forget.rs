//! `server forget`: Forget Servers, for every Server of the Organization when they
//! were deleted. Cloud tries each first and refuses while any answers.

use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;

use super::super::{Error, config_path, leaf_matches, runtime, store::Next};
use crate::cli::value;
use crate::cloud_account::{self, ForgetCheck};
use crate::cloud_login::CredentialStore;
use crate::output::say;
use crate::ui::Hint;

pub(super) fn command() -> Command {
    Command::new("forget")
        .about("Forget every Server of the Organization, when they were deleted")
        .long_about(
            "Forget every Server of the Organization when they were deleted. Cloud tries \
             each Server first, waiting up to ten seconds for it, and refuses while any \
             answers: remove those with ployz server rm instead. Projects, settings and \
             Deployment history stay; Volume data on these Servers can't be recovered. \
             The next ployz server add starts over, and published Environments deploy to \
             it. Type the Organization's slug with --confirm; without it the command \
             fails with confirmation_required, naming what goes and the exact command \
             to retry.",
        )
        .arg(
            value("confirm", None)
                .value_name("ORGANIZATION")
                .help("The Organization's slug, typed to confirm forgetting its Servers"),
        )
}

/// Forget the Organization's Servers. Without `--confirm`, Cloud's check names what
/// goes and nothing changes; Cloud checks again before it forgets.
pub(super) fn forget(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = CredentialStore::beside(&config_path(matches)?);
    let forgotten = runtime()?.block_on(async {
        let credential = cloud_account::from_env(&store).await?;
        let Some(typed) = matches.get_one::<String>("confirm") else {
            let check = cloud_account::check_forget_servers(&credential).await?;
            return Err(unconfirmed(&check));
        };
        Ok::<_, Error>(cloud_account::forget_servers(&credential, typed).await?)
    })?;
    let next = "ployz server add".to_owned();
    crate::output::finish(&Next::new(&forgotten, Some(next)), || {
        let check = &forgotten.check;
        say!(
            "Forgot the Servers of Organization {}: {}. Cloud reached none of them.",
            check.organization,
            server_names(check)
        );
        say!(
            "Volume data they held can't be recovered: {}.",
            volumes(check)
        );
        if !forgotten.cancelled.is_empty() {
            say!(
                "Cancelled {} Deployment(s) that might still have run.",
                forgotten.cancelled.len()
            );
        }
        say!("Next: ployz server add");
    })
}

/// Without `--confirm` nothing changes: the error names what would go and the
/// command that confirms it.
fn unconfirmed(check: &ForgetCheck) -> Error {
    let slug = &check.organization;
    let retry = format!("ployz server forget --confirm {slug}");
    Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        format!(
            "Forgetting the Servers of Organization {slug} ({}) loses the data of its Volumes \
             ({}); this can't be undone. No changes made.",
            server_names(check),
            volumes(check),
        ),
        serde_json::json!({
            "organization": slug,
            "servers": check.servers,
            "volumes": check.volumes,
        }),
    )
    .hint(Hint::Retry(retry))
}

fn server_names(check: &ForgetCheck) -> String {
    listed(check.servers.iter().map(|server| server.name.clone()))
}

fn volumes(check: &ForgetCheck) -> String {
    listed(
        check
            .volumes
            .iter()
            .map(|lost| format!("{}/{}/{}", lost.project, lost.environment, lost.volume)),
    )
}

fn listed(names: impl Iterator<Item = String>) -> String {
    let names: Vec<_> = names.collect();
    match names.is_empty() {
        true => "none".to_owned(),
        false => names.join(", "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_account::{ObservedServer, Reach};

    #[test]
    fn without_confirm_it_names_what_goes_and_the_confirming_command() {
        let check = ForgetCheck {
            organization: "acme".into(),
            servers: vec![ObservedServer {
                id: ployz_core::MachineId::parse("a".repeat(32)).unwrap(),
                name: "web-1".into(),
                reach: Reach::DidntAnswer,
            }],
            volumes: vec![ployz_store::AppliedVolume {
                project: ployz_store::ProjectName::parse("shop").unwrap(),
                environment: ployz_store::EnvironmentName::parse("production").unwrap(),
                volume: ployz_store::VolumeName::parse("data").unwrap(),
            }],
        };
        let error = unconfirmed(&check).report();
        assert_eq!(error.code, RpcErrorCode::ConfirmationRequired);
        assert_eq!(
            error.details.get("retry"),
            Some(&serde_json::json!("ployz server forget --confirm acme"))
        );
        assert!(error.message.contains("web-1") && error.message.contains("shop/production/data"));
    }
}
