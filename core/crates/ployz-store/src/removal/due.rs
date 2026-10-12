//! Going on with a removal: what comes off the Servers next, admitting it under
//! the mark, and deleting what no longer runs.

use std::collections::BTreeSet;

use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::mark::{self, Accepted, Closed, Evidence, Mark, State};
use super::review::held;
use super::{
    Fix, Left, LeftStatus, Removal, Residual, Target, Waiting, guard, guard_removal, latest,
    members, next, on_servers, purge, teardown_order,
};
use crate::automation::{Automated, Skipped};
use crate::deployment::admit::{AdmitRemoval, ShutDown};
use crate::deployment::{self, DeploymentStatus, DeploymentSummary, Outcome};
use crate::error;
use crate::id::{DeploymentId, EnvironmentId, ProjectId};
use crate::review::approval;
use crate::scope::{self, Environment, EnvironmentSummary};
use crate::storage::Tx;
use crate::{Actor, Trusted, VolumeLoss};

/// List what comes off the Servers next, across every removal under way.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DueQuery {}

/// An Environment whose removal Deployment may be admitted now.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Due {
    pub environment: EnvironmentSummary,
    /// The Servers its last removal failed on, when it failed.
    pub failed_on: Vec<String>,
}

/// Where one member of a removal stands, before anyone admits it.
enum Standing {
    /// Nothing of it runs on the Servers.
    Gone,
    InFlight(Box<DeploymentSummary>),
    /// Its last removal may or may not have run.
    Unknown,
    /// Another Environment comes off first, or it can't come off on its own.
    Blocked(RpcError),
    Due,
}

fn standing(tx: &mut dyn Tx, mark: &Mark, environment: &Environment) -> Result<Standing, RpcError> {
    let id = &environment.summary.id;
    if on_servers(tx, id)?.is_none() {
        return Ok(Standing::Gone);
    }
    if let Some(running) = deployment::in_flight(tx, id)? {
        return Ok(Standing::InFlight(Box::new(running)));
    }
    if latest(tx, id)?.is_some_and(|latest| latest.status == DeploymentStatus::Unknown) {
        return Ok(Standing::Unknown);
    }
    let structural = match mark.target {
        Target::Environment(_) => guard(tx, environment),
        Target::Project(_) => Ok(()),
    };
    match structural.and_then(|()| guard_removal(tx, environment)) {
        Ok(()) => Ok(Standing::Due),
        Err(error) => Ok(Standing::Blocked(error)),
    }
}

fn unknown(environment: &Environment) -> String {
    format!(
        "Nobody knows whether removing {} ran: remove it by hand",
        environment.summary.name
    )
}

/// The members of `mark` that still exist, in the order they come off.
pub(super) fn alive(tx: &mut dyn Tx, mark: &Mark) -> Result<Vec<EnvironmentId>, RpcError> {
    match &mark.target {
        Target::Environment(id) => Ok(vec![id.clone()]),
        Target::Project(project) => {
            let members = members(tx, project)?;
            let default = default_of(tx, project)?;
            Ok(teardown_order(&members, &default)
                .into_iter()
                .filter(|member| mark.members.contains(&member.id))
                .map(|member| member.id.clone())
                .collect())
        }
    }
}

fn default_of(tx: &mut dyn Tx, project: &ProjectId) -> Result<EnvironmentId, RpcError> {
    tx.query(
        "SELECT default_environment_id FROM config_project WHERE id = ?1",
        &[project.as_str().into()],
    )?
    .first()
    .ok_or_else(|| error::corrupt("Removal Project"))?
    .parse::<EnvironmentId>(0, "Project")
}

pub(crate) fn due(tx: &mut dyn Tx, who: &Actor) -> Result<Vec<Due>, RpcError> {
    let mut due = Vec::new();
    for mark in mark::live(tx, who)? {
        for member in alive(tx, &mark)? {
            let environment = scope::load_by_id(tx, &member)?;
            if !matches!(standing(tx, &mark, &environment)?, Standing::Due) {
                continue;
            }
            let failed_on = match latest(tx, &member)? {
                Some(failed) if failed.status == DeploymentStatus::Failed => {
                    deployment::failed_on(tx, &failed.id)?
                }
                _ => Vec::new(),
            };
            due.push(Due {
                environment: environment.summary,
                failed_on,
            });
        }
    }
    Ok(due)
}

/// Admit what comes off next under `mark` and delete what no longer runs:
/// `remove()` going on right after it wrote the mark.
pub(super) fn go_on(
    tx: &mut dyn Tx,
    who: &Actor,
    mark: &mut Mark,
    trusted: &Trusted,
    waiting_on: Vec<Waiting>,
) -> Result<Removal, RpcError> {
    let mut left = Vec::new();
    for member in alive(tx, mark)? {
        let environment = scope::lock_id(tx, who, &member)?;
        let (deployment, status, why) = match standing(tx, mark, &environment)? {
            Standing::Gone => continue,
            Standing::InFlight(running) if running.remove => {
                (Some(*running), LeftStatus::Removing, None)
            }
            // A Deployment someone authored goes first; the removal waits for it.
            Standing::InFlight(running) => (
                None,
                LeftStatus::WaitingTurn,
                Some(format!("Deployment #{} goes first", running.number)),
            ),
            Standing::Unknown => (None, LeftStatus::Stalled, Some(unknown(&environment))),
            Standing::Blocked(error) => (None, LeftStatus::WaitingTurn, Some(error.message)),
            Standing::Due => {
                let id = DeploymentId::parse(uuid::Uuid::new_v4().to_string())?;
                match admit_member(tx, who, (&environment, &id), mark, trusted, true) {
                    Ok(admitted) if !admitted.in_flight => continue,
                    Ok(admitted) => (Some(admitted), LeftStatus::Removing, None),
                    Err(error) if error.code == RpcErrorCode::Conflict => {
                        (None, LeftStatus::Stalled, Some(error.message))
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        left.push(Left {
            services: services(&environment)?,
            environment: environment.summary,
            deployment,
            status,
            why,
        });
    }
    if finish(tx, who, mark)?.is_some() {
        return Ok(Removal::Removed {
            residuals: mark.residuals.clone(),
        });
    }
    Ok(Removal::Removing { left, waiting_on })
}

fn services(environment: &Environment) -> Result<Vec<ServiceName>, RpcError> {
    environment
        .working
        .services
        .iter()
        .map(|service| {
            ServiceName::parse(service.slug.as_str()).map_err(|_| error::corrupt("Service name"))
        })
        .collect()
}

/// Delete `mark`'s target once nothing of it runs on the Servers, recording what
/// came off only in configuration. A Project goes in one piece, after its last
/// member. Returns what it deleted.
fn finish(
    tx: &mut dyn Tx,
    who: &Actor,
    mark: &mut Mark,
) -> Result<Option<Vec<EnvironmentSummary>>, RpcError> {
    let mut gone = Vec::new();
    for member in alive(tx, mark)? {
        if on_servers(tx, &member)?.is_some() {
            return Ok(None);
        }
        gone.push(scope::load_by_id(tx, &member)?);
    }
    let mut residuals = Vec::new();
    for environment in &gone {
        let forgotten = latest(tx, &environment.summary.id)?
            .is_some_and(|latest| latest.outcome == Some(Outcome::Forgotten));
        if forgotten {
            residuals.push(Residual {
                environment: environment.summary.name.clone(),
                services: services(environment)?,
            });
        }
    }
    mark.residuals = residuals;
    mark.state = State::Removed;
    // Saved before the delete: its target columns go NULL with the target.
    mark::save(tx, who, mark)?;
    match &mark.target {
        Target::Environment(id) => purge(tx, id)?,
        Target::Project(id) => {
            tx.execute(
                "DELETE FROM config_project WHERE id = ?1",
                &[id.as_str().into()],
            )?;
        }
    }
    Ok(Some(gone.into_iter().map(|gone| gone.summary).collect()))
}

/// Move a removal on without admitting anything: tell Cloud what to admit, and
/// delete its target once nothing of it runs.
fn settle(
    tx: &mut dyn Tx,
    who: &Actor,
    mark: &mut Mark,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    for member in alive(tx, mark)? {
        let environment = scope::lock_id(tx, who, &member)?;
        let skip = |automated: &mut Automated, reason: String| {
            automated.skipped.push(Skipped {
                environment: member.clone(),
                reason,
            });
        };
        match standing(tx, mark, &environment)? {
            Standing::Gone | Standing::InFlight(_) => {}
            Standing::Unknown => skip(automated, unknown(&environment)),
            Standing::Blocked(error) => {
                if matches!(mark.target, Target::Environment(_)) {
                    skip(automated, error.message);
                }
            }
            Standing::Due => automated.closing.push(environment.summary),
        }
    }
    if let Some(removed) = finish(tx, who, mark)? {
        automated.removed.extend(removed);
    }
    Ok(())
}

/// Close a Branch as the Store: remove it with everything it holds, unless
/// someone cancelled its removal. What it is deploying stops first.
pub(crate) fn close(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &EnvironmentId,
    because: Closed,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let environment = scope::lock_id(tx, who, id)?;
    let target = Target::Environment(id.clone());
    let mut mark = match mark::of(tx, &target)? {
        Some(mark) if mark.state == State::Cancelled => return Ok(()),
        Some(mark) => mark,
        None => {
            if mark::naming(tx, id)?.is_some() {
                return Ok(());
            }
            let summary = &environment.summary;
            let mark = Mark::new(
                who,
                (target, format!("{}/{}", summary.project, summary.name)),
                vec![id.clone()],
                [(id.clone(), Accepted::Everything { because })].into(),
            );
            mark::save(tx, who, &mark)?;
            mark
        }
    };
    if let Some(running) = deployment::in_flight(tx, id)?.filter(|running| {
        !running.remove
            && matches!(
                running.status,
                DeploymentStatus::Queued | DeploymentStatus::Running
            )
    }) {
        deployment::cancel(tx, who, &running.id)?;
    }
    settle(tx, who, &mut mark, automated)
}

/// Move every removal on, then close each Branch idle past its deadline.
pub(crate) fn sweep(
    tx: &mut dyn Tx,
    who: &Actor,
    now: i64,
    idle: &[(EnvironmentId, Option<i64>)],
) -> Result<Automated, RpcError> {
    let mut automated = Automated::default();
    for mut mark in mark::live(tx, who)? {
        settle(tx, who, &mut mark, &mut automated)?;
    }
    for (id, closes_at) in idle {
        if closes_at.is_some_and(|at| now >= at) && scope::load_by_id(tx, id).is_ok() {
            close(tx, who, id, Closed::Idle, &mut automated)?;
        }
    }
    Ok(automated)
}

/// Admit the removal Deployment of one member under its live mark: Cloud's
/// `Admit::Remove`. The mark is the consent, so nothing asks again.
pub(crate) fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &AdmitRemoval,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    let environment = scope::lock(tx, who, &admit.environment)?;
    let summary = &environment.summary;
    let Some(mut mark) = mark::naming(tx, &summary.id)?.filter(|mark| {
        !matches!(&mark.target, Target::Project(_)) || mark.members.contains(&summary.id)
    }) else {
        return Err(error::conflict(
            format!("{} isn't being removed", summary.name),
            json!({ "next": next("remove", json!({ "project": summary.project, "environment": summary.name })) }),
        ));
    };
    if let Standing::Unknown = standing(tx, &mark, &environment)? {
        return Err(error::conflict(unknown(&environment), json!({})));
    }
    if let Target::Environment(_) = mark.target {
        guard(tx, &environment)?;
    }
    admit_member(
        tx,
        who,
        (&environment, &admit.id),
        &mut mark,
        trusted,
        false,
    )
}

/// Queue the Deployment that takes one member of `mark` off the Servers. `retry`
/// is a user's own remove: it admits a failed removal again outright.
fn admit_member(
    tx: &mut dyn Tx,
    who: &Actor,
    (environment, deployment_id): (&Environment, &DeploymentId),
    mark: &mut Mark,
    trusted: &Trusted,
    retry: bool,
) -> Result<DeploymentSummary, RpcError> {
    guard_removal(tx, environment)?;
    let id = &environment.summary.id;
    refuse_running(tx, environment)?;
    if !retry {
        stalled(tx, environment, mark, trusted)?;
    }
    let forget = deployment::forgettable(tx, id, trusted)?;
    let review = crate::review::review(tx, environment)?;
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let empty = scope::empty(&environment.working.environment_slug);
    let losses = match forget {
        Some(_) => Vec::new(),
        None => {
            let removed = super::removed(&review.head.applied, &empty, &namespace)?;
            losses(environment, removed, mark.accepted.get(id), trusted)?
        }
    };
    let frozen = deployment::freeze(
        id,
        &empty,
        &review.head.applied,
        deployment::Scope::All { losses: &losses },
        namespace,
        None,
    )?;
    let admitted = deployment::admit(
        tx,
        who,
        (deployment_id, &[], None, None),
        id,
        deployment::NOTHING,
        &frozen,
    )?;
    mark.evidence.insert(
        id.clone(),
        Evidence {
            deployment: admitted.id.clone(),
            servers: trusted.servers,
            unanswered: unanswered(trusted),
        },
    );
    mark::save(tx, who, mark)?;
    if let Some(outcome) = forget {
        return deployment::forget(tx, &admitted.id, outcome);
    }
    Ok(admitted)
}

fn unanswered(trusted: &Trusted) -> BTreeSet<ployz_core::MachineId> {
    trusted
        .volumes
        .iter()
        .flat_map(|observed| observed.unanswered.iter().cloned())
        .collect()
}

fn refuse_running(tx: &mut dyn Tx, environment: &Environment) -> Result<(), RpcError> {
    let Some(running) = deployment::in_flight(tx, &environment.summary.id)?.filter(|running| {
        matches!(
            running.status,
            DeploymentStatus::Running | DeploymentStatus::Cancelling
        )
    }) else {
        return Ok(());
    };
    Err(error::conflict(
        format!(
            "Deployment #{} is running: wait for it or cancel it before removing {}",
            running.number, environment.summary.name
        ),
        json!({ "deployment": running.id }),
    ))
}

/// Refuse to admit a member whose last removal failed again, unless something
/// changed since that may let it go now: the user removed it again, a Server
/// that didn't answer answers, or a Server left.
fn stalled(
    tx: &mut dyn Tx,
    environment: &Environment,
    mark: &Mark,
    trusted: &Trusted,
) -> Result<(), RpcError> {
    let id = &environment.summary.id;
    let Some(failed) = latest(tx, id)?.filter(|latest| latest.status == DeploymentStatus::Failed)
    else {
        return Ok(());
    };
    let fresh = unanswered(trusted);
    let changed = mark.evidence.get(id).is_some_and(|before| {
        before
            .unanswered
            .iter()
            .any(|server| !fresh.contains(server))
            || matches!((before.servers, trusted.servers), (Some(then), Some(now)) if now < then)
    });
    if mark.retried > failed.ended_at.unwrap_or(i64::MAX) || changed {
        return Ok(());
    }
    let failed_on = deployment::failed_on(tx, &failed.id)?;
    let reason = failed
        .message
        .clone()
        .unwrap_or_else(|| format!("it failed on {}", failed_on.join(", ")));
    Err(error::conflict(
        format!("Removing {} stalled: {reason}", environment.summary.name),
        json!({
            "failed_on": failed_on,
            "next": next("remove", json!({
                "project": environment.summary.project,
                "environment": environment.summary.name,
            })),
        }),
    ))
}

/// The Docker Volumes this member's removal deletes, held to what its remover
/// accepted against what the Servers report now.
fn losses(
    environment: &Environment,
    removed: Vec<super::RemovedVolume>,
    accepted: Option<&Accepted>,
    trusted: &Trusted,
) -> Result<Vec<VolumeLoss>, RpcError> {
    let observed = trusted.volumes.as_ref();
    let lenient = |removed: Vec<super::RemovedVolume>| {
        held(removed.clone(), observed).map_or_else(
            |_| {
                removed
                    .into_iter()
                    .map(|volume| VolumeLoss {
                        volume,
                        deletes: Vec::new(),
                    })
                    .collect()
            },
            |(losses, _)| losses,
        )
    };
    let name = &environment.summary.name;
    match accepted {
        Some(Accepted::Everything { .. }) => Ok(lenient(removed)),
        Some(Accepted::EveryCopy(names)) => {
            if removed.iter().all(|volume| names.contains(&volume.name)) {
                return Ok(lenient(removed));
            }
            let (losses, unanswered) = held(removed, observed)?;
            refuse_unaccepted(name, &losses, |loss| names.contains(&loss.volume.name))?;
            if !unanswered.is_empty() {
                return Err(unobserved(&unanswered));
            }
            Ok(losses)
        }
        confirmed => {
            let empty = BTreeSet::new();
            let confirmed = match confirmed {
                Some(Accepted::Confirmed(confirmed)) => confirmed,
                _ => &empty,
            };
            let (losses, unanswered) = held(removed, observed)?;
            if !unanswered.is_empty() {
                return Err(unobserved(&unanswered));
            }
            refuse_unaccepted(name, &losses, |loss| {
                loss.deletes.iter().all(|delete| confirmed.contains(delete))
            })?;
            Ok(losses)
        }
    }
}

fn refuse_unaccepted(
    name: &crate::id::EnvironmentName,
    losses: &[VolumeLoss],
    accepted: impl Fn(&VolumeLoss) -> bool,
) -> Result<(), RpcError> {
    let unaccepted: Vec<&str> = losses
        .iter()
        .filter(|loss| !loss.deletes.is_empty() && !accepted(loss))
        .map(|loss| loss.volume.name.as_str())
        .collect();
    if unaccepted.is_empty() {
        return Ok(());
    }
    Err(error::conflict(
        format!(
            "Removing {name} now deletes data its removal didn't accept ({}): run remove again",
            unaccepted.join(", ")
        ),
        json!({ "volumes": unaccepted }),
    ))
}

fn unobserved(unanswered: &[ployz_core::MachineId]) -> RpcError {
    let waiting_on: Vec<Waiting> = unanswered
        .iter()
        .map(|server| Waiting {
            server: server.to_string(),
            why: "It didn't answer whether it holds this data".to_owned(),
            fix: Fix::ServerRemove {
                server: server.to_string(),
            },
        })
        .collect();
    error::unobserved(
        "Some Servers didn't answer whether they hold this data",
        json!({ "waiting_on": waiting_on }),
    )
}

/// Take an Environment off the Servers and keep it: a Server's shutdown. It
/// writes no mark, and asks a human only when it deletes Volume data.
pub(crate) fn shut_down(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &ShutDown,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    let environment = scope::lock(tx, who, &admit.environment)?;
    guard_removal(tx, &environment)?;
    refuse_running(tx, &environment)?;
    let id = &environment.summary.id;
    let forget = deployment::forgettable(tx, id, trusted)?;
    let review = crate::review::review(tx, &environment)?;
    // A shut-down consumes no draft, so whether its pull requests merged doesn't
    // matter: no version is needed, a stale one is still refused.
    if let Some(version) = admit.version.as_deref() {
        crate::review::check(&review, Some(version))?;
    }
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let empty = scope::empty(&environment.working.environment_slug);
    let losses = match forget {
        Some(_) => Vec::new(),
        None => crate::review::destructive(
            who,
            &review,
            (&empty, &namespace, crate::review::Shipping::Deploy(&[])),
            (admit.version.as_deref(), &admit.accept_volume_loss),
            trusted.volumes.as_ref(),
        )?,
    };
    if forget.is_none() && !admit.accept_volume_loss.is_empty() {
        let deployed = approval::deployed(&review.head);
        let cluster_domains = if crate::domain::has_generated(&deployed) {
            deployment::cluster_domains(tx, id, &review.head)?
        } else {
            Default::default()
        };
        super::gate(
            who,
            &Target::Environment(id.clone()),
            &review.view.version,
            approval::destructive_effects(&deployed, &empty, &cluster_domains),
            &trusted.approval,
            || json!(review.view),
        )?;
    }
    let frozen = deployment::freeze(
        id,
        &empty,
        &review.head.applied,
        deployment::Scope::All { losses: &losses },
        namespace,
        None,
    )?;
    let admitted = deployment::admit(
        tx,
        who,
        (&admit.id, &[], None, None),
        id,
        deployment::NOTHING,
        &frozen,
    )?;
    if let Some(outcome) = forget {
        return deployment::forget(tx, &admitted.id, outcome);
    }
    Ok(admitted)
}
