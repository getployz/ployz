use crate::cloud;
use crate::cluster::{Call, Deadline};
use clap::ArgMatches;
use ployz_core::{
    DescribeContractRequest, Hint, LiveServices, Machine, MachineId, MachineTarget,
    QualifiedService, RpcError, RpcErrorCode, op,
};

use super::super::route::Reached;
use super::super::runtime;
use super::forget::{already_forgotten, cloud_forgot, no_cloud_row};
use super::{ConnectionOptions, target};
use crate::approval;
use crate::cli::Rerun;
use crate::cloud::account::{Credential, Release, StoreCallError};
use crate::cloud::login::{CredentialStore, LoginError};
use crate::cluster::{CloudHold, DeadConfirmed, Reply, refuse_last_managed};
use crate::connect::Client;
use crate::context::ConnectionSource;
use crate::drain::{movable_services_on, replicated_services_on, services_on};
use crate::handlers::{
    Error,
    data_loss::{Typed, VolumeLabels, refusal_from_rpc, volume_label},
    leaf_matches, store,
};
use ployz_core::{DataLoss, EnvironmentValues};
use ployz_store::OperationVerb;
use ployz_store::{EnvironmentRef, NamespacesQuery, VolumesQuery, docker_volume};
use serde_json::json;

/// Which command asked. Both take the Server out of the Cluster and revoke its keys,
/// and nothing reaches the Server itself: a removed Server is forgotten.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Removal {
    /// `server rm`.
    Remove,
    /// `server forget <server>`.
    Forget,
}

impl Removal {
    /// The words of the command, in one place.
    fn doing(self) -> &'static str {
        match self {
            Self::Remove => "Removing",
            Self::Forget => "Forgetting",
        }
    }

    fn done(self) -> &'static str {
        match self {
            Self::Remove => "Removed",
            Self::Forget => "Forgot",
        }
    }

    /// How Cloud's progress line names it.
    fn following(self) -> &'static str {
        match self {
            Self::Remove => "removing",
            Self::Forget => "forgetting",
        }
    }

    fn verb(self) -> OperationVerb {
        match self {
            Self::Remove => OperationVerb::Remove,
            Self::Forget => OperationVerb::Forget,
        }
    }
}

pub(in crate::handlers) fn remove(root: &ArgMatches) -> Result<(), Error> {
    leave(root, Removal::Remove)
}

pub(super) fn forget(root: &ArgMatches) -> Result<(), Error> {
    leave(root, Removal::Forget)
}

/// Refuse, before anything is asked or sent, a removal the entry would refuse: the
/// Forget goes to the entry, and needs this CLI there as an approved Device, over SSH, or
/// on the local socket.
fn refuse_without_device(
    client: &Client,
    machines: &[ployz_core::MachineObservation],
    selected: &Machine,
    entry: MachineId,
    removal: Removal,
    matches: &ArgMatches,
) -> Result<(), Error> {
    let device = super::holds_device_key(matches)?;
    let Some(unreached) = (entry != selected.id)
        .then_some(entry)
        .and_then(|id| machines.iter().find(|observed| observed.machine.id == id))
        .filter(|observed| !client.reaches_full(&observed.machine.id, device))
    else {
        return Ok(());
    };
    Err(Error::detailed(
        RpcErrorCode::from(ployz_core::FORBIDDEN_CODE),
        format!(
            "{} Server {} needs an approved device: Server {} refuses it otherwise. Run it \
             from an approved device, or over SSH on a Server. No changes made.",
            removal.doing(),
            selected.name,
            unreached.machine.name
        ),
        json!({
            "capability": ployz_core::FORGET_MACHINE_CAPABILITY,
            "server": { "id": selected.id, "name": selected.name },
            "needs_device_on": unreached.machine.name,
        }),
    ))
}

/// What `selected` is when a removal starts: the Cluster as the entry lists it, the entry
/// the CLI talks to, and what the Cluster holds of the Server.
struct Chosen {
    machines: Vec<ployz_core::MachineObservation>,
    selected: Machine,
    current: MachineId,
    hold: CloudHold,
    reply: Reply,
    by_device: bool,
}

impl Chosen {
    /// The last Server: no other is left to forget it, so the Cluster goes with it.
    fn last(&self) -> bool {
        self.machines.len() == 1
    }

    /// The Server did not answer, so `dead` is what it needs typed.
    fn dead(&self) -> bool {
        matches!(self.reply, Reply::Dead)
    }
}

/// What the Cluster says the removal touches.
struct Surveyed {
    /// Cloud, when it manages the Server and so removes it itself.
    cloud: Option<Credential>,
    observed: ployz_core::ObservedDataLoss,
    services: Vec<QualifiedService>,
    movable: Vec<QualifiedService>,
    short: LeftShort,
}

/// What the operator typed, and the words that retry it.
struct Confirmed {
    confirmation: ployz_core::DataLossConfirmation,
    accepting: Rerun,
    labels: VolumeLabels,
}

/// Where the removal went after the operator confirmed.
enum Started {
    /// Cloud's run to follow, or none when the CLI removes it itself.
    Run(Option<String>),
    /// Cloud has no row for the Server.
    CloudHasNone(RpcError),
}

fn leave(root: &ArgMatches, removal: Removal) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let matches = leaf_matches(root);
    let selector = target(matches, "server")?.to_owned();
    let machine_target = MachineTarget::parse(&selector)
        .map_err(|_| Error::usage("Name a Server by name or ID."))?;
    let runtime = runtime()?;
    // A Cloud that couldn't hand out the connections is retried.
    let reached = runtime
        .block_on(super::super::route::connect_or_unanswered(
            matches,
            options.context(),
        ))
        .map_err(|error| {
            if error.report().code == RpcErrorCode::Unavailable {
                error.hint(Rerun::again(root))
            } else {
                error
            }
        })?;
    let mut client = match reached {
        Reached::Client(client) => client,
        Reached::Unanswered(credential) => {
            return super::forget::forget_unanswered(&runtime, root, &credential, &selector);
        }
    };
    // Before anything is listed or confirmed: a removal that can't happen asks nothing.
    let chosen = runtime.block_on(async {
        let machines = client.machines().await?;
        let observation = crate::cluster::visible_machine(&machine_target, &machines)?;
        let selected = observation.machine.clone();
        let mut current = client
            .call::<op::DescribeContract>(DescribeContractRequest {}, Call::read())
            .await?
            .machine_id;
        // A Server forgets another only for a Full caller, so the CLI drives the removal
        // itself as a Device.
        let by_device = machines.len() > 1;
        let mut switched = false;
        if by_device {
            (client, current, switched) = super::super::route::connect_full_avoiding(
                matches,
                options.context(),
                Some(&selected),
            )
            .await?;
            refuse_without_device(&client, &machines, &selected, current, removal, matches)?;
        }
        let hold = refuse_last_managed(&client, &machines, selected.id).await?;
        let reply = client.reply(observation).await?;
        if let (Reply::Gone(unchecked), true) = (&reply, switched) {
            return Err(unchecked.clone().into());
        }
        Ok::<_, Error>(Chosen {
            machines,
            selected,
            current,
            hold,
            reply,
            by_device,
        })
    })?;
    if let Reply::Gone(_) = &chosen.reply {
        return already_forgotten(&chosen.selected);
    }
    let surveyed = runtime.block_on(survey(removal, matches, &mut client, &chosen))?;
    warn_before(matches, &chosen, &surveyed);
    // The Store reads block on their own runtime, so they run between the two.
    let confirmed = confirm(removal, root, &client, &chosen, &surveyed)?;
    let started = start(removal, &runtime, matches, &chosen, &surveyed, &confirmed)?;
    let run = match started {
        Started::Run(run) => run,
        Started::CloudHasNone(refused) => {
            return runtime.block_on(cloud_forgot(&mut client, &chosen.selected, refused));
        }
    };
    let release = runtime.block_on(depart(
        removal,
        &mut client,
        &chosen,
        &surveyed,
        &confirmed,
        &run,
    ))?;
    report(removal, &options, &chosen, &surveyed, &confirmed, release)
}

async fn survey(
    removal: Removal,
    matches: &ArgMatches,
    client: &mut Client,
    chosen: &Chosen,
) -> Result<Surveyed, Error> {
    let Chosen {
        machines,
        selected,
        current,
        hold,
        by_device,
        ..
    } = chosen;
    let entry = machines
        .iter()
        .find(|observed| observed.machine.id == *current)
        .map_or_else(
            || current.to_string(),
            |observed| observed.machine.name.to_string(),
        );
    let cloud = if *by_device {
        None
    } else if *hold == CloudHold::Last || cloud_manages(&*client, *current, &entry).await? {
        Some(cloud_removal(matches, selected, removal).await?)
    } else {
        None
    };
    // A Server that doesn't answer can't list what it holds.
    let observed = if chosen.dead() {
        ployz_core::ObservedDataLoss {
            data_loss: Vec::new(),
        }
    } else {
        client
            .data_loss_if_machine_removed(&MachineTarget::from(&selected.id))
            .await?
    };
    let live = client
        .live_services_from(machines, EnvironmentValues::Redacted)
        .await?;
    let last = chosen.last();
    let services = services_on(&selected.id, &live);
    let movable = if last {
        Vec::new()
    } else {
        movable_services_on(&selected.id, &live)
    };
    let short = left_short(&selected.id, &live, last);
    Ok(Surveyed {
        cloud,
        observed,
        services,
        movable,
        short,
    })
}

fn warn_before(matches: &ArgMatches, chosen: &Chosen, surveyed: &Surveyed) {
    let selected = &chosen.selected;
    if surveyed.cloud.is_none() && matches.get_one::<String>("approval").is_some() {
        crate::ui::warn("--approval applies only when Ployz Cloud manages the Server.");
    }
    if let Some((warning, drain)) = running_services(
        selected.name.as_str(),
        &surveyed.movable,
        matches.get_one::<String>("confirm").is_some(),
    ) {
        crate::ui::warn(warning);
        if let Some(drain) = drain {
            crate::ui::hint(&drain);
        }
    }
    if chosen.last() {
        crate::ui::warn(format!(
            "Server {} is the last Server: removing it lets go of the Cluster, and nothing deploys until you add a Server.",
            selected.name
        ));
    }
}

fn confirm(
    removal: Removal,
    root: &ArgMatches,
    client: &Client,
    chosen: &Chosen,
    surveyed: &Surveyed,
) -> Result<Confirmed, Error> {
    let selected = &chosen.selected;
    let observed = &surveyed.observed;
    let labels = volume_labels(
        root,
        &observed
            .data_loss
            .iter()
            .map(DataLoss::name)
            .collect::<Vec<_>>(),
    );
    let dead = chosen.dead();
    let unanswered = format!(
        "Server {} did not answer in {}s, so Ployz can't list the Volumes only it holds",
        selected.name,
        crate::connect::target_read_timeout().as_secs()
    );
    if dead && crate::ui::can_prompt() {
        crate::ui::warn(format!("{unanswered}."));
    }
    let (confirmation, accepting) = super::super::data_loss::confirm_removal(
        root,
        client,
        match &surveyed.cloud {
            Some(_) => &ConnectionSource::Cloud,
            None => client.connection_source(),
        },
        observed,
        if dead {
            Typed::dead(selected.name.as_str())
        } else {
            Typed::name(selected.name.as_str())
        },
        &labels,
        |retry| {
            let volumes = if observed.data_loss.is_empty() {
                String::new()
            } else {
                format!(
                    " Server {} holds the only copy of Volumes: {}.",
                    selected.name,
                    observed
                        .data_loss
                        .iter()
                        .map(|loss| volume_label(&labels, loss))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                if dead {
                    format!("{unanswered}, and needs dead typed. No changes made.")
                } else {
                    format!(
                        "{} Server {} needs its name typed.{volumes} No changes made.",
                        removal.doing(),
                        selected.name
                    )
                },
                json!({
                    "server": { "id": selected.id, "name": selected.name },
                    "services": surveyed.services,
                    "data_loss": observed.data_loss,
                    "answered": !dead,
                }),
            )
            // A Server that doesn't answer lists no Volumes, so nothing is left to accept.
            .hint(if dead {
                retry.without("accept-volume-loss")
            } else {
                retry
            })
        },
    )?;
    Ok(Confirmed {
        confirmation,
        accepting,
        labels,
    })
}

/// Cloud's removal, once a human approves it when the Organization wants that.
fn start(
    removal: Removal,
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
    chosen: &Chosen,
    surveyed: &Surveyed,
    confirmed: &Confirmed,
) -> Result<Started, Error> {
    let selected = &chosen.selected;
    let Some(credential) = &surveyed.cloud else {
        return Ok(Started::Run(None));
    };
    let key = uuid::Uuid::new_v4().to_string();
    let started = approval::approved(
        runtime,
        removal.verb(),
        matches.get_one::<String>("approval").cloned(),
        &confirmed.accepting,
        async |approval| match removal {
            Removal::Remove => cloud::account::start_removal(
                credential,
                selected,
                &confirmed.confirmation,
                approval,
                &key,
            )
            .await
            .map(Ok),
            Removal::Forget => {
                match cloud::account::start_forget(credential, selected, approval, &key).await {
                    Err(StoreCallError::Refused(refused)) if no_cloud_row(&refused) => {
                        Ok(Err(refused))
                    }
                    started => started.map(Ok),
                }
            }
        },
    )?;
    Ok(match started {
        Ok(id) => Started::Run(Some(id)),
        Err(refused) => Started::CloudHasNone(refused),
    })
}

/// Take the Server out: Cloud follows its run, or the CLI has another Server forget it.
/// The last Server has no other: without Cloud, only this CLI's context lets go of it.
async fn depart(
    removal: Removal,
    client: &mut Client,
    chosen: &Chosen,
    surveyed: &Surveyed,
    confirmed: &Confirmed,
    run: &Option<String>,
) -> Result<Option<Release>, Error> {
    let selected = &chosen.selected;
    if let (Some(credential), Some(id)) = (&surveyed.cloud, run) {
        let removed =
            cloud::account::follow_removal(credential, &selected.name, removal.following(), id)
                .await?;
        if let Release::Kept { reason } = &removed.release {
            crate::ui::warn(format!("Cloud keeps its hold on the Cluster: {reason}"));
        }
        return Ok(Some(removed.release));
    }
    if chosen.last() {
        return Ok(None);
    }
    // The loss is read again as the removal runs: a Volume whose other copy went since
    // the operator looked needs naming too.
    if !chosen.dead() {
        client
            .data_loss_if_machine_removed(&MachineTarget::from(&selected.id))
            .await?
            .require(&confirmed.confirmation)
            .map_err(|unconfirmed| refusal_from_rpc(unconfirmed.into_rpc_error()))?;
    }
    client
        .remove_membership(
            &MachineTarget::from(&selected.id),
            if chosen.dead() {
                DeadConfirmed::Yes
            } else {
                DeadConfirmed::No
            },
        )
        .await
        .map_err(|error| {
            let late_dead = error.code == RpcErrorCode::ConfirmationRequired;
            let error = Error::from(error);
            if !late_dead {
                return error;
            }
            error.hint(
                confirmed
                    .accepting
                    .clone()
                    .set("confirm", "dead")
                    .without("accept-volume-loss"),
            )
        })?;
    Ok(None)
}

/// Say how it went, before local cleanup can fail: the removal is committed.
fn report(
    removal: Removal,
    options: &ConnectionOptions,
    chosen: &Chosen,
    surveyed: &Surveyed,
    confirmed: &Confirmed,
    release: Option<Release>,
) -> Result<(), Error> {
    let selected = &chosen.selected;
    let Surveyed {
        observed, short, ..
    } = surveyed;
    let cloud_released = release
        .as_ref()
        .map(|release| *release == Release::Released);
    let cloud_kept = release
        .as_ref()
        .map(|release| matches!(release, Release::Kept { .. }));
    crate::ui::stream(format_args!("{} Server {}.", removal.done(), selected.name));
    for loss in &observed.data_loss {
        crate::ui::stream(format_args!(
            "Volume {} was only on {}; the Cluster no longer has it.",
            volume_label(&confirmed.labels, loss),
            selected.name
        ));
    }
    if cloud_released == Some(true) {
        crate::ui::stream(
            "Cloud let go of the Cluster: this Organization has no Server now. Its Environments keep their config; nothing runs until you add a Server.",
        );
    }
    let still_running = chosen.last() && !chosen.dead();
    if cloud_released == Some(true) {
        crate::ui::hint(&Hint::Next("ployz server add".into()));
    }
    for warning in short.warnings() {
        crate::ui::warn(warning);
    }
    if !short.no_copies.is_empty() {
        crate::ui::hint(&Hint::Next("ployz deploy".to_owned()));
    }

    // The removal is committed: print it before local cleanup can fail.
    crate::ui::emit(&json!({
        "server": crate::enrollment::output::server_json(selected),
        "data_loss": observed.data_loss,
        "under_replicated": short.under_replicated,
        "no_copies": short.no_copies,
        "cloud_released": cloud_released,
        "cloud_kept": cloud_kept,
        "still_running": still_running,
        "next": (cloud_released == Some(true)).then_some("ployz server add"),
    }))?;
    let config = options.load_or_empty_config().map_err(|error| {
        Error::from(error).context("Server removed; local context cleanup failed.")
    })?;
    if let Some(context_name) = config.context_name(options.context()).map(str::to_owned)
        && config.contexts.contains_key(&context_name)
    {
        config
            .update(|latest| {
                if let Some(context) = latest.contexts.get_mut(&context_name) {
                    context.drop_machine(&selected.id);
                }
                Ok::<_, Error>(())
            })
            .map_err(|error| error.context("Server removed; local context cleanup failed."))?;
    }
    Ok(())
}

/// Cloud manages this Server, so Cloud removes it through its durable removal and drops
/// its own hold: its row for it, and the Cluster with its last Server. That needs a
/// Cloud credential; without one nothing happens.
async fn cloud_removal(
    matches: &ArgMatches,
    selected: &Machine,
    removal: Removal,
) -> Result<Credential, Error> {
    let store = CredentialStore::beside(&crate::handlers::config_path(matches)?);
    match cloud::account::from_env(&store).await {
        Ok(credential) => Ok(credential),
        Err(LoginError::SignedOut) => Err(Error::coded(
            RpcErrorCode::Conflict,
            format!(
                "Cloud manages Server {}, so Cloud {} it and drops its hold on it. \
                 Sign in to Cloud first, or remove it from the dashboard. No changes made.",
                selected.name,
                match removal {
                    Removal::Remove => "removes",
                    Removal::Forget => "forgets",
                }
            ),
        )
        .hint(Hint::Next("ployz login".into()))),
        Err(error) => Err(error.into()),
    }
}

/// Whether Cloud manages this Cluster: the entry Server, which always answers, holds
/// Cloud's key. Cloud holds every Server it enrolled, so it removes any of them itself
/// and drops its row. Holders that can't be read refuse: they may be Cloud's.
async fn cloud_manages(
    client: &crate::connect::Client,
    entry: MachineId,
    name: &str,
) -> Result<bool, Error> {
    let details = client
        .call_shared::<op::Inspect>(
            ployz_core::InspectRequest::default(),
            Call::once()
                .to(&MachineTarget::from(&entry))
                .within(Deadline::Target(crate::connect::TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)
        .map_err(|error| {
            Error::caused(
                RpcErrorCode::Unavailable,
                format!("Cannot read who manages Server {name}. No changes made."),
                error,
            )
        })?;
    Ok(details
        .management_clients
        .iter()
        .any(ployz_core::ManagementClientLabel::is_cloud))
}

/// The warning that the Server runs Services a drain could move off it, with the drain
/// command only while the removal still asks: a typed `--confirm` goes ahead and leaves
/// no Server to drain.
fn running_services(
    server: &str,
    movable: &[QualifiedService],
    confirmed: bool,
) -> Option<(String, Option<Hint>)> {
    if movable.is_empty() {
        return None;
    }
    Some((
        format!(
            "Server {server} is running Services: {}.",
            super::super::joined(movable)
        ),
        (!confirmed).then(|| Hint::Next(format!("ployz server drain {server}"))),
    ))
}

#[derive(Default)]
struct LeftShort {
    under_replicated: Vec<QualifiedService>,
    /// Those of `under_replicated` with every copy on the removed Server, as a listing
    /// every other Server answered shows.
    no_copies: Vec<QualifiedService>,
}

impl LeftShort {
    /// The removed Server is never told, so it may still run its replicas: only what the
    /// remaining Servers showed is claimed.
    fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if !self.no_copies.is_empty() {
            warnings.push(format!(
                "No copies of Replicated Services were seen on the remaining Servers: {}. Their replicas were not moved.",
                super::super::joined(&self.no_copies)
            ));
        }
        let rest: Vec<&QualifiedService> = self
            .under_replicated
            .iter()
            .filter(|service| !self.no_copies.contains(service))
            .collect();
        if !rest.is_empty() {
            warnings.push(format!(
                "Replicated Services may now be under-replicated: {}. Their replicas were not moved.",
                super::super::joined(&rest)
            ));
        }
        warnings
    }
}

/// The replicated user Services the removal leaves a replica short: none on the last
/// Server, whose removal stops everything.
fn left_short(removed: &MachineId, live: &LiveServices<RpcError>, last: bool) -> LeftShort {
    let mut short = LeftShort::default();
    if last {
        return short;
    }
    let others_answered = live
        .containers
        .failures
        .iter()
        .map(|failure| &failure.machine_id)
        .chain(&live.containers.omissions)
        .all(|machine| machine == removed);
    let services = live.services();
    for service in replicated_services_on(removed, live) {
        if service.namespace.is_system() {
            continue;
        }
        let elsewhere = services
            .iter()
            .filter(|observed| observed.identity == service)
            .flat_map(|observed| &observed.containers)
            .any(|copy| copy.as_observation().machine_id != *removed);
        if others_answered && !elsewhere {
            short.no_copies.push(service.clone());
        }
        short.under_replicated.push(service);
    }
    short
}

/// Each of the `docker` Volumes that keeps a Volume's data, by that Volume's name, so
/// the list and `--accept-volume-loss` speak Volume names. Best effort: without a
/// reachable Store, or for a Docker Volume no Environment owns, the Docker name stays.
fn volume_labels(root: &ArgMatches, docker: &[&str]) -> VolumeLabels {
    let mut owners = Vec::new();
    if docker.is_empty() {
        return VolumeLabels::new();
    }
    let Ok(Some(store)) = store::reachable(root) else {
        return VolumeLabels::new();
    };
    let Ok(owned) = store.try_read(&NamespacesQuery {}) else {
        return VolumeLabels::new();
    };
    for owned in owned.namespaces {
        let prefix = format!("{}_", owned.namespace);
        if !docker.iter().any(|name| name.starts_with(&prefix)) {
            continue;
        }
        let environment = EnvironmentRef {
            project: Some(owned.project.clone()),
            environment: Some(owned.environment.clone()),
        };
        let Ok(view) = store.try_read(&VolumesQuery { environment }) else {
            continue;
        };
        for listing in view.volumes {
            if let Ok(volume) = docker_volume(&owned.namespace, listing.volume.id.as_str())
                && docker.contains(&volume.as_str())
            {
                owners.push(VolumeOwner {
                    docker: volume.to_string(),
                    project: owned.project.to_string(),
                    environment: owned.environment.to_string(),
                    volume: listing.volume.name.to_string(),
                });
            }
        }
    }
    qualified_labels(&owners)
}

/// A Volume some Environment keeps on a Docker Volume.
struct VolumeOwner {
    docker: String,
    project: String,
    environment: String,
    volume: String,
}

/// Each Docker Volume by the shortest name that means one Volume: `VOLUME`, else
/// `ENVIRONMENT/VOLUME`, else `PROJECT/ENVIRONMENT/VOLUME`. So one accepted name never
/// covers a Volume of another Environment.
fn qualified_labels(owners: &[VolumeOwner]) -> VolumeLabels {
    let unique = |name: &dyn Fn(&VolumeOwner) -> String, owner: &VolumeOwner| {
        owners
            .iter()
            .filter(|other| name(other) == name(owner))
            .all(|other| {
                (&other.project, &other.environment) == (&owner.project, &owner.environment)
            })
    };
    let bare = |owner: &VolumeOwner| owner.volume.clone();
    let in_environment = |owner: &VolumeOwner| format!("{}/{}", owner.environment, owner.volume);
    owners
        .iter()
        .map(|owner| {
            let label = if unique(&bare, owner) {
                bare(owner)
            } else if unique(&in_environment, owner) {
                in_environment(owner)
            } else {
                format!("{}/{}/{}", owner.project, owner.environment, owner.volume)
            };
            (owner.docker.clone(), label)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerKind, ContainerObservation, ContainerRuntimeObservation, HealthObservation,
        LiveServices, MachineId, MachineSuccess, PartialResult, QualifiedService, RpcError,
        ServiceId, ServiceMode, ServiceName, derive_live_services,
    };
    use serde_json::json;

    use super::{
        VolumeOwner, left_short, movable_services_on, qualified_labels, replicated_services_on,
        running_services, services_on,
    };
    use ployz_core::Hint;

    #[test]
    fn a_volume_name_two_environments_share_is_qualified_by_environment() {
        let owner = |docker: &str, project: &str, environment: &str, volume: &str| VolumeOwner {
            docker: docker.into(),
            project: project.into(),
            environment: environment.into(),
            volume: volume.into(),
        };
        let labels = qualified_labels(&[
            owner("a_vol-1", "shop", "production", "data"),
            owner("b_vol-2", "shop", "staging", "data"),
            owner("c_vol-3", "blog", "staging", "data"),
            owner("a_vol-4", "shop", "production", "logs"),
        ]);
        assert_eq!(
            labels.get("a_vol-1").map(String::as_str),
            Some("production/data")
        );
        assert_eq!(
            labels.get("b_vol-2").map(String::as_str),
            Some("shop/staging/data")
        );
        assert_eq!(
            labels.get("c_vol-3").map(String::as_str),
            Some("blog/staging/data")
        );
        assert_eq!(labels.get("a_vol-4").map(String::as_str), Some("logs"));
    }

    #[test]
    fn services_on_names_services_with_a_service_container_on_that_machine() {
        let live: LiveServices<RpcError> = derive_live_services(PartialResult {
            successes: vec![
                MachineSuccess {
                    machine_id: machine_id('a'),
                    value: vec![
                        observation('1', 'a', "api", ContainerKind::ServiceContainer, 'a'),
                        observation('2', 'a', "api", ContainerKind::ServiceContainer, 'a'),
                        observation('3', 'a', "hooked", ContainerKind::PreDeployHook, 'b'),
                    ],
                },
                MachineSuccess {
                    machine_id: machine_id('b'),
                    value: vec![observation(
                        '4',
                        'b',
                        "web",
                        ContainerKind::ServiceContainer,
                        'c',
                    )],
                },
            ],
            failures: Vec::new(),
            omissions: Vec::new(),
        });
        assert_eq!(
            services_on(&machine_id('a'), &live),
            [QualifiedService::parse("app/api").unwrap()]
        );
    }

    #[test]
    fn rm_suggests_a_drain_only_for_services_a_drain_can_move() {
        let ordinary = json!({
            "volumes": [{"reference": "data", "source": {"kind": "ordinary", "name": "app_data", "scope": {"namespace": "app", "logical_name": "data"}, "driver": {"name": "local", "options": {}}}}],
            "mounts": [{"volume": "data", "target": "/var/lib/postgresql"}]
        });
        let bind = json!({
            "volumes": [{"reference": "host", "source": {"kind": "bind", "machine_path": "/srv/files"}}],
            "mounts": [{"volume": "host", "target": "/files"}]
        });
        let mut ingress = observation('5', 'a', "ingress", ContainerKind::ServiceContainer, 'e');
        ingress
            .try_update(|parts| {
                parts.namespace = ployz_core::Namespace::parse("ployz-system").unwrap();
            })
            .unwrap();
        let live: LiveServices<RpcError> = derive_live_services(PartialResult {
            successes: vec![
                MachineSuccess {
                    machine_id: machine_id('a'),
                    value: vec![
                        observation('1', 'a', "api", ContainerKind::ServiceContainer, 'a'),
                        pinned('2', 'a', "db", 'b', &ordinary),
                        pinned('3', 'a', "files", 'c', &bind),
                        ingress,
                    ],
                },
                MachineSuccess {
                    machine_id: machine_id('b'),
                    value: vec![observation(
                        '4',
                        'b',
                        "api",
                        ContainerKind::ServiceContainer,
                        'a',
                    )],
                },
            ],
            failures: Vec::new(),
            omissions: Vec::new(),
        });
        assert_eq!(
            movable_services_on(&machine_id('a'), &live),
            [QualifiedService::parse("app/api").unwrap()]
        );
    }

    #[test]
    fn rm_warns_of_running_services_but_suggests_a_drain_only_while_it_still_asks() {
        let web = [QualifiedService::parse("app/web").unwrap()];
        let warning = "Server gamma is running Services: app/web.".to_owned();
        assert_eq!(
            running_services("gamma", &web, false),
            Some((
                warning.clone(),
                Some(Hint::Next("ployz server drain gamma".into()))
            ))
        );
        assert_eq!(running_services("gamma", &web, true), Some((warning, None)));
        assert_eq!(running_services("gamma", &[], false), None);
    }

    fn api_only_on_a_web_on_both() -> LiveServices<RpcError> {
        derive_live_services(PartialResult {
            successes: vec![
                MachineSuccess {
                    machine_id: machine_id('a'),
                    value: vec![
                        observation('1', 'a', "api", ContainerKind::ServiceContainer, 'a'),
                        observation('2', 'a', "web", ContainerKind::ServiceContainer, 'b'),
                    ],
                },
                MachineSuccess {
                    machine_id: machine_id('b'),
                    value: vec![observation(
                        '3',
                        'b',
                        "web",
                        ContainerKind::ServiceContainer,
                        'b',
                    )],
                },
            ],
            failures: Vec::new(),
            omissions: Vec::new(),
        })
    }

    #[test]
    fn rm_keeps_every_short_service_under_replicated_and_names_those_with_no_copies() {
        let short = left_short(&machine_id('a'), &api_only_on_a_web_on_both(), false);
        assert_eq!(
            short.no_copies,
            [QualifiedService::parse("app/api").unwrap()]
        );
        assert_eq!(
            short.under_replicated,
            [
                QualifiedService::parse("app/api").unwrap(),
                QualifiedService::parse("app/web").unwrap()
            ]
        );
    }

    #[test]
    fn a_removal_that_left_copies_running_says_only_what_the_remaining_servers_showed() {
        let short = left_short(&machine_id('a'), &api_only_on_a_web_on_both(), false);
        assert_eq!(
            short.warnings(),
            [
                "No copies of Replicated Services were seen on the remaining Servers: app/api. Their replicas were not moved.",
                "Replicated Services may now be under-replicated: app/web. Their replicas were not moved.",
            ]
        );
    }

    #[test]
    fn rm_claims_no_copies_only_when_every_other_server_answered() {
        let mut live = api_only_on_a_web_on_both();
        live.containers.omissions.push(machine_id('c'));
        let short = left_short(&machine_id('a'), &live, false);
        assert!(short.no_copies.is_empty());
        assert_eq!(
            short.under_replicated,
            [
                QualifiedService::parse("app/api").unwrap(),
                QualifiedService::parse("app/web").unwrap()
            ]
        );
    }

    #[test]
    fn the_last_server_leaves_nothing_short() {
        let mut live = api_only_on_a_web_on_both();
        live.containers.successes.truncate(1);
        let short = left_short(&machine_id('a'), &live, true);
        assert!(short.no_copies.is_empty());
        assert!(short.under_replicated.is_empty());
    }

    #[test]
    fn replicated_services_on_excludes_global_services() {
        let live: LiveServices<RpcError> = derive_live_services(PartialResult {
            successes: vec![MachineSuccess {
                machine_id: machine_id('a'),
                value: vec![
                    observation('1', 'a', "api", ContainerKind::ServiceContainer, 'a'),
                    {
                        let mut global =
                            observation('2', 'a', "caddy", ContainerKind::ServiceContainer, 'b');
                        global
                            .try_update(|parts| parts.resolved_spec.mode = ServiceMode::Global)
                            .unwrap();
                        global
                    },
                ],
            }],
            failures: Vec::new(),
            omissions: Vec::new(),
        });
        assert_eq!(
            replicated_services_on(&machine_id('a'), &live),
            [QualifiedService::parse("app/api").unwrap()]
        );
    }

    fn observation(
        id: char,
        machine: char,
        name: &str,
        kind: ContainerKind,
        service: char,
    ) -> ContainerObservation {
        observation_with(id, machine, name, kind, service, &json!({}))
    }

    /// A Service Container whose spec mounts `storage`'s Volumes.
    fn pinned(
        id: char,
        machine: char,
        name: &str,
        service: char,
        storage: &serde_json::Value,
    ) -> ContainerObservation {
        observation_with(
            id,
            machine,
            name,
            ContainerKind::ServiceContainer,
            service,
            storage,
        )
    }

    fn observation_with(
        id: char,
        machine: char,
        name: &str,
        kind: ContainerKind,
        service: char,
        storage: &serde_json::Value,
    ) -> ContainerObservation {
        let service_id = ServiceId::parse(service.to_string().repeat(32)).unwrap();
        let service_name = ServiceName::parse(name).unwrap();
        ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
            rotation: None,
            r#gen: None,
            container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: name.into(),
            created_at_unix_nanos: 0,
            machine_id: machine_id(machine),
            namespace: ployz_core::Namespace::parse("app").unwrap(),
            kind,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec: {
                let mut spec = json!({
                    "service_id": service_id,
                    "name": service_name,
                    "mode": { "mode": "replicated", "replicas": 1 },
                    "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
                });
                if let (Some(spec), Some(storage)) = (spec.as_object_mut(), storage.as_object()) {
                    spec.extend(storage.clone());
                }
                serde_json::from_value(spec).unwrap()
            },
            address: None,
            labels: Default::default(),
            stopped: false,
            run: None,
            log_driver: None,
            admission: None,
            replaced_by: None,
        })
        .unwrap()
    }

    fn machine_id(value: char) -> MachineId {
        MachineId::parse(value.to_string().repeat(32)).unwrap()
    }
}
