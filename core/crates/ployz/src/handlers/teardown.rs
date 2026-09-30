//! Removing what ran: the typed confirmation, what goes, and the removal Deployment
//! that takes an Environment off the Servers first. `env rm` and `project rm` share it.

use clap::ArgMatches;
use ployz_core::ServiceName;
use ployz_store::{
    Admit, DeploymentId, DeploymentView, EnvironmentRef, EnvironmentSummary, ServicesQuery,
    VolumeName, VolumesQuery,
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
        .services(&ServicesQuery {
            environment: at.clone(),
        })
        .map_err(failed(matches, &words))?;
    let volumes = store
        .volumes(&VolumesQuery {
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

/// Take Environment `at` off the Servers: admit a removal Deployment under the
/// destructive review, accepting the loss of `accept`, then run or follow it as
/// `deploy` does. `again` is the command that retries the whole removal.
pub(super) fn take_off(
    matches: &ArgMatches,
    store: &Store,
    at: &EnvironmentRef,
    accept: &[VolumeName],
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
                version: None,
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
