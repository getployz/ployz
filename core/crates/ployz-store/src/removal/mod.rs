//! Removal: the Store owns taking an Environment or a whole Project off the
//! Servers and out of the Store.
//!
//! [`Remove`] reviews once what the removal deletes, asks once, and writes a mark.
//! The mark is the authority from then on: [`due`] lists what to take off the
//! Servers next, `Admit::Remove` takes it off under the mark without asking again,
//! and the sweep deletes the target once nothing of it runs. Branches come off
//! before their Parents, and a Project's Default Environment last.
//!
//! A removal with nothing new to try reports stalled with its reason; nothing
//! retries it on a timer.

mod due;
pub(crate) mod mark;
mod review;

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{MachineId, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

pub use self::due::{Due, DueQuery};
pub(crate) use self::due::{admit, close, sweep};
pub(crate) use self::due::{due, shut_down};
use self::mark::{Accepted, Mark, State};
pub use self::review::{RemovedVolume, VolumeLoss, docker_volume};
pub(crate) use self::review::{removed, review, short_digest};
use crate::deployment::{self, DeploymentStatus, DeploymentSummary};
use crate::error;
use crate::id::{
    ApprovalDigest, DeploymentId, EnvironmentId, EnvironmentName, ProjectId, ProjectName,
    RemovalId, VolumeName,
};
use crate::review::{DestructiveEffect, approval};
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;
use crate::trusted::Approval;
use crate::{Actor, Trusted};

/// Take an Environment, or a whole Project, off the Servers and out of the Store.
/// It asks once for everything it deletes, then goes on without asking again.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Remove {
    pub project: ProjectName,
    /// One Environment of the Project; none removes the Project.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub environment: Option<EnvironmentName>,
    /// The version a `confirmation_required` refusal handed back.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub version: Option<String>,
    /// Deployed Volumes whose data this may delete, by Environment and name.
    #[serde(default)]
    #[ts(as = "Option<BTreeMap<EnvironmentName, Vec<VolumeName>>>", optional)]
    pub accept_volume_loss: BTreeMap<EnvironmentName, Vec<VolumeName>>,
    /// Volumes whose every copy may go, wherever one turns up: a Server that didn't
    /// answer may hold one.
    #[serde(default)]
    #[ts(as = "Option<BTreeMap<EnvironmentName, Vec<VolumeName>>>", optional)]
    pub every_copy: BTreeMap<EnvironmentName, Vec<VolumeName>>,
}

/// Stop removing an Environment or a Project: what is running is cancelled, and
/// nothing closes it again on its own until it is reopened or pushed to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CancelRemoval {
    pub project: ProjectName,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub environment: Option<EnvironmentName>,
}

/// A removal stopped, with the Deployments it cancelled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RemovalCancelled {
    pub project: ProjectName,
    pub environment: Option<EnvironmentName>,
    pub cancelled: Vec<DeploymentId>,
}

/// Where a removal stands.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "removal", rename_all = "snake_case")]
pub enum Removal {
    /// Gone from the Servers and the Store.
    Removed { residuals: Vec<Residual> },
    /// Still on the Servers: what is left, and the Servers it waits on. `id` reads
    /// what it left on old servers once it is gone.
    Removing {
        id: RemovalId,
        left: Vec<Left>,
        waiting_on: Vec<Waiting>,
    },
}

/// Read what a finished removal left on old servers, by the `id` it was removing
/// under.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemovedQuery {
    pub id: RemovalId,
}

/// What a finished removal left on old servers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RemovedView {
    pub residuals: Vec<Residual>,
}

/// An Environment a removal hasn't taken off the Servers yet.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Left {
    pub environment: EnvironmentSummary,
    /// The Deployment taking it off, once one is admitted.
    pub deployment: Option<DeploymentSummary>,
    pub status: LeftStatus,
    /// Why it waits or stalled; users read it.
    pub why: Option<String>,
    pub services: Vec<ServiceName>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LeftStatus {
    Removing,
    /// Another Environment comes off first.
    WaitingTurn,
    /// Its removal failed, and nothing changed since that would make it go now.
    Stalled,
}

/// A Server a removal waits on, and what fixes that.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Waiting {
    pub server: String,
    pub why: String,
    pub fix: Fix,
}

/// What a caller does about a refusal or a wait.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Fix {
    UseCloud,
    ConnectServers,
    OpenDashboard { page: String },
    UpdateClient,
    ServerRemove { server: String },
}

/// An Environment that came off only in configuration: no Server was left to take
/// its Services off, so whatever ran them may still run them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Residual {
    pub environment: EnvironmentName,
    pub services: Vec<ServiceName>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Environment(EnvironmentId),
    Project(ProjectId),
}

/// What a refusal's caller does next: a tool and its input.
pub(crate) fn next(tool: &str, input: Value) -> Value {
    json!({ "tool": tool, "input": input })
}

fn show(project: &ProjectName, environment: &EnvironmentName) -> Value {
    next(
        "environment_show",
        json!({ "project": project, "environment": environment }),
    )
}

/// One member's review: what taking it off the Servers deletes.
struct Reviewed {
    environment: Environment,
    base: String,
    effects: BTreeSet<DestructiveEffect>,
    losses: Vec<VolumeLoss>,
    unanswered: Vec<MachineId>,
    every_copy: BTreeSet<VolumeName>,
    accept: BTreeSet<VolumeName>,
    diff: crate::DiffView,
}

impl Reviewed {
    fn lost(&self) -> impl Iterator<Item = &VolumeLoss> {
        self.losses.iter().filter(|loss| !loss.deletes.is_empty())
    }

    /// The Volumes an unanswered Server may hold that `every_copy` doesn't cover.
    fn uncovered(&self, every_copy: &BTreeSet<VolumeName>) -> Vec<VolumeName> {
        if self.unanswered.is_empty() {
            return Vec::new();
        }
        self.losses
            .iter()
            .map(|loss| loss.volume.name.clone())
            .filter(|name| !every_copy.contains(name))
            .collect()
    }

    fn destructive(&self) -> bool {
        !self.effects.is_empty() || self.lost().next().is_some() || !self.unanswered.is_empty()
    }

    fn binding(&self, who: &Actor, every_copy: &BTreeSet<VolumeName>) -> String {
        let lost: Vec<&VolumeLoss> = self.lost().collect();
        short_digest(
            &json!({
                "organization": who.organization,
                "environment": self.environment.summary.id,
                "volumes": lost,
                "every_copy": every_copy,
                "unanswered": self.unanswered,
                "effects": self.effects,
            })
            .to_string(),
        )
    }

    fn accepted(&self) -> Accepted {
        if self.every_copy.is_empty() {
            Accepted::Confirmed(self.lost().flat_map(|loss| loss.deletes.clone()).collect())
        } else {
            Accepted::EveryCopy(self.every_copy.union(&self.accept).cloned().collect())
        }
    }
}

pub(crate) fn remove(
    tx: &mut dyn Tx,
    who: &Actor,
    remove: &Remove,
    trusted: &Trusted,
) -> Result<Removal, RpcError> {
    let project = scope::project(tx, who, Some(&remove.project))?;
    lock_project(tx, &project.id)?;
    let (target, target_name, members) = match &remove.environment {
        Some(name) => {
            let environment = scope::lock(
                tx,
                who,
                &EnvironmentRef {
                    project: Some(project.name.clone()),
                    environment: Some(name.clone()),
                },
            )?;
            guard(tx, &environment)?;
            (
                Target::Environment(environment.summary.id.clone()),
                format!("{}/{}", project.name, name),
                vec![environment],
            )
        }
        None => {
            let mut members = Vec::new();
            for member in self::members(tx, &project.id)? {
                members.push(scope::lock_id(tx, who, &member.id)?);
            }
            (
                Target::Project(project.id.clone()),
                project.name.to_string(),
                members,
            )
        }
    };
    let names: Vec<&EnvironmentName> = members.iter().map(|member| &member.summary.name).collect();
    if let Some(unknown) = remove
        .accept_volume_loss
        .keys()
        .chain(remove.every_copy.keys())
        .find(|name| !names.contains(name))
    {
        return Err(error::invalid(
            format!("This removes no Environment named {unknown}"),
            json!({ "valid_children": names }),
        ));
    }
    let mut reviews = Vec::new();
    for environment in members {
        let id = environment.summary.id.clone();
        if on_servers(tx, &id)?.is_none() || deployment::forgettable(tx, &id, trusted)?.is_some() {
            continue;
        }
        let name = environment.summary.name.clone();
        let consent = |given: &BTreeMap<EnvironmentName, Vec<VolumeName>>| {
            given
                .get(&name)
                .into_iter()
                .flatten()
                .cloned()
                .collect::<BTreeSet<_>>()
        };
        let (accept, every_copy) = (
            consent(&remove.accept_volume_loss),
            consent(&remove.every_copy),
        );
        reviews.push(review_member(
            tx,
            who,
            environment,
            (accept, every_copy),
            trusted,
        )?);
    }
    refuse_unanswered(who, remove, &reviews, &target)?;
    let version = version(who, &target, &reviews, |reviewed| {
        reviewed.every_copy.clone()
    });
    confirm(remove, &reviews, &version)?;
    gate(
        who,
        &target,
        &version,
        reviews
            .iter()
            .flat_map(|reviewed| reviewed.effects.clone())
            .collect(),
        &trusted.approval,
        || {
            let diffs: BTreeMap<&EnvironmentName, &crate::DiffView> = reviews
                .iter()
                .map(|reviewed| (&reviewed.environment.summary.name, &reviewed.diff))
                .collect();
            match &target {
                Target::Environment(_) => json!(diffs.values().next()),
                Target::Project(_) => json!(diffs),
            }
        },
    )?;
    let accepted: BTreeMap<EnvironmentId, Accepted> = reviews
        .iter()
        .map(|reviewed| (reviewed.environment.summary.id.clone(), reviewed.accepted()))
        .collect();
    mark::clear_tombstone(tx, &target)?;
    let mut mark = match mark::of(tx, &target)? {
        Some(mut live) => {
            live.accepted = accepted;
            live.retried = deployment::now();
            live
        }
        None => {
            let members = match &target {
                Target::Environment(id) => vec![id.clone()],
                Target::Project(id) => self::members(tx, id)?
                    .into_iter()
                    .map(|member| member.id)
                    .collect(),
            };
            Mark::new(who, (target, target_name), members, accepted)
        }
    };
    mark::save(tx, who, &mark)?;
    let waiting_on = reviews
        .iter()
        .flat_map(|reviewed| {
            let holds = reviewed
                .losses
                .iter()
                .map(|loss| loss.volume.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            reviewed.unanswered.iter().map(move |machine| Waiting {
                server: machine.to_string(),
                why: format!("It didn't answer whether it holds {holds}"),
                fix: Fix::ServerRemove {
                    server: machine.to_string(),
                },
            })
        })
        .collect();
    due::go_on(tx, who, &mut mark, trusted, waiting_on)
}

fn review_member(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: Environment,
    (accept, every_copy): (BTreeSet<VolumeName>, BTreeSet<VolumeName>),
    trusted: &Trusted,
) -> Result<Reviewed, RpcError> {
    let id = &environment.summary.id;
    let review = crate::review::review(tx, &environment)?;
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let empty = scope::empty(&environment.working.environment_slug);
    let deployed = approval::deployed(&review.head);
    let cluster_domains = if crate::domain::has_generated(&deployed) {
        deployment::cluster_domains(tx, id, &review.head)?
    } else {
        BTreeMap::new()
    };
    let effects = approval::destructive_effects(&deployed, &empty, &cluster_domains);
    let removed = self::removed(&review.head.applied, &empty, &namespace)?;
    let names: Vec<&VolumeName> = removed.iter().map(|volume| &volume.name).collect();
    if let Some(unknown) = accept
        .iter()
        .chain(&every_copy)
        .find(|name| !names.contains(name))
    {
        return Err(error::invalid(
            format!(
                "Removing {} deletes no Volume named {unknown}",
                environment.summary.name
            ),
            json!({ "valid_children": names }),
        ));
    }
    let (losses, unanswered) = review::held(removed, trusted.volumes.as_ref())?;
    Ok(Reviewed {
        base: review.view.version.clone(),
        diff: review.view,
        environment,
        effects,
        losses,
        unanswered,
        every_copy,
        accept,
    })
}

/// Refuse, writing nothing, while a Server that didn't answer may hold a Volume
/// the caller didn't give up every copy of.
fn refuse_unanswered(
    who: &Actor,
    remove: &Remove,
    reviews: &[Reviewed],
    target: &Target,
) -> Result<(), RpcError> {
    let uncovered: Vec<(&Reviewed, Vec<VolumeName>)> = reviews
        .iter()
        .map(|reviewed| (reviewed, reviewed.uncovered(&reviewed.every_copy)))
        .filter(|(_, uncovered)| !uncovered.is_empty())
        .collect();
    if uncovered.is_empty() {
        return Ok(());
    }
    let waiting_on: Vec<Waiting> = uncovered
        .iter()
        .flat_map(|(reviewed, volumes)| {
            let volumes = volumes
                .iter()
                .map(VolumeName::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            reviewed.unanswered.iter().map(move |machine| Waiting {
                server: machine.to_string(),
                why: format!(
                    "It didn't answer whether it holds {volumes} of {}",
                    reviewed.environment.summary.name
                ),
                fix: Fix::ServerRemove {
                    server: machine.to_string(),
                },
            })
        })
        .collect();
    let all_of = |reviewed: &Reviewed| -> BTreeSet<VolumeName> {
        reviewed
            .losses
            .iter()
            .map(|loss| loss.volume.name.clone())
            .collect()
    };
    let version = version(who, target, reviews, |reviewed| {
        if reviewed.unanswered.is_empty() {
            reviewed.every_copy.clone()
        } else {
            all_of(reviewed)
        }
    });
    let every_copy: BTreeMap<&EnvironmentName, BTreeSet<VolumeName>> = reviews
        .iter()
        .filter(|reviewed| !reviewed.unanswered.is_empty())
        .map(|reviewed| (&reviewed.environment.summary.name, all_of(reviewed)))
        .collect();
    let accept: BTreeMap<&EnvironmentName, Vec<&VolumeName>> = reviews
        .iter()
        .map(|reviewed| {
            (
                &reviewed.environment.summary.name,
                reviewed
                    .lost()
                    .map(|loss| &loss.volume.name)
                    .collect::<Vec<_>>(),
            )
        })
        .filter(|(_, lost)| !lost.is_empty())
        .collect();
    Err(error::unobserved(
        "Some Servers didn't answer whether they hold this data, so nothing was removed. \
         Remove those Servers, or give up every copy of it wherever one turns up",
        json!({
            "waiting_on": waiting_on,
            "every_copy": next("remove", json!({
                "project": remove.project,
                "environment": remove.environment,
                "accept_volume_loss": accept,
                "every_copy": every_copy,
                "version": version,
            })),
        }),
    ))
}

/// What a confirmation binds: an Environment's review version and binding, or for
/// a Project a digest over every member's.
fn version(
    who: &Actor,
    target: &Target,
    reviews: &[Reviewed],
    every_copy: impl Fn(&Reviewed) -> BTreeSet<VolumeName>,
) -> String {
    match (target, reviews) {
        (Target::Environment(_), [reviewed]) => {
            format!(
                "{}:{}",
                reviewed.base,
                reviewed.binding(who, &every_copy(reviewed))
            )
        }
        (Target::Environment(id), _) => id.to_string(),
        (Target::Project(id), _) => {
            let bindings: BTreeMap<&EnvironmentId, (&str, String)> = reviews
                .iter()
                .map(|reviewed| {
                    (
                        &reviewed.environment.summary.id,
                        (
                            reviewed.base.as_str(),
                            reviewed.binding(who, &every_copy(reviewed)),
                        ),
                    )
                })
                .collect();
            format!(
                "{id}:{}",
                short_digest(&json!({ "project": id, "members": bindings }).to_string())
            )
        }
    }
}

/// Refuse a destructive removal unless it passes the version its refusal handed
/// back and accepts each lost Volume by name.
fn confirm(remove: &Remove, reviews: &[Reviewed], version: &str) -> Result<(), RpcError> {
    if !reviews.iter().any(Reviewed::destructive) {
        return Ok(());
    }
    let unaccepted = reviews.iter().any(|reviewed| {
        reviewed.lost().any(|loss| {
            !reviewed.accept.contains(&loss.volume.name)
                && !reviewed.every_copy.contains(&loss.volume.name)
        })
    });
    if remove.version.as_deref() == Some(version) && !unaccepted {
        return Ok(());
    }
    let lost: BTreeMap<&EnvironmentName, Vec<&VolumeLoss>> = reviews
        .iter()
        .map(|reviewed| {
            (
                &reviewed.environment.summary.name,
                reviewed.lost().collect(),
            )
        })
        .filter(|(_, lost): &(_, Vec<_>)| !lost.is_empty())
        .collect();
    let accept: BTreeMap<&EnvironmentName, Vec<&VolumeName>> = lost
        .iter()
        .map(|(name, lost)| (*name, lost.iter().map(|loss| &loss.volume.name).collect()))
        .collect();
    let effects: BTreeSet<&DestructiveEffect> = reviews
        .iter()
        .flat_map(|reviewed| &reviewed.effects)
        .collect();
    let deletes = effects
        .iter()
        .map(|effect| effect.describe())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    let ask = match lost.is_empty() {
        true => "Pass the version to go ahead",
        false => "Pass the version and accept each lost Volume by name to go ahead",
    };
    Err(error::confirmation_required(
        format!("Removing this will {deletes}. {ask}"),
        json!({
            "volumes": lost,
            "accept": accept,
            "effects": effects,
            "version": version,
        }),
    ))
}

/// Ask a human once for everything a removal deletes: the same digest whether
/// `remove` or `Admit::ShutDown` asks.
pub(crate) fn gate(
    who: &Actor,
    target: &Target,
    version: &str,
    effects: BTreeSet<DestructiveEffect>,
    approval: &Approval,
    diff: impl FnOnce() -> Value,
) -> Result<(), RpcError> {
    if matches!(approval, Approval::NotRequired) || effects.is_empty() {
        return Ok(());
    }
    let digest = match target {
        Target::Environment(id) => approval::approval_digest(who, id, version, &effects),
        Target::Project(id) => project_digest(who, id, version, &effects),
    };
    if matches!(approval, Approval::Approved(approved) if *approved == digest) {
        return Ok(());
    }
    let asked = effects
        .iter()
        .map(DestructiveEffect::describe)
        .collect::<Vec<_>>()
        .join(", ");
    Err(error::approval_required(
        format!("A human must approve this first: {asked}"),
        json!({ "effects": effects, "approval": digest, "diff": diff() }),
    ))
}

fn project_digest(
    who: &Actor,
    project: &ProjectId,
    version: &str,
    effects: &BTreeSet<DestructiveEffect>,
) -> ApprovalDigest {
    let bound = json!({
        "organization": who.organization,
        "project": project,
        "effects": effects,
    });
    ApprovalDigest::parse(format!("{version}:{}", short_digest(&bound.to_string())))
        .expect("a version and a 16-digit hex digest form an approval")
}

pub(crate) fn cancel_removal(
    tx: &mut dyn Tx,
    who: &Actor,
    cancel: &CancelRemoval,
) -> Result<RemovalCancelled, RpcError> {
    let project = scope::project(tx, who, Some(&cancel.project))?;
    lock_project(tx, &project.id)?;
    let target = match &cancel.environment {
        Some(name) => Target::Environment(
            scope::lock(
                tx,
                who,
                &EnvironmentRef {
                    project: Some(project.name.clone()),
                    environment: Some(name.clone()),
                },
            )?
            .summary
            .id,
        ),
        None => Target::Project(project.id.clone()),
    };
    let Some(mut mark) = mark::of(tx, &target)?.filter(|mark| mark.state == State::Removing) else {
        return Err(error::conflict(
            format!(
                "{} isn't being removed",
                cancel.environment.as_ref().map_or_else(
                    || project.name.to_string(),
                    |name| format!("{}/{name}", project.name)
                )
            ),
            json!({}),
        ));
    };
    let cancelled = stop(tx, who, &mut mark, None)?;
    Ok(RemovalCancelled {
        project: project.name,
        environment: cancel.environment.clone(),
        cancelled,
    })
}

/// Cancel a Deployment. Cancelling one that removes a marked target stops the
/// whole removal, as [`cancel_removal`] does.
pub(crate) fn cancel(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
) -> Result<DeploymentSummary, RpcError> {
    let cancelled = deployment::cancel(tx, who, id)?;
    if cancelled.remove
        && let Some(mut mark) = mark::naming(tx, &cancelled.environment_id)?
    {
        stop(tx, who, &mut mark, Some(id))?;
    }
    Ok(cancelled)
}

/// Leave `mark` a tombstone and cancel every removal of its members still in
/// flight but `except`.
fn stop(
    tx: &mut dyn Tx,
    who: &Actor,
    mark: &mut Mark,
    except: Option<&DeploymentId>,
) -> Result<Vec<DeploymentId>, RpcError> {
    let mut cancelled = Vec::new();
    for member in due::alive(tx, mark)? {
        scope::lock_id(tx, who, &member)?;
        if let Some(running) = deployment::in_flight(tx, &member)?
            .filter(|running| running.remove && Some(&running.id) != except)
        {
            deployment::cancel(tx, who, &running.id)?;
            cancelled.push(running.id);
        }
    }
    mark.state = State::Cancelled;
    mark::save(tx, who, mark)?;
    Ok(cancelled)
}

/// Refuse to build on an Environment being removed, itself or with its Project.
pub(crate) fn marked(tx: &mut dyn Tx, environment: &Environment) -> Result<(), RpcError> {
    if mark::naming(tx, &environment.summary.id)?.is_none() {
        return Ok(());
    }
    let summary = &environment.summary;
    Err(error::conflict(
        format!("{} is being removed", summary.name),
        json!({ "next": show(&summary.project, &summary.name) }),
    ))
}

/// Refuse to add an Environment to a Project being removed. Takes the Project
/// lock, which `remove` holds while it freezes the Project's members.
pub(crate) fn project_marked(
    tx: &mut dyn Tx,
    project: &ProjectId,
    name: &ProjectName,
) -> Result<(), RpcError> {
    lock_project(tx, project)?;
    let live = mark::of(tx, &Target::Project(project.clone()))?
        .is_some_and(|mark| mark.state == State::Removing);
    if !live {
        return Ok(());
    }
    Err(error::conflict(
        format!("{name} is being removed"),
        json!({ "next": next("project_show", json!({ "project": name })) }),
    ))
}

fn lock_project(tx: &mut dyn Tx, project: &ProjectId) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_project SET name = name WHERE id = ?1",
        &[project.as_str().into()],
    )?;
    Ok(())
}

/// The Deployment through which `environment` may still run on the Servers: one
/// in flight, or the last that ran unless it was a removal that applied. None
/// when nothing ever ran or its removal applied.
pub(crate) fn on_servers(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    match deployment::in_flight(tx, environment)? {
        Some(running) => Ok(Some(running)),
        None => ran(tx, environment),
    }
}

/// The last Deployment that ran `environment` on the Servers, unless it was a
/// removal that applied. None when nothing of it ever ran, or its removal applied.
pub(crate) fn ran(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    Ok(deployment::last_ran(tx, environment)?
        .filter(|ran| !(ran.remove && ran.status == DeploymentStatus::Applied)))
}

/// `environment`'s latest Deployment, when that removes it.
pub(crate) fn latest(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    Ok(deployment::history(tx, environment, 1)?
        .into_iter()
        .next()
        .filter(|latest| latest.remove))
}

/// Refuse to take an Environment off the Servers while a Branch of it may still
/// run there, using it live; or, for the Default Environment, while any other
/// Environment of its Project but its Parents may. So the Default comes off last.
pub(crate) fn guard_removal(tx: &mut dyn Tx, environment: &Environment) -> Result<(), RpcError> {
    let summary = &environment.summary;
    let project = scope::project_of(tx, &summary.id)?;
    let members = members(tx, &project.id)?;
    let branches = members
        .iter()
        .filter(|member| member.parent.as_ref() == Some(&summary.id));
    let running = still_running(tx, branches)?;
    if let Some(first) = running.first() {
        return Err(error::conflict(
            format!(
                "{} has Branches that may still run ({}): they come off first",
                summary.name,
                names(&running)
            ),
            json!({ "branches": running, "next": show(&summary.project, first) }),
        ));
    }
    if summary.id != project.default_environment {
        return Ok(());
    }
    let parents = ancestors(&members, &summary.id);
    let others = members
        .iter()
        .filter(|member| member.id != summary.id && !parents.contains(&member.id));
    let running = still_running(tx, others)?;
    if let Some(first) = running.first() {
        return Err(error::conflict(
            format!(
                "{} is the Default Environment: it comes off last, after {}",
                summary.name,
                names(&running)
            ),
            json!({ "environments": running, "next": show(&summary.project, first) }),
        ));
    }
    Ok(())
}

fn names(names: &[EnvironmentName]) -> String {
    names
        .iter()
        .map(EnvironmentName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn still_running<'a>(
    tx: &mut dyn Tx,
    members: impl Iterator<Item = &'a Member>,
) -> Result<Vec<EnvironmentName>, RpcError> {
    let mut running = Vec::new();
    for member in members {
        if on_servers(tx, &member.id)?.is_some() {
            running.push(member.name.clone());
        }
    }
    Ok(running)
}

/// Refuse to remove the Default Environment, or one with Branches, on its own:
/// Branches use it live, and the Default goes with its Project.
pub(crate) fn guard(tx: &mut dyn Tx, environment: &Environment) -> Result<(), RpcError> {
    let summary = &environment.summary;
    let default = tx.query(
        "SELECT name FROM config_project WHERE default_environment_id = ?1",
        &[summary.id.as_str().into()],
    )?;
    if !default.is_empty() {
        return Err(error::unsupported(
            format!(
                "{} is the Default Environment: it goes only with Project {}",
                summary.name, summary.project
            ),
            json!({ "next": show(&summary.project, &summary.name) }),
        ));
    }
    let branches = tx
        .query(
            "SELECT e.name FROM config_environment_branch b \
             JOIN config_environment e ON e.id = b.environment_id \
             WHERE b.parent_id = ?1 ORDER BY e.name",
            &[summary.id.as_str().into()],
        )?
        .iter()
        .map(|row| row.parse::<EnvironmentName>(0, "Environment"))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(first) = branches.first() {
        return Err(error::unsupported(
            format!(
                "{} has Branches ({}): remove them first",
                summary.name,
                names(&branches)
            ),
            json!({ "branches": branches, "next": show(&summary.project, first) }),
        ));
    }
    Ok(())
}

/// One Environment of a Project, as teardown orders them.
pub(crate) struct Member {
    pub(crate) id: EnvironmentId,
    pub(crate) name: EnvironmentName,
    pub(crate) parent: Option<EnvironmentId>,
}

/// A Project's Environments with their Parents, in ID order.
pub(crate) fn members(tx: &mut dyn Tx, project: &ProjectId) -> Result<Vec<Member>, RpcError> {
    let rows = tx.query(
        "SELECT e.id, e.name, b.parent_id FROM config_environment e \
         LEFT JOIN config_environment_branch b ON b.environment_id = e.id \
         WHERE e.project_id = ?1 ORDER BY e.id",
        &[project.as_str().into()],
    )?;
    rows.iter()
        .map(|row| {
            Ok(Member {
                id: row.parse::<EnvironmentId>(0, "Environment")?,
                name: row.parse::<EnvironmentName>(1, "Environment")?,
                parent: row.parse_optional::<EnvironmentId>(2, "Environment")?,
            })
        })
        .collect()
}

/// The Parent of `environment`, its Parent's Parent, and so on.
fn ancestors(members: &[Member], environment: &EnvironmentId) -> Vec<EnvironmentId> {
    let mut chain = Vec::new();
    let mut at = environment;
    while let Some(parent) = members
        .iter()
        .find(|member| &member.id == at)
        .and_then(|member| member.parent.as_ref())
        .filter(|parent| !chain.contains(*parent))
    {
        chain.push(parent.clone());
        at = parent;
    }
    chain
}

/// The order a Project's Environments come off the Servers: Branches before their
/// Parents, and the Default after every other but its own Parents.
pub(crate) fn teardown_order<'a>(
    members: &'a [Member],
    default: &EnvironmentId,
) -> Vec<&'a Member> {
    let parents = ancestors(members, default);
    let mut order: Vec<&Member> = members.iter().collect();
    order.sort_by_key(|member| {
        let class = if &member.id == default {
            1
        } else if parents.contains(&member.id) {
            2
        } else {
            0
        };
        (
            class,
            std::cmp::Reverse(ancestors(members, &member.id).len()),
        )
    });
    order
}

/// Delete `environment` and everything it owns: its rows go with it.
pub(crate) fn purge(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_environment WHERE id = ?1",
        &[environment.as_str().into()],
    )?;
    Ok(())
}
