//! Removing what ran: the typed confirmation, what goes, and the removal Deployment
//! that takes an Environment off the Servers first. `env rm` and `project rm` share it.

use clap::ArgMatches;
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Admit, DeploymentId, DeploymentStatus, DeploymentSummary, DeploymentView, EnvironmentRef,
    EnvironmentSummary, ProjectName, RemovalsQuery, ServicesQuery, Teardown, Tell, VolumeName,
    VolumesQuery,
};
use serde_json::json;

use super::Error;
use super::deploy;
use super::store::{self, Store, failed, mint};
use crate::failure::USAGE_EXIT;

/// Whether `--confirm` typed `name`; a different name is a usage error.
pub(super) fn confirmed(matches: &ArgMatches, name: &str, what: &str) -> Result<bool, Error> {
    match matches.get_one::<String>("confirm") {
        Some(typed) if typed == name => Ok(true),
        Some(typed) => Err(Error::usage(format!(
            "--confirm {} does not match {what} {name}. No changes made.",
            typed.escape_debug()
        ))
        .with_exit(USAGE_EXIT)),
        None => Ok(false),
    }
}

pub(super) fn inventory(
    matches: &ArgMatches,
    store: &Store,
    at: &EnvironmentRef,
) -> Result<Inventory, Error> {
    let words = ["env", "rm"];
    let services = store
        .read(&ServicesQuery {
            environment: at.clone(),
        })
        .map_err(failed(matches, &words))?;
    let volumes = store
        .read(&VolumesQuery {
            environment: at.clone(),
        })
        .map_err(failed(matches, &words))?;
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
    matches
        .get_many::<String>("accept-volume-loss")
        .into_iter()
        .flatten()
        .map(|name| {
            VolumeName::parse(name.as_str()).map_err(|_| {
                Error::usage("Expected Volume names: lowercase letters, digits and -")
                    .with_exit(USAGE_EXIT)
            })
        })
        .collect()
}

/// Run `remove` until the Store deletes what it names: each Environment of `project`
/// it names as still on the Servers goes off them first through a removal Deployment,
/// accepting the loss of the `accept` Volumes it deletes. Returns what was removed and
/// those Deployments; `None` once a removal that didn't apply was reported (`again`
/// finishes it).
pub(super) fn remove_all<C, T>(
    matches: &ArgMatches,
    store: &Store,
    remove: &C,
    project: &ProjectName,
    mut events: Option<std::io::BufWriter<std::fs::File>>,
    words: &[&str],
    again: &[&str],
) -> Result<Option<(T, Vec<DeploymentSummary>)>, Error>
where
    C: Tell<Written = Teardown<T>>,
{
    let accept = accepted(matches)?;
    // The reviewed version binds the first removal, the one whose refusal named it.
    let mut version = matches.get_one::<String>("expect-version").cloned();
    let mut ran = Vec::new();
    loop {
        let environment = match store.write(remove).map_err(failed(matches, words))? {
            Teardown::Removed(removed) => return Ok(Some((removed, ran))),
            Teardown::Waiting {
                environment,
                deployment,
            } => {
                return Err(Error::detailed(
                    RpcErrorCode::Conflict,
                    format!(
                        "A Deployment of {environment} hasn't ended: wait for it or cancel it first"
                    ),
                    json!({
                        "environment": environment,
                        "deployment": deployment,
                        "next": shell_words::join(["ployz", "deployment", "show", deployment.as_str()]),
                    }),
                ));
            }
            Teardown::NeedsRemoval { environment, .. } => environment,
        };
        let at = EnvironmentRef {
            project: Some(project.clone()),
            environment: Some(environment),
        };
        // Each removal accepts only the Volumes it deletes; a name may recur across Environments.
        let deletes = store
            .read(&RemovalsQuery {
                environment: at.clone(),
                remove: true,
            })
            .map_err(failed(matches, words))?;
        let accept: Vec<_> = accept
            .iter()
            .filter(|name| deletes.volumes.iter().any(|volume| &volume.name == *name))
            .cloned()
            .collect();
        let writer = events
            .as_mut()
            .map(|writer| writer.get_ref().try_clone())
            .transpose()?
            .map(std::io::BufWriter::new);
        let (view, outcome) = take_off(
            matches,
            store,
            &at,
            (&accept, version.take()),
            writer,
            words,
            again,
        )?;
        if view.deployment.status != DeploymentStatus::Applied {
            unfinished(matches, &view, outcome, again)?;
            return Ok(None);
        }
        ran.push(view.deployment);
    }
}

/// Take Environment `at` off the Servers: admit a removal Deployment under the
/// destructive review, accepting the loss of `accept` as reviewed at `version`, then run or follow it as
/// `deploy` does. `again` is the command that retries the whole removal.
pub(super) fn take_off(
    matches: &ArgMatches,
    store: &Store,
    at: &EnvironmentRef,
    (accept, version): (&[VolumeName], Option<String>),
    events: Option<std::io::BufWriter<std::fs::File>>,
    words: &[&str],
    again: &[&str],
) -> Result<(DeploymentView, Result<(), Error>), Error> {
    // The in-process Store trusts this CLI to observe the Servers; Cloud observes them itself.
    let volumes = match store.local() {
        Some(_) => deploy::observe(matches, store, at, true)?,
        None => None,
    };
    let admitted = store
        .admit(
            &Admit::Remove(ployz_store::Removal {
                id: DeploymentId::parse(mint())?,
                environment: at.clone(),
                version,
                accept_volume_loss: accept.to_vec(),
            }),
            volumes,
        )
        .map_err(|error| failed(matches, words)(deploy::accepting(error, matches, again)))?;
    let deploy::Shipped { view, ran, .. } =
        deploy::execute(matches, store, &admitted, None, events, words)?;
    Ok((view, ran))
}

/// Report a removal Deployment that didn't apply (yet), naming `again` to finish
/// it: exit 3, or 0 when `--detach` asked not to wait.
pub(super) fn unfinished(
    matches: &ArgMatches,
    view: &DeploymentView,
    ran: Result<(), Error>,
    again: &[&str],
) -> Result<(), Error> {
    deploy::finish_view(view, Some(store::next(matches, again)))?;
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
