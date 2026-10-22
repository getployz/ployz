//! `server forget`: Forget one Server without a reset, or with no Server named, every
//! Server of the Organization when they were deleted.

use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;

use super::super::{Error, config_path, leaf_matches, rerun, runtime, store::Next};
use crate::approval::{self, Verb};
use crate::cli::{base, positional, value};
use crate::cloud_account::{self, Credential, ForgetCheck, ObservedServer, ServersForgotten};
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
         A human approves it in Ployz Cloud first.\n\n\
         Without --confirm outside a terminal, either form fails with \
         confirmation_required, naming what goes and the exact command to retry.",
    )
    .arg(positional("server", false).help("Server name or ID; omit to forget every Server"))
    .arg(value("confirm", None).value_name("NAME").help(
        "With a Server: its name, or dead for one that does not answer. Without: the \
         Organization's slug",
    ))
    .arg(crate::cli::approval())
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
    let runtime = runtime()?;
    let (credential, typed) = runtime.block_on(async {
        let credential = cloud_account::from_env(&store).await?;
        let Some(typed) = matches.get_one::<String>("confirm") else {
            return Err(unconfirmed(&check(&credential).await?));
        };
        Ok::<_, Error>((credential, typed))
    })?;
    forget_through_cloud(
        &runtime,
        matches,
        &credential,
        typed,
        &["server", "forget", "--confirm", typed],
    )
}

/// `server forget <server>` when no Server answers, so no Cluster can take it out: Cloud
/// forgets it as it forgets every Server when they were deleted, once it is the last
/// Server.
pub(super) fn forget_unanswered(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
    credential: &Credential,
    selector: &str,
) -> Result<(), Error> {
    let organization = runtime.block_on(unanswered(matches, credential, selector))?;
    forget_through_cloud(
        runtime,
        matches,
        credential,
        &organization,
        &["server", "forget", selector, "--confirm", "dead"],
    )
}

/// Cloud's forget, once a human approves it, which tries every Server again before it
/// forgets them. `confirmed` is the command that reruns it with an approval's id.
fn forget_through_cloud(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
    credential: &Credential,
    organization: &str,
    confirmed: &[&str],
) -> Result<(), Error> {
    let forgotten = approval::approved(
        runtime,
        credential,
        Verb::Forget,
        matches.get_one::<String>("approval").cloned(),
        |id| super::approved_rerun(matches, confirmed, id),
        async |approval| {
            if !crate::ui::json() {
                crate::ui::note("Asking Cloud to forget the Servers…");
            }
            cloud_account::forget_servers(credential, organization, approval).await
        },
    )?;
    finished(&forgotten)
}

async fn unanswered(
    matches: &ArgMatches,
    credential: &Credential,
    selector: &str,
) -> Result<String, Error> {
    let check = check(credential).await?;
    let named =
        |server: &ObservedServer| server.name == selector || server.id.to_string() == selector;
    let server = match check.servers.as_slice() {
        [server] if named(server) => server,
        servers if servers.iter().any(named) => {
            return Err(Error::conflict(format!(
                "No Server answers, so the Cluster can't forget Server {selector} alone. When \
                 every Server is gone, forget them all. No changes made."
            ))
            .hint(Hint::Next("ployz server forget".into())));
        }
        _ => return Err(Error::not_found(format!("Server {selector} not found"))),
    };
    if matches.get_one::<String>("confirm").map(String::as_str) != Some("dead") {
        let retry = rerun(
            matches,
            &["server", "forget", selector, "--confirm", "dead"],
        );
        return Err(Error::detailed(
            RpcErrorCode::ConfirmationRequired,
            format!(
                "Server {} did not answer and is the last Server: forgetting it lets go of the \
                 Cluster, and the data of its Volumes ({}) can't be recovered. It needs dead \
                 typed. No changes made.",
                server.name,
                volumes(&check),
            ),
            serde_json::json!({
                "server": { "id": server.id, "name": server.name },
                "volumes": check.volumes,
                "answered": false,
            }),
        )
        .hint(Hint::Retry(retry)));
    }
    Ok(check.organization)
}

/// Cloud's check, which waits on every Server before it answers.
async fn check(credential: &Credential) -> Result<ForgetCheck, Error> {
    if !crate::ui::json() {
        crate::ui::note("Asking Cloud which Servers to forget…");
    }
    Ok(cloud_account::check_forget_servers(credential).await?)
}

fn finished(forgotten: &ServersForgotten) -> Result<(), Error> {
    let next = "ployz server add".to_owned();
    // Nothing reaches a forgotten Server to fence it (#2927), so the operator must.
    crate::ui::warn(
        "A forgotten Server doesn't know it was forgotten. If one runs again as it is, its \
         services start again on its own copy of each Volume, and Ployz never brings back \
         what they write there. When it boots, run sudo ployz-uninstall on it to stop them \
         and keep every Volume copy on its disk. Adding it back with ployz server add --reset \
         keeps its writer copies read-only but can delete its mirrors, so don't while one may \
         be a Volume's last copy. Wipe its disk only if you need nothing on it.",
    );
    crate::ui::finish(&Next::new(forgotten, Some(next)), || {
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

    fn cloud_with(servers: &[&str]) -> (Credential, std::sync::mpsc::Receiver<String>) {
        let check = serde_json::json!({
            "organization": "acme",
            "servers": servers.iter().enumerate().map(|(at, name)| serde_json::json!({
                "id": format!("{at}").repeat(32), "name": name, "reach": "didnt_answer",
            })).collect::<Vec<_>>(),
            "volumes": [],
        });
        let (forgets, forgot) = std::sync::mpsc::channel();
        let cloud = crate::cloud_login::tests::fake_cloud(move |route, body| match route {
            "GET /api/cli/forget-servers" => (200, check.clone()),
            "POST /api/cli/forget-servers" => {
                forgets.send(body.to_owned()).unwrap();
                let mut forgotten = check.clone();
                if let Some(fields) = forgotten.as_object_mut() {
                    fields.insert("cancelled".into(), serde_json::json!([]));
                }
                (200, forgotten)
            }
            other => panic!("unexpected {other}"),
        });
        let credential = Credential::Token {
            cloud,
            token: crate::cloud_login::Secret::new("ployz_secret".to_owned()),
        };
        (credential, forgot)
    }

    fn forget_args(args: &[&str]) -> clap::ArgMatches {
        crate::cli::command()
            .try_get_matches_from(["ployz", "server", "forget"].iter().chain(args))
            .unwrap()
    }

    #[tokio::test]
    async fn a_lone_server_that_does_not_answer_is_forgotten_by_name_through_cloud() {
        let (credential, forgot) = cloud_with(&["web-1"]);
        let unconfirmed = forget_args(&["web-1"]);
        let error = unanswered(leaf_matches(&unconfirmed), &credential, "web-1")
            .await
            .unwrap_err()
            .report();
        assert_eq!(error.code, RpcErrorCode::ConfirmationRequired);
        assert_eq!(
            error.details.get("retry"),
            Some(&serde_json::json!(
                "ployz server forget web-1 --confirm dead"
            ))
        );
        assert!(forgot.try_recv().is_err(), "forgot without dead typed");

        let confirmed = forget_args(&["web-1", "--confirm", "dead"]);
        let organization = unanswered(leaf_matches(&confirmed), &credential, "web-1")
            .await
            .unwrap();
        assert_eq!(organization, "acme");
        let forgotten = cloud_account::forget_servers(&credential, &organization, None)
            .await
            .unwrap();
        assert_eq!(
            forgotten
                .check
                .servers
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>(),
            ["web-1"]
        );
        let body: serde_json::Value = serde_json::from_str(&forgot.recv().unwrap()).unwrap();
        assert_eq!(body, serde_json::json!({ "organization": "acme" }));
    }

    #[tokio::test]
    async fn one_of_several_servers_that_do_not_answer_points_at_forgetting_them_all() {
        let (credential, forgot) = cloud_with(&["web-1", "web-2"]);
        let confirmed = forget_args(&["web-1", "--confirm", "dead"]);
        let error = unanswered(leaf_matches(&confirmed), &credential, "web-1")
            .await
            .unwrap_err()
            .report();
        assert_eq!(error.code, RpcErrorCode::Conflict);
        assert_eq!(
            error.details.get("next"),
            Some(&serde_json::json!("ployz server forget"))
        );
        let missing = unanswered(leaf_matches(&confirmed), &credential, "db-1")
            .await
            .unwrap_err()
            .report();
        assert_eq!(missing.code, RpcErrorCode::NotFound);
        assert!(forgot.try_recv().is_err());
    }

    #[test]
    fn a_named_server_is_forgotten_alone_and_either_form_takes_an_approval() {
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
        let approved = parse(&["--confirm", "acme", "--approval", "apr_1"]).unwrap();
        assert!(!leaf_matches(&approved).contains_id("server"));
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
