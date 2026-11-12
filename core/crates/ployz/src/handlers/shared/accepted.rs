use clap::ArgMatches;
use ployz_core::Hint;
use ployz_store::{
    Admit, DeploymentId, DeploymentSummary, DeploymentView, EnvironmentRef, ShutDown, VolumeName,
};

use super::super::Error;
use super::super::deploy;
use super::super::store::{self, Store, mint};

/// Every `--accept-volume-loss` name.
pub(in crate::handlers) fn accepted(matches: &ArgMatches) -> Result<Vec<VolumeName>, Error> {
    store::volume_names(matches, "accept-volume-loss")
}

/// `--accept-volume-loss` for each of `accept`, the words a retry repeats.
pub(in crate::handlers) fn with_accepted(accept: &[VolumeName]) -> Vec<&str> {
    accept
        .iter()
        .flat_map(|name| ["--accept-volume-loss", name.as_str()])
        .collect()
}

/// Whether removal Deployment `deployment` found no Server left to take it off, so
/// whatever ran is still on the Servers that ran it: "Left on old servers".
pub(in crate::handlers) fn left_on_old_servers(deployment: &DeploymentSummary) -> bool {
    matches!(deployment.outcome, Some(ployz_store::Outcome::Forgotten))
}

/// Take Environment `at` off the Servers: admit a removal Deployment under the
/// destructive review, accepting the loss of `accept` as reviewed at `version`, then run or follow it as
/// `deploy` does. A volume-loss refusal names this command again, accepting.
pub(in crate::handlers) fn take_off(
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

/// Report a shutdown Deployment that didn't apply (yet), naming `again`, the
/// command that finishes it: exit 3, or 0 when `--detach` asked not to wait.
pub(in crate::handlers) fn unfinished(
    matches: &ArgMatches,
    view: &DeploymentView,
    ran: Result<(), Error>,
    again: &str,
) -> Result<(), Error> {
    crate::ui::finish(&crate::ui::Next::new(view, Some(again.to_owned())), || {
        deploy::say_view(view);
    })?;
    not_applied(matches, ran, again.to_owned())
}

/// How a command ends after a removal Deployment didn't apply: how running it
/// failed, naming `again`, the command that finishes it; else exit 3, or 0 when
/// `--detach` asked not to wait.
pub(in crate::handlers) fn not_applied(
    matches: &ArgMatches,
    ran: Result<(), Error>,
    again: String,
) -> Result<(), Error> {
    ran.map_err(|error| error.hint(Hint::Retry(again)))
        .and_then(|()| match matches.get_flag("detach") {
            true => Ok(()),
            false => Err(Error::partial()),
        })
}
