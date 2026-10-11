//! Removing what ran: the typed confirmation, what goes, and the removal Deployment
//! that takes an Environment off the Servers first. `env rm` and `project rm` share it.

use clap::ArgMatches;
use ployz_core::{RpcErrorCode, ServiceName};
use std::collections::BTreeMap;

use ployz_store::{
    Admit, AdmitRemoval, DeploymentId, DeploymentStatus, DeploymentSummary, DeploymentView,
    DueQuery, EnvironmentId, EnvironmentName, EnvironmentRef, EnvironmentSummary,
    EnvironmentsQuery, Left, ProjectName, ProjectsQuery, Removal, RemovalsQuery, Remove, Residual,
    ServicesQuery, ShutDown, VolumeName, VolumesQuery,
};
use serde_json::json;

use super::Error;
use super::deploy;
use super::store::{self, Store, mint};
use crate::cloud_account::StoreCallError;
use crate::ui::{self, Hint, Tree};

/// `--confirm` typed `name`, or a person typed it at the prompt after seeing
/// `unconfirmed`'s loss tree. Anywhere else its refusal is the error; a
/// different `--confirm` is a usage error naming `retry`.
pub(super) fn confirm(
    matches: &ArgMatches,
    name: &str,
    what: &str,
    retry: String,
    unconfirmed: impl FnOnce() -> Result<(Error, Tree), Error>,
) -> Result<(), Error> {
    match matches.get_one::<String>("confirm") {
        Some(typed) if typed == name => Ok(()),
        Some(typed) => Err(Error::usage(format!(
            "--confirm {} does not match {what} {name}. No changes made.",
            typed.escape_debug()
        ))
        .hint(Hint::Retry(retry))),
        None => {
            let (refusal, loss) = unconfirmed()?;
            if ui::can_prompt() {
                ui::note_inline(&loss);
            }
            ui::confirm_name(name, || refusal, "Cancelled. Nothing was removed.")
        }
    }
}

pub(super) fn inventory(store: &Store, at: &EnvironmentRef) -> Result<Inventory, Error> {
    let services = store.read(&ServicesQuery {
        environment: at.clone(),
    })?;
    let volumes = store.read(&VolumesQuery {
        environment: at.clone(),
    })?;
    Ok(Inventory {
        environment: services.environment,
        services: services
            .services
            .into_iter()
            .map(|listing| listing.service.name)
            .collect(),
        volumes: volumes
            .volumes
            .iter()
            .map(|listing| json!({ "name": listing.volume.name, "deployed": listing.deployed }))
            .collect(),
    })
}

/// Every `--accept-volume-loss` name.
pub(super) fn accepted(matches: &ArgMatches) -> Result<Vec<VolumeName>, Error> {
    store::volume_names(matches, "accept-volume-loss")
}

/// `--accept-volume-loss` for each of `accept`, the words a retry repeats.
pub(super) fn with_accepted(accept: &[VolumeName]) -> Vec<&str> {
    accept
        .iter()
        .flat_map(|name| ["--accept-volume-loss", name.as_str()])
        .collect()
}

/// Remove `environment` of `project`, or the whole Project, in one write: each
/// `--accept-volume-loss` name goes to the members whose removal deletes it, and
/// `--expect-version` binds the review its refusal handed back. Then run each
/// removal Deployment it admitted. In process, this CLI also moves the removal on
/// (sweep, admit what is due, run it) until the Store deleted the target; over
/// HTTPS, Cloud carries on after the first ones. `None` once a removal Deployment
/// that didn't apply was reported (running the command again finishes it).
pub(super) fn remove(
    matches: &ArgMatches,
    store: &Store,
    (project, environment): (&ProjectName, Option<&EnvironmentName>),
    mut events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<Option<Removed>, Error> {
    deploy::refuse_local_detach(matches, store)?;
    let accept = accepted(matches)?;
    let again = store.again(&with_accepted(&accept));
    let members = match environment {
        Some(name) => vec![name.clone()],
        None => store
            .read(&EnvironmentsQuery {
                project: Some(project.clone()),
            })?
            .environments
            .into_iter()
            .map(|listing| listing.name)
            .collect(),
    };
    let at = |name: &EnvironmentName| EnvironmentRef {
        project: Some(project.clone()),
        environment: Some(name.clone()),
    };
    let mut accept_volume_loss = BTreeMap::new();
    for name in &members {
        let deletes = store.read(&RemovalsQuery {
            environment: at(name),
            remove: true,
        })?;
        let accepted: Vec<_> = accept
            .iter()
            .filter(|accepted| {
                deletes
                    .volumes
                    .iter()
                    .any(|volume| &volume.name == *accepted)
            })
            .cloned()
            .collect();
        if !accepted.is_empty() {
            accept_volume_loss.insert(name.clone(), accepted);
        }
    }
    let request = Remove {
        project: project.clone(),
        environment: environment.cloned(),
        version: matches.get_one::<String>("expect-version").cloned(),
        accept_volume_loss,
        every_copy: BTreeMap::new(),
    };
    let refs: Vec<_> = members.iter().map(at).collect();
    let removal = store.remove(&request, &refs).map_err(|error| {
        let first = goes_first(&error, project);
        let goes = first.is_some();
        store.accepting(store::with_next(
            error,
            |_| goes,
            || first.unwrap_or_default(),
        ))
    })?;
    let mut ran = Vec::new();
    let result = (|| {
        let (left, waiting_on) = match removal {
            Removal::Removed { residuals } => {
                return Ok(Some((Removal::Removed { residuals }, Vec::new())));
            }
            Removal::Removing { left, waiting_on } => (left, waiting_on),
        };
        let mut names: BTreeMap<EnvironmentId, EnvironmentName> = left
            .iter()
            .map(|left| (left.environment.id.clone(), left.environment.name.clone()))
            .collect();
        let mut admitted: Vec<DeploymentSummary> = left
            .iter()
            .filter_map(|left| left.deployment.clone())
            .collect();
        loop {
            for deployment in std::mem::take(&mut admitted) {
                let writer = events
                    .as_mut()
                    .map(|writer| writer.get_ref().try_clone())
                    .transpose()?
                    .map(std::io::BufWriter::new);
                let deploy::Shipped {
                    view, ran: outcome, ..
                } = deploy::execute(matches, store, &deployment, None, writer)?;
                if view.deployment.status != DeploymentStatus::Applied {
                    let reported = unfinished(matches, &view, &ran, outcome, &again);
                    ran.clear();
                    reported?;
                    return Ok(None);
                }
                ran.push(view.deployment);
            }
            if store.local().is_none() {
                return Ok(Some((
                    Removal::Removing { left, waiting_on },
                    std::mem::take(&mut ran),
                )));
            }
            store.sweep().map_err(|error| store.fail(error))?;
            if gone(store, project, environment)? {
                let residuals = ran
                    .iter()
                    .filter(|deployment| left_on_old_servers(deployment))
                    .filter_map(|deployment| {
                        Some(Residual {
                            environment: names.get(&deployment.environment_id)?.clone(),
                            services: deployment.services.clone(),
                        })
                    })
                    .collect();
                return Ok(Some((
                    Removal::Removed { residuals },
                    std::mem::take(&mut ran),
                )));
            }
            for due in store.read(&DueQuery {})? {
                let summary = due.environment;
                if summary.project != *project
                    || environment.is_some_and(|name| *name != summary.name)
                {
                    continue;
                }
                admitted.push(
                    store
                        .admit(&Admit::Remove(AdmitRemoval {
                            id: DeploymentId::parse(mint())?,
                            environment: at(&summary.name),
                        }))
                        .map_err(|error| store.fail(error))?,
                );
                names.insert(summary.id, summary.name);
            }
            if admitted.is_empty() {
                return Err(stalled(project, environment, &left).hint(Hint::Next(again.clone())));
            }
        }
    })();
    match result {
        Ok(Some((removal, ran))) => Ok(Some(Removed { removal, ran })),
        Ok(None) => Ok(None),
        Err(error) if !ran.is_empty() => {
            for deployment in &ran {
                crate::ui::stream(format_args!("Deployment #{} applied", deployment.number));
            }
            crate::ui::emit_committed(json!({ "applied": ran }), Err(error)).map(|()| None)
        }
        Err(error) => Err(error),
    }
}

/// Where a removal ended, and the removal Deployments this command ran.
#[derive(serde::Serialize)]
pub(super) struct Removed {
    #[serde(flatten)]
    pub(super) removal: Removal,
    #[serde(rename = "deployments")]
    pub(super) ran: Vec<DeploymentSummary>,
}

impl Removed {
    /// Each line a person reads about it, after the ones `target` names.
    pub(super) fn say(&self, target: &str) {
        for deployment in &self.ran {
            crate::ui::stream(format_args!(
                "Took an Environment off the Servers (Deployment #{}).",
                deployment.number
            ));
        }
        match &self.removal {
            Removal::Removed { residuals } => {
                for residual in residuals {
                    crate::ui::stream(format_args!(
                        "Left {} on old servers: no Server was left to take it off.",
                        residual.environment
                    ));
                }
                crate::ui::stream(format_args!("Removed {target}."));
            }
            Removal::Removing { left, waiting_on } => {
                for left in left {
                    let why = left
                        .why
                        .as_deref()
                        .map(|why| format!(": {why}"))
                        .unwrap_or_default();
                    crate::ui::stream(format_args!(
                        "{} {}{why}",
                        left.environment.name,
                        store::word(&left.status)
                    ));
                }
                for waiting in waiting_on {
                    crate::ui::stream(format_args!(
                        "Waiting on {}: {}",
                        waiting.server, waiting.why
                    ));
                }
                crate::ui::stream(format_args!("Cloud goes on removing {target}."));
            }
        }
    }
}

/// What goes before an Environment of `project` the Store won't remove alone: its
/// first Branch, or for a Default Environment another Default.
fn goes_first(error: &StoreCallError, project: &ProjectName) -> Option<String> {
    let StoreCallError::Refused(refusal) = error else {
        return None;
    };
    if refusal.code != RpcErrorCode::Unsupported {
        return None;
    }
    let branch = refusal
        .details
        .get("branches")
        .and_then(|branches| branches.get(0));
    let words = match branch.and_then(serde_json::Value::as_str) {
        Some(branch) => ["env", "rm", branch],
        None => ["env", "default", "ENV"],
    };
    let project = ["--project", project.as_str()];
    Some(shell_words::join(
        ["ployz"].into_iter().chain(words).chain(project),
    ))
}

/// Whether the Store deleted `environment` of `project`, or the whole Project.
fn gone(
    store: &Store,
    project: &ProjectName,
    environment: Option<&EnvironmentName>,
) -> Result<bool, Error> {
    let projects = store.read(&ProjectsQuery {})?.projects;
    let Some(listing) = projects.iter().find(|listing| &listing.name == project) else {
        return Ok(true);
    };
    Ok(environment.is_some_and(|name| !listing.environments.contains(name)))
}

/// A removal nothing more of can come off now.
fn stalled(project: &ProjectName, environment: Option<&EnvironmentName>, left: &[Left]) -> Error {
    let target = match environment {
        Some(name) => format!("{project}/{name}"),
        None => project.to_string(),
    };
    let why = left
        .iter()
        .find_map(|left| {
            Some(format!(
                ": {} {}",
                left.environment.name,
                left.why.as_deref()?
            ))
        })
        .unwrap_or_default();
    Error::detailed(
        RpcErrorCode::Conflict,
        format!("Nothing more of {target} can come off the Servers now{why}"),
        json!({ "left": left }),
    )
}

/// Whether removal Deployment `deployment` found no Server left to take it off, so
/// whatever ran is still on the Servers that ran it: "Left on old servers".
pub(super) fn left_on_old_servers(deployment: &DeploymentSummary) -> bool {
    matches!(deployment.outcome, Some(ployz_store::Outcome::Forgotten))
}

/// Take Environment `at` off the Servers: admit a removal Deployment under the
/// destructive review, accepting the loss of `accept` as reviewed at `version`, then run or follow it as
/// `deploy` does. A volume-loss refusal names this command again, accepting.
pub(super) fn take_off(
    matches: &ArgMatches,
    store: &Store,
    at: &EnvironmentRef,
    (accept, version): (&[VolumeName], Option<String>),
    events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<(DeploymentView, Result<(), Error>), Error> {
    deploy::refuse_local_detach(matches, store)?;
    let admitted = store
        .admit(&Admit::ShutDown(ShutDown {
            id: DeploymentId::parse(mint())?,
            environment: at.clone(),
            version,
            accept_volume_loss: accept.to_vec(),
        }))
        .map_err(|error| store.accepting(error))?;
    let deploy::Shipped { view, ran, .. } =
        deploy::execute(matches, store, &admitted, None, events)?;
    Ok((view, ran))
}

/// Report a removal Deployment that didn't apply (yet), after the `applied` ones
/// before it, naming `again`, the command that finishes it: exit 3, or 0 when
/// `--detach` asked not to wait.
pub(super) fn unfinished(
    matches: &ArgMatches,
    view: &DeploymentView,
    applied: &[DeploymentSummary],
    ran: Result<(), Error>,
    again: &str,
) -> Result<(), Error> {
    #[derive(serde::Serialize)]
    struct Unfinished<'a> {
        #[serde(flatten)]
        view: &'a DeploymentView,
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        applied: &'a [DeploymentSummary],
    }
    let hint = Some(again.to_owned());
    crate::ui::finish(
        &store::Next::new(&Unfinished { view, applied }, hint),
        || {
            for deployment in applied {
                crate::ui::stream(format_args!("Deployment #{} applied", deployment.number));
            }
            deploy::say_view(view);
        },
    )?;
    ran.and_then(|()| match matches.get_flag("detach") {
        true => Ok(()),
        false => Err(Error::partial()),
    })
}

/// What removing an Environment deletes: its Services and Volumes, by name.
#[derive(serde::Serialize)]
pub(super) struct Inventory {
    pub(super) environment: EnvironmentSummary,
    pub(super) services: Vec<ServiceName>,
    pub(super) volumes: Vec<serde_json::Value>,
}

impl Inventory {
    /// A line for its Services and one for its Volumes, each only when it has some.
    pub(super) fn branches(&self) -> Vec<Tree> {
        let volumes = self
            .volumes
            .iter()
            .filter_map(|volume| volume.get("name")?.as_str())
            .collect::<Vec<_>>();
        [
            ("Services", super::joined(&self.services)),
            ("Volumes", volumes.join(", ")),
        ]
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(kind, names)| Tree::leaf(format!("{kind}: {names}")))
        .collect()
    }
}
