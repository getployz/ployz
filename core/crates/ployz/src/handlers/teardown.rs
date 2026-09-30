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
use super::store::{self, Store, mint};
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

/// Run `remove` until the Store deletes what it names: each Environment of `project`
/// it names as still on the Servers goes off them first through a removal Deployment,
/// accepting the loss of the `--accept-volume-loss` Volumes it deletes. Returns what
/// was removed and those Deployments; `None` once a removal that didn't apply was
/// reported (running the command again finishes it).
pub(super) fn remove_all<C, T>(
    matches: &ArgMatches,
    store: &Store,
    remove: &C,
    project: &ProjectName,
    mut events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<Option<(T, Vec<DeploymentSummary>)>, Error>
where
    C: Tell<Written = Teardown<T>>,
{
    let accept = accepted(matches)?;
    let again = store.again(&with_accepted(&accept));
    // The reviewed version binds the first removal, the one whose refusal named it.
    let mut version = matches.get_one::<String>("expect-version").cloned();
    let mut ran = Vec::new();
    loop {
        let environment = match store.write(remove)? {
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
                        "next": deploy::show_hint(&deployment),
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
        let deletes = store.read(&RemovalsQuery {
            environment: at.clone(),
            remove: true,
        })?;
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
        )?;
        if view.deployment.status != DeploymentStatus::Applied {
            unfinished(matches, &view, &ran, outcome, &again)?;
            return Ok(None);
        }
        ran.push(view.deployment);
    }
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
    let admitted = store
        .admit(&Admit::Remove(ployz_store::Removal {
            id: DeploymentId::parse(mint())?,
            environment: at.clone(),
            version,
            accept_volume_loss: accept.to_vec(),
            close: false,
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
    crate::output::finish(
        &store::Next::new(&Unfinished { view, applied }, hint),
        || {
            for deployment in applied {
                crate::output::say!("Deployment #{} applied", deployment.number);
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
