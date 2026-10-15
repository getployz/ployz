//! `server forget`: Forget one Server without a reset, or with no Server named, every
//! Server of the Organization when they were deleted.

use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;

use super::super::{Error, config_path, leaf_matches, runtime, store::Next};
use crate::cli::{base, positional, value};
use crate::cloud_account::{self, ForgetCheck};
use crate::cloud_login::CredentialStore;
use crate::ui::Hint;

pub(crate) fn command(command: Command) -> Command {
    base(command, "Forget a Server without resetting it, or every Server when they were deleted",
    )
    .long_about(
        "Forget a Server: take it out of the Cluster without resetting it, for a Server \
         that is gone or can't be reset. Refused while the Server answers and holds a \
         Volume copy: remove it with ployz server rm instead, so its reset demotes the \
         copy. Type the Server's name with --confirm, or dead for a Server that does not \
         answer within ten seconds. When Cloud manages the Server, Cloud forgets it even \
         with --context or --connect, asking a human first when the Organization wants \
         that.\n\n\
         Without a Server, forget every Server of the Organization when they were \
         deleted. Cloud tries each Server first, waiting up to ten seconds for it, and \
         refuses while any answers. Projects, settings and Deployment history stay; \
         Volume data on these Servers can't be recovered. The next ployz server add \
         starts over, and Environments with a saved or deployed revision deploy to it. Type the Organization's \
         slug with --confirm.\n\n\
         Without --confirm outside a terminal, either form fails with \
         confirmation_required, naming what goes and the exact command to retry.",
    )
    .arg(positional("server", false))
    .arg(value("confirm", None).value_name("NAME").help(
        "With a Server: its name, or dead for one that does not answer. Without: the \
         Organization's slug",
    ))
    .arg(crate::cli::approval().requires("server"))
}

/// Forget the named Server, or without one, every Server of the Organization.
pub(crate) fn forget(root: &ArgMatches) -> Result<(), Error> {
    if leaf_matches(root).contains_id("server") {
        super::remove::forget(root)
    } else {
        forget_all(root)
    }
}

/// Forget the Organization's Servers. Without `--confirm`, Cloud's check names what
/// goes and nothing changes; Cloud checks again before it forgets.
fn forget_all(root: &ArgMatches) -> Result<(), Error> {
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
    crate::ui::finish(&Next::new(&forgotten, Some(next)), || {
        let check = &forgotten.check;
        crate::ui::stream(format_args!(
            "Forgot the Servers of Organization {}: {}. Cloud reached none of them.",
            check.organization,
            server_names(check)
        ));
        crate::ui::stream(format_args!(
            "Volume data they held can't be recovered: {}.",
            volumes(check)
        ));
        if !forgotten.cancelled.is_empty() {
            crate::ui::stream(format_args!(
                "Cancelled {} Deployment(s) that might still have run.",
                forgotten.cancelled.len()
            ));
        }
        crate::ui::hint(&crate::ui::Hint::Next("ployz server add".into()));
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
    fn a_named_server_is_forgotten_alone_and_only_it_takes_an_approval() {
        let parse = |args: &[&str]| {
            crate::cli::command()
                .try_get_matches_from(["ployz", "server", "forget"].iter().chain(args))
        };
        let all = parse(&["--confirm", "acme"]).unwrap();
        assert!(!leaf_matches(&all).contains_id("server"));
        let one = parse(&["web-1", "--confirm", "dead", "--approval", "apr_1"]).unwrap();
        assert_eq!(
            leaf_matches(&one).get_one::<String>("server").unwrap(),
            "web-1"
        );
        assert!(parse(&["--confirm", "acme", "--approval", "apr_1"]).is_err());
    }

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
