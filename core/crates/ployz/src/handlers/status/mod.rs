//! `ployz status`: who you are, where commands act, and what is staged, deploying and
//! needs attention there.

pub(super) mod model;

use std::borrow::Cow;
use std::time::Duration;

use clap::{ArgMatches, Command};
use ployz_core::{LocalMachinePhase, RpcErrorCode, error_chain};
use ployz_store::{
    DeploymentStatus, DeploymentSummary, DeploymentsQuery, DiffQuery, EnvironmentSummary,
    NamespaceQuery,
};
use serde::Serialize;

use super::link::{Identity, identity, scope};
use super::status_servers::UNCHECKED;
use super::store::{next, reachable, scoped};
use super::{Error, config_path, leaf_matches};
use crate::checkin::{LocalCheckin, Paths, Seen};
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

/// This host's check-in record, and whether its Machine left the Cluster since it joined.
#[derive(Serialize)]
struct ThisServer {
    #[serde(flatten)]
    checkin: LocalCheckin,
    /// The check-in loop saw this server join, and its daemon now runs no Machine, as after
    /// `ployz server rm` resets it. The stopped loop never rewrites its record.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    left: bool,
    /// A check-in loop ran here and none runs now, so the record no longer changes.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stopped: bool,
}

impl ThisServer {
    fn read(checkin: LocalCheckin, left: bool, loop_stopped: bool) -> Self {
        // A loop that ended or joined meant to stop; one that didn't left its last answer behind.
        let stopped =
            loop_stopped && checkin.view.answer != Seen::Joined && !checkin.headline().ended;
        Self {
            checkin,
            left,
            stopped,
        }
    }
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
    this_server: Option<ThisServer>,
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
        .and_then(|runtime| runtime.block_on(crate::checkin::local_machine(LOCAL_INSPECT)));
    let this_server = this_server(local.as_ref());
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
        .or_else(|| {
            (identity.is_none() && this_server.is_none()).then(|| "ployz login".to_owned())
        });
    let status = Status {
        this_server,
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

fn this_server(local: Option<&ployz_core::MachineDetails>) -> Option<ThisServer> {
    let path = Paths::production().status();
    let checkin = LocalCheckin::read(&path)
        .inspect_err(|error| {
            crate::ui::warn(format!("Could not read {}: {error}", path.display()));
        })
        .ok()
        .flatten()?;
    let left = checkin.view.answer == Seen::Joined
        && local.is_some_and(|details| {
            matches!(
                details.phase,
                LocalMachinePhase::Uninitialized | LocalMachinePhase::Resetting
            )
        });
    let stopped = crate::checkin::loop_stopped(&Paths::production().checkin_dir);
    Some(ThisServer::read(checkin, left, stopped))
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
        this_server: None,
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
    if let Some(ThisServer { left: true, .. }) = &status.this_server {
        record.push(
            "this server",
            Cell::status("Not in a Cluster", Tone::Change),
        );
    } else if let Some(ThisServer {
        checkin, stopped, ..
    }) = &status.this_server
    {
        let crate::checkin::Headline {
            title: headline,
            ended,
        } = checkin.headline();
        let tone = match (checkin.view.answer, ended) {
            (Seen::Joined, _) => Tone::Good,
            (_, true) => Tone::Bad,
            (_, false) => Tone::Change,
        };
        if *stopped {
            record.push("this server", Cell::status("Check-in stopped", Tone::Bad));
            record.push("last answer", Cell::status(headline, tone));
        } else {
            record.push("this server", Cell::status(headline, tone));
        }
        for blocker in &checkin.view.blockers {
            record.push("blocked", Cell::status(&blocker.message, Tone::Bad));
        }
        if let Some(message) = checkin
            .view
            .message
            .as_deref()
            .filter(|_| checkin.says_why())
        {
            record.push("reason", message);
        }
        if *stopped {
            record.push(
                "check-in",
                "Not running. See why with `sudo systemctl status ployz-checkin`, then run `sudo ployz server checkin --install`.",
            );
        }
        record.push(
            "checked in",
            crate::checkin::ago(checkin.view.at, chrono::Utc::now()),
        );
    }
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

    /// The check-in record a Fleet Token host kept after `ployz server rm` reset it.
    const JOINED: &str = r#"{
        "answer": "joined",
        "at": "2026-10-20T22:29:15.665202151Z",
        "prep": {"docker": "ready", "zfs": "ready"},
        "blockers": [],
        "prepare_retries": 0,
        "attempt": {"attemptId": "attempt-1", "stage": "publishing"}
    }"#;

    /// The check-in record of a host still waiting for approval.
    const WAITING: &str = r#"{
        "answer": "wait",
        "at": "2026-10-22T01:21:48Z",
        "prep": {"docker": "ready", "zfs": "ready"},
        "blockers": [],
        "prepare_retries": 0
    }"#;

    fn status_on(left: bool) -> String {
        status_of(JOINED, left, false)
    }

    fn status_of(record: &str, left: bool, stopped: bool) -> String {
        let status = Status {
            this_server: Some(ThisServer::read(
                serde_json::from_str(record).unwrap(),
                left,
                stopped,
            )),
            this_machine: None,
            identity: None,
            scope: Scope::default(),
            environment: None,
            staged: None,
            deploying: Vec::new(),
            attention: Vec::new(),
        };
        let mut out = Vec::new();
        status_record(&status).write(&mut out, false).unwrap();
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
    }

    #[test]
    fn a_reset_server_is_not_in_a_cluster_whatever_its_check_in_last_saw() {
        assert_eq!(
            status_on(true),
            "this server = Not in a Cluster\nsigned in as = no one\n"
        );
        assert!(
            status_on(false).starts_with("this server = Joined\nchecked in = "),
            "{}",
            status_on(false)
        );
    }

    #[test]
    fn status_on_a_server_shows_the_fingerprint_device_enforce_prints() {
        let key = ployz_core::WireGuardPublicKey([3; 32]);
        let status = Status {
            this_server: None,
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

    /// The record a loop left when it found no ployzd and exited.
    const NO_PLOYZD: &str = r#"{
        "answer": "no_ployzd",
        "message": "No ployzd on PATH. Run sudo ployz server checkin --install.",
        "at": "2026-10-22T01:21:48Z",
        "blockers": [],
        "prepare_retries": 0
    }"#;

    const STOPPED: &str = "check-in = Not running. See why with `sudo systemctl status ployz-checkin`, then run `sudo ployz server checkin --install`.\n";

    #[test]
    fn a_waiting_server_whose_check_in_loop_stopped_says_so_and_what_it_last_saw() {
        assert!(
            status_of(WAITING, false, true).starts_with(&format!(
                "this server = Check-in stopped\nlast answer = Waiting for approval\n{STOPPED}checked in = "
            )),
            "{}",
            status_of(WAITING, false, true)
        );
        assert!(
            status_of(WAITING, false, false)
                .starts_with("this server = Waiting for approval\nchecked in = "),
            "{}",
            status_of(WAITING, false, false)
        );
        assert!(status_of(JOINED, false, true).starts_with("this server = Joined\n"));
    }

    #[test]
    fn json_says_stopped_only_where_the_text_does() {
        let json = |record| {
            serde_json::to_value(ThisServer::read(
                serde_json::from_str(record).unwrap(),
                false,
                true,
            ))
            .unwrap()
        };
        assert_eq!(
            json(WAITING).get("stopped"),
            Some(&serde_json::Value::Bool(true))
        );
        assert!(json(JOINED).get("stopped").is_none());
        assert!(
            json(&WAITING.replace(r#""wait""#, r#""stop""#))
                .get("stopped")
                .is_none()
        );
    }

    #[test]
    fn a_stopped_loop_keeps_the_reason_it_recorded() {
        assert!(
            status_of(NO_PLOYZD, false, true).starts_with(&format!(
                "this server = Check-in stopped\nlast answer = No ployzd\nreason = No ployzd on PATH. Run sudo ployz server checkin --install.\n{STOPPED}checked in = "
            )),
            "{}",
            status_of(NO_PLOYZD, false, true)
        );
    }
}
