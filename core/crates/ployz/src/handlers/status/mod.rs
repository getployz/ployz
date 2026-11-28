//! `ployz status`: who you are, where commands act, and what is staged, deploying and
//! needs attention there.

pub(super) mod model;

use std::borrow::Cow;
use std::time::Duration;

use clap::{ArgMatches, Command};
use ployz_core::{RpcErrorCode, error_chain};
use ployz_store::{
    DeploymentStatus, DeploymentSummary, DeploymentsQuery, DiffQuery, EnvironmentSummary,
    NamespaceQuery,
};
use serde::Serialize;

use super::link::{Identity, identity, scope};
use super::status_servers::UNCHECKED;
use super::store::{next, reachable, scoped};
use super::{Error, config_path, leaf_matches};
use crate::cloud::account::StoreCallError;
use crate::scope::Scope;
use crate::ui::{Cell, Fields, Next, Tone};
use model::{Attention, Subject};

pub(super) fn command(command: Command) -> Command {
    scoped(crate::cli::base(
        command,
        "Show who you are, where commands act, and what is staged, deploying or needs attention",
    ))
}

/// Working State that isn't deployed yet.
#[derive(Serialize)]
struct Staged {
    /// How many changes, counting each node and each Setting.
    changes: usize,
    /// Whether a saved version already holds them.
    saved: bool,
    /// The `ployz diff` version they'd deploy by.
    version: String,
}

/// This host's Machine key and its fingerprint.
#[derive(Serialize)]
struct ThisMachine {
    wireguard_key: String,
    fingerprint: String,
}

#[derive(Serialize)]
struct Status {
    #[serde(skip_serializing_if = "Option::is_none")]
    this_machine: Option<ThisMachine>,
    identity: Option<Identity>,
    scope: Scope,
    /// The Environment the scope resolves to; none when it doesn't resolve.
    environment: Option<EnvironmentSummary>,
    staged: Option<Staged>,
    /// Deployments queued or running, newest first.
    deploying: Vec<DeploymentSummary>,
    attention: Vec<Attention>,
}

/// Enough recent Deployments to hold every in-flight one and the latest that ended.
const RECENT: usize = 5;

/// `ployz status`: one call that orients an agent.
pub(super) fn status(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    crate::context::Config::load_or_empty(config_path(matches)?)?;
    let local = super::runtime()
        .ok()
        .and_then(|runtime| runtime.block_on(local_machine()));
    let this_machine = this_machine(local.as_ref());
    let store = reachable(root)?;
    let identity = store.as_ref().map(identity).transpose()?;
    let scope = scope(matches)?;
    let mut attention = Vec::new();
    let diff = match &store {
        None => None,
        Some(store) => match store.try_read(&DiffQuery {
            environment: scope.at(),
        }) {
            Ok(diff) => Some(diff),
            Err(StoreCallError::Refused(error))
                if matches!(error.code, RpcErrorCode::NotFound | RpcErrorCode::Ambiguous) =>
            {
                let next = match error.details.get("next").and_then(|next| next.as_str()) {
                    Some(next) => next.to_owned(),
                    None => "ployz link --project PROJECT".to_owned(),
                };
                attention.push(
                    Attention::new(None, error.code.to_string(), error_chain::row(&error))
                        .next(next),
                );
                None
            }
            Err(error) => return Err(store.fail(error)),
        },
    };
    let mut deploying = Vec::new();
    if let (Some(store), Some(_)) = (&store, &diff) {
        let page = store.read(&DeploymentsQuery {
            environment: scope.at(),
            limit: Some(RECENT),
            cursor: None,
        })?;
        // The latest numbered one, when it ended; else, behind work queued since, the
        // first numbered one on the page that ended, as numbered ones list in number order.
        let ended = page
            .latest
            .as_ref()
            .filter(|latest| latest.status.ended())
            .or_else(|| {
                page.deployments
                    .iter()
                    .find(|deployment| deployment.number.is_some() && deployment.status.ended())
            });
        if let Some(ended) = ended {
            let reason = match ended.status {
                DeploymentStatus::Failed => Some("deployment_failed"),
                DeploymentStatus::Unknown => Some("deployment_unknown"),
                DeploymentStatus::WaitingForCi
                | DeploymentStatus::Queued
                | DeploymentStatus::Superseded
                | DeploymentStatus::Running
                | DeploymentStatus::Applied
                | DeploymentStatus::Cancelling
                | DeploymentStatus::Cancelled => None,
            };
            if let Some(reason) = reason {
                attention.push(
                    Attention::new(
                        None,
                        reason,
                        format!("Deployment {} did not complete", ended.label()),
                    )
                    .deployment(ended.id.clone())
                    .next(super::deploy::hint_for(matches, ended)),
                );
            }
        }
        deploying = page.in_flight;
    }
    for reference in diff.iter().flat_map(|diff| &diff.broken) {
        attention.push(
            Attention::new(None, "broken_reference", reference.to_string())
                .next(super::store::fix_broken(matches, reference)),
        );
    }
    let mut partial = false;
    if let (Some(store), Some(_)) = (&store, &diff) {
        match store.try_read(&NamespaceQuery {
            environment: scope.at(),
        }) {
            Ok(view) => {
                let servers = super::status_servers::attention(
                    root,
                    &view.namespace,
                    &view.services,
                    view.authored.as_ref(),
                    &deploying,
                );
                attention.extend(servers.attention);
                partial = servers.partial;
            }
            Err(error) => {
                crate::ui::warn_cause(UNCHECKED, &store.fail(error));
                partial = true;
            }
        }
    }
    let staged = diff.as_ref().map(|diff| Staged {
        changes: diff.review_count(),
        saved: diff.published,
        version: diff.version.clone(),
    });
    let hint = attention
        .first()
        .and_then(|attention| attention.command().map(str::to_owned))
        .or_else(|| {
            staged
                .as_ref()
                .is_some_and(|staged| staged.changes > 0)
                .then(|| next(matches, &["diff"]))
        })
        .or_else(|| {
            deploying
                .first()
                .map(|deployment| super::deploy::hint_for(matches, deployment))
        })
        .or_else(|| identity.is_none().then(|| "ployz login".to_owned()));
    let status = Status {
        this_machine,
        identity,
        scope,
        environment: diff.map(|diff| diff.environment),
        staged,
        deploying,
        attention,
    };
    let followed = Next::new(&status, hint);
    crate::ui::fields(&followed, &status_record(&status))?;
    followed.say();
    if partial {
        return Err(Error::partial());
    }
    Ok(())
}

/// How long `status` waits for this host's daemon to say whether it still runs a Machine.
const LOCAL_INSPECT: Duration = Duration::from_secs(3);

/// This host's Machine, if its daemon answers on the local socket within [`LOCAL_INSPECT`].
async fn local_machine() -> Option<ployz_core::MachineDetails> {
    use crate::context::{Connection, ConnectionSource, SelectedConnections};
    use ployz_core::{InspectRequest, op};

    let inspect = async {
        let connection = Connection::unix(ployz_core::MachineLayout::production().socket()).ok()?;
        let mut client = crate::connect::connect_selected_with(
            SelectedConnections {
                source: ConnectionSource::LocalSocket,
                connections: vec![connection],
            },
            std::sync::Arc::new(crate::connect::SystemConnector::default()),
        )
        .await
        .ok()?;
        client
            .call::<op::Inspect>(
                InspectRequest::default(),
                crate::cluster::Call::read().waiting(crate::cluster::Wait::standard()),
            )
            .await
            .ok()
    };
    tokio::time::timeout(LOCAL_INSPECT, inspect)
        .await
        .ok()
        .flatten()
}

fn this_machine(local: Option<&ployz_core::MachineDetails>) -> Option<ThisMachine> {
    let key = local?.machine.as_ref()?.public_key;
    Some(ThisMachine {
        wireguard_key: key.to_string(),
        fingerprint: key.fingerprint(),
    })
}

#[cfg(test)]
pub(in crate::handlers) fn rendered(attention: Vec<Attention>) -> String {
    let status = Status {
        this_machine: None,
        identity: Some(Identity {
            credential: "local",
            cloud: None,
            account: None,
            organization: None,
        }),
        scope: Scope::default(),
        environment: None,
        staged: None,
        deploying: Vec::new(),
        attention,
    };
    let mut out = Vec::new();
    status_record(&status).write(&mut out, true).unwrap();
    anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
}

fn status_record(status: &Status) -> Fields {
    let mut record = Fields::new();
    if let Some(machine) = &status.this_machine {
        record.push("wireguard key", &machine.fingerprint);
    }
    let Some(identity) = &status.identity else {
        record.push("signed in as", "no one");
        return record;
    };
    if let Some(cloud) = &identity.cloud {
        record.push("cloud", cloud);
    }
    record.push(
        "organization",
        identity
            .organization
            .as_ref()
            .map_or("?", |organization| organization.slug.as_str()),
    );
    record.push(
        "signed in as",
        identity
            .account
            .as_ref()
            .map_or(identity.credential, |account| account.email.as_str()),
    );
    if let Some(environment) = &status.environment {
        record.push(
            "environment",
            format_args!("{}/{}", environment.project, environment.name),
        );
    }
    if let Some(link) = &status.scope.link {
        record.push("linked from", link);
    }
    if let Some(staged) = &status.staged {
        record.push(
            "staged",
            format_args!(
                "{} {}",
                staged.changes,
                if staged.changes == 1 {
                    "change"
                } else {
                    "changes"
                }
            ),
        );
    }
    for deployment in &status.deploying {
        record.push(
            "deploying",
            format_args!(
                "{} {}",
                deployment.label(),
                super::store::word(&deployment.status)
            ),
        );
    }
    for attention in &status.attention {
        let label = match attention.subject() {
            Some(
                Subject::NotPickedUp { service, .. }
                | Subject::StaleAddress { service, .. }
                | Subject::NotServing { service, .. }
                | Subject::LeftPending { service, .. }
                | Subject::NoCopies { service },
            ) => Cow::Owned(crate::ui::escaped(&service.name.to_string())),
            Some(Subject::Down { machine } | Subject::DiskFull { machine, .. }) => {
                Cow::Owned(crate::ui::escaped(&machine.to_string()))
            }
            Some(Subject::Unanswered { .. }) | None => Cow::Borrowed("needs attention"),
        };
        record.push(
            label,
            Cell::status(crate::ui::escaped(attention.message()), Tone::Bad),
        );
    }
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_on_a_server_shows_the_fingerprint_device_enforce_prints() {
        let key = ployz_core::WireGuardPublicKey([3; 32]);
        let status = Status {
            this_machine: Some(ThisMachine {
                wireguard_key: key.to_string(),
                fingerprint: key.fingerprint(),
            }),
            identity: None,
            scope: Scope::default(),
            environment: None,
            staged: None,
            deploying: Vec::new(),
            attention: Vec::new(),
        };
        let mut out = Vec::new();
        status_record(&status).write(&mut out, false).unwrap();
        assert_eq!(
            anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string(),
            format!(
                "wireguard key = {}\nsigned in as = no one\n",
                key.fingerprint()
            )
        );
    }
}
