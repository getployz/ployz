use crate::cluster::{Call, Deadline};
use clap::ArgMatches;
use ployz_core::error_chain;
use ployz_core::{
    DescribeContractRequest, Hint, LiveServices, Machine, MachineId, MachineTarget,
    QualifiedService, RpcError, RpcErrorCode, op,
};

use super::super::route::Reached;
use super::super::runtime;
use super::fence::{Forgotten, already_forgotten};
use super::forget::{cloud_forgot, no_cloud_row};
use super::{ConnectionOptions, target};
use crate::approval;
use crate::cloud_account::{self, Credential, Release, StoreCallError};
use crate::cloud_login::{CredentialStore, LoginError};
use crate::cluster::{
    CloudHold, DeadConfirmed, HeldCopies, LeftNotForgotten, copies_left_behind, refuse_last_managed,
};
use crate::connect::{Client, Remover};
use crate::context::ConnectionSource;
use crate::drain::{movable_services_on, replicated_services_on, services_on};
use crate::handlers::{
    Error,
    data_loss::{Typed, VolumeEffect, VolumeLabels, volume_label},
    leaf_matches, store, typed_with,
};
use ployz_core::{DataLoss, Departed, DockerVolumeName, EnvironmentValues};
use ployz_store::OperationVerb;
use ployz_store::{EnvironmentRef, NamespacesQuery, VolumesQuery, docker_volume};
use serde_json::json;

/// How a Server leaves the Cluster.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Removal {
    /// `server rm`: reset it, accepting the Cluster's loss of its Volumes.
    Reset,
    /// `server forget <server>`: take it out without a reset. Refused while it answers
    /// and holds a Volume copy; one that does not answer needs `dead` typed.
    Forget,
}

impl Removal {
    /// The words of the mode, in one place.
    fn doing(self) -> &'static str {
        match self {
            Self::Reset => "Removing",
            Self::Forget => "Forgetting",
        }
    }

    fn done(self) -> &'static str {
        match self {
            Self::Reset => "Removed",
            Self::Forget => "Forgot",
        }
    }

    /// How Cloud's progress line names it.
    fn following(self) -> &'static str {
        match self {
            Self::Reset => "removing",
            Self::Forget => "forgetting",
        }
    }

    fn capability(self) -> &'static str {
        match self {
            Self::Reset => ployz_core::LEAVE_MACHINE_CAPABILITY,
            Self::Forget => ployz_core::FORGET_MACHINE_CAPABILITY,
        }
    }

    fn verb(self) -> OperationVerb {
        match self {
            Self::Reset => OperationVerb::Remove,
            Self::Forget => OperationVerb::Forget,
        }
    }

    fn effect(self) -> VolumeEffect {
        match self {
            Self::Reset => VolumeEffect::LoseAccess,
            Self::Forget => VolumeEffect::Preserve,
        }
    }

    /// Reach the Cluster. A reset needs its Server to answer; a forget of one that does not
    /// answer goes to Cloud.
    async fn reach(
        self,
        matches: &ArgMatches,
        options: &ConnectionOptions,
        selector: &str,
    ) -> Result<Reached, Error> {
        match self {
            Self::Forget => {
                super::super::route::connect_or_unanswered(matches, options.context()).await
            }
            Self::Reset => super::super::route::connect(matches, options.context())
                .await
                .map(Reached::Client)
                .map_err(|error| unanswered_reset(error, matches, selector)),
        }
    }

    /// The Volume copies a forget finds on the Server, before anything is asked.
    async fn held(
        self,
        client: &Client,
        observation: &ployz_core::MachineObservation,
        switched: bool,
    ) -> Result<Option<HeldCopies>, Error> {
        match self {
            Self::Forget => match client.held_copies(observation).await? {
                HeldCopies::Gone(unchecked) if switched => Err(unchecked.into()),
                held @ (HeldCopies::Dead | HeldCopies::Answered(_) | HeldCopies::Gone(_)) => {
                    Ok(Some(held))
                }
            },
            Self::Reset => Ok(None),
        }
    }

    /// The Volumes whose data the Cluster loses: a forget leaves every copy where it is.
    async fn data_loss(
        self,
        client: &mut Client,
        target: &MachineTarget,
        selected: &Machine,
    ) -> Result<ployz_core::ObservedDataLoss, Error> {
        match self {
            Self::Forget => Ok(ployz_core::ObservedDataLoss {
                data_loss: Vec::new(),
            }),
            Self::Reset => client
                .data_loss_if_machine_removed(target)
                .await
                .map_err(|error| machine_removal_refusal(error, selected.name.as_str())),
        }
    }
}

pub(in crate::handlers) fn remove(root: &ArgMatches) -> Result<(), Error> {
    leave(root, Removal::Reset)
}

fn enforces(observation: &ployz_core::MachineObservation) -> bool {
    observation.machine.runtime.device_enforcement == Some(true)
}

/// Refuse, before anything is asked or sent, a removal an enforcing Server would refuse:
/// the Leave goes to the leaving Server and the Forget to the entry, and each needs this
/// CLI there as an approved Device, over SSH, or on the local socket.
fn refuse_without_device(
    client: &Client,
    machines: &[ployz_core::MachineObservation],
    selected: &Machine,
    entry: MachineId,
    removal: Removal,
    matches: &ArgMatches,
) -> Result<(), Error> {
    let device = super::holds_device_key(matches)?;
    let leave = (removal == Removal::Reset).then_some(selected.id);
    let forget = (entry != selected.id).then_some(entry);
    let Some(unreached) = leave
        .into_iter()
        .chain(forget)
        .filter_map(|id| machines.iter().find(|observed| observed.machine.id == id))
        .find(|observed| enforces(observed) && !client.reaches_full(&observed.machine.id, device))
    else {
        return Ok(());
    };
    let (capability, verb) = (removal.capability(), removal.doing());
    Err(Error::detailed(
        RpcErrorCode::from(ployz_core::FORBIDDEN_CODE),
        format!(
            "{verb} Server {} needs an approved device: Server {} refuses it otherwise. Run it \
             from an approved device, or over SSH on a Server. No changes made.",
            selected.name, unreached.machine.name
        ),
        json!({
            "capability": capability,
            "server": { "id": selected.id, "name": selected.name },
            "needs_device_on": unreached.machine.name,
        }),
    ))
}

/// A reset needs its Server to answer. Servers that didn't answer may be gone, and one that
/// is gone is forgotten instead; a Cloud that couldn't hand out the connections is retried.
/// A Server's own refusal says what is wrong with it.
fn unanswered_reset(error: Error, matches: &ArgMatches, selector: &str) -> Error {
    if super::super::route::refused(&error) {
        return error;
    }
    if super::super::route::servers_unanswered(&error) {
        let forget = super::super::rerun(
            matches,
            &["server", "forget", selector, "--confirm", "dead"],
        );
        return error.hint(Hint::Next(forget));
    }
    if error.report().code == RpcErrorCode::Unavailable {
        return error.hint(Hint::Retry(typed_with(&[])));
    }
    error
}

pub(super) fn forget(root: &ArgMatches) -> Result<(), Error> {
    leave(root, Removal::Forget)
}

/// What `selected` is when a removal starts: the Cluster as the entry lists it, the entry
/// the CLI talks to, and what the Cluster holds of the Server.
struct Chosen {
    machines: Vec<ployz_core::MachineObservation>,
    selected: Machine,
    current: MachineId,
    hold: CloudHold,
    /// The Volume copies a forget finds on the Server. None for a reset, which demotes them.
    held: Option<HeldCopies>,
    by_device: bool,
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
    accepting: Vec<String>,
    labels: VolumeLabels,
    /// The Server did not answer, so `dead` is what it needs typed.
    dead: bool,
}

/// How the Server left.
struct Left {
    reset_failure: Option<String>,
    departed: Option<Departed>,
    cloud_released: Option<bool>,
    cloud_kept: Option<bool>,
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
    let reached = runtime.block_on(removal.reach(matches, &options, &selector))?;
    let mut client = match reached {
        Reached::Client(client) => client,
        Reached::Unanswered(credential) => {
            return super::forget::forget_unanswered(&runtime, matches, &credential, &selector);
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
        // An enforcing Server leaves, and forgets another, only for a Full caller, so the
        // CLI drives the removal itself as a Device. The last Server waits on teardown.
        let by_device = machines.len() > 1 && machines.iter().any(enforces);
        let mut switched = selected.id == current && machines.len() > 1;
        if by_device {
            (client, current, switched) = super::super::route::connect_full_avoiding(
                matches,
                options.context(),
                Some(&selected),
            )
            .await?;
            refuse_without_device(&client, &machines, &selected, current, removal, matches)?;
        } else if switched {
            (client, current) =
                super::super::route::connect_avoiding(matches, options.context(), &selected)
                    .await?;
        }
        let hold = refuse_last_managed(&client, &machines, selected.id).await?;
        let held = removal.held(&client, observation, switched).await?;
        Ok::<_, Error>(Chosen {
            machines,
            selected,
            current,
            hold,
            held,
            by_device,
        })
    })?;
    if let Some(HeldCopies::Gone(_)) = &chosen.held {
        return already_forgotten(&chosen.selected);
    }
    refuse_held_copies(root, &chosen)?;
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
    let left = runtime.block_on(depart(
        removal,
        &mut client,
        &chosen,
        &surveyed,
        &confirmed,
        &run,
    ))?;
    report(removal, &options, &chosen, &surveyed, &confirmed, &left)
}

/// A Server that answers and holds a Volume copy is not forgotten: the copy would be
/// left behind, so a reset demotes it instead.
fn refuse_held_copies(root: &ArgMatches, chosen: &Chosen) -> Result<(), Error> {
    if let Some(HeldCopies::Answered(held)) = &chosen.held
        && !held.is_empty()
    {
        let docker = held
            .iter()
            .map(DockerVolumeName::as_str)
            .collect::<Vec<_>>();
        let labels = volume_labels(root, &docker);
        return Err(copies_left_behind(
            &chosen.selected.name,
            docker
                .iter()
                .map(|name| labels.get(*name).map_or(*name, String::as_str)),
        )
        .into());
    }
    Ok(())
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
    let selected_target = MachineTarget::from(&selected.id);
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
        Some(cloud_removal(matches, selected, *hold, removal).await?)
    } else {
        None
    };
    let observed = removal
        .data_loss(client, &selected_target, selected)
        .await?;
    let live = client
        .live_services_from(machines, EnvironmentValues::Redacted)
        .await?;
    if removal == Removal::Reset {
        if let Some(failure) = live
            .containers
            .failures
            .iter()
            .find(|failure| failure.machine_id == selected.id)
        {
            return Err(Error::caused(
                RpcErrorCode::Unavailable,
                format!(
                    "Cannot observe Services on Server {}. No changes made.",
                    selected.name
                ),
                failure.error.clone(),
            ));
        }
        if live.containers.omissions.contains(&selected.id) {
            return Err(Error::unavailable(format!(
                "Cannot observe Services on Server {}: no terminal response. No changes made.",
                selected.name
            )));
        }
    }
    let last = machines.len() == 1;
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
    if chosen.hold == CloudHold::Last {
        crate::ui::warn(format!(
            "Server {} is the last Server: whatever runs on it stops, and nothing runs until you add a Server.",
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
    let dead = matches!(chosen.held, Some(HeldCopies::Dead));
    let unanswered = format!(
        "Server {} did not answer in {}s. Forgetting it leaves any Volume copies on it behind",
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
        removal.effect(),
        &labels,
        |retry| {
            let volumes = if removal == Removal::Reset && !observed.data_loss.is_empty() {
                format!(
                    " Server {} holds Volumes: {}.",
                    selected.name,
                    observed
                        .data_loss
                        .iter()
                        .map(|loss| volume_label(&labels, loss))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                String::new()
            };
            Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                if dead {
                    format!("{unanswered} and needs dead typed. No changes made.")
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
            .hint(Hint::Retry(retry))
        },
    )?;
    Ok(Confirmed {
        confirmation,
        accepting,
        labels,
        dead,
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
    let started = approval::approved(
        runtime,
        credential,
        removal.verb(),
        matches.get_one::<String>("approval").cloned(),
        |id| {
            let mut command = confirmed.accepting.clone();
            command.extend(["--approval".into(), id.to_owned()]);
            shell_words::join(command)
        },
        async |approval| match removal {
            Removal::Reset => cloud_account::start_removal(
                credential,
                selected,
                &confirmed.confirmation,
                approval,
            )
            .await
            .map(Ok),
            Removal::Forget => match cloud_account::start_forget(credential, selected, approval)
                .await
            {
                Err(StoreCallError::Refused(refused)) if no_cloud_row(&refused) => Ok(Err(refused)),
                started => started.map(Ok),
            },
        },
    )?;
    Ok(match started {
        Ok(id) => Started::Run(Some(id)),
        Err(refused) => Started::CloudHasNone(refused),
    })
}

/// Take the Server out: Cloud follows its run, or the CLI asks the Cluster.
async fn depart(
    removal: Removal,
    client: &mut Client,
    chosen: &Chosen,
    surveyed: &Surveyed,
    confirmed: &Confirmed,
    run: &Option<String>,
) -> Result<Left, Error> {
    let selected = &chosen.selected;
    let selected_target = MachineTarget::from(&selected.id);
    let mut left = Left {
        reset_failure: None,
        departed: None,
        cloud_released: None,
        cloud_kept: None,
    };
    // TODO: do not reroute away from the current entry before removal.
    if let (Some(credential), Some(id)) = (&surveyed.cloud, run) {
        let removed =
            cloud_account::follow_removal(credential, &selected.name, removal.following(), id)
                .await?;
        left.reset_failure = removed.reset_warning;
        left.departed = removed.departed;
        if let Release::Kept { reason } = &removed.release {
            crate::ui::warn(format!("Cloud keeps its hold on the Cluster: {reason}"));
        }
        left.cloud_kept = Some(matches!(removed.release, Release::Kept { .. }));
        left.cloud_released = Some(removed.release == Release::Released);
    } else if removal == Removal::Forget {
        client
            .remove_membership(
                &selected_target,
                if confirmed.dead {
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
                let mut retry = confirmed.accepting.clone();
                if let Some(at) = retry.iter().position(|word| word == "--confirm") {
                    retry.truncate(at);
                }
                retry.extend(["--confirm".into(), "dead".into()]);
                error.hint(Hint::Retry(shell_words::join(retry)))
            })?;
    } else {
        let removed = client
            .remove_machine(&selected_target, &confirmed.confirmation, Remover::Operator)
            .await
            .map_err(|error| reset_removal_refusal(error, selected.name.as_str()))?;
        left.reset_failure = removed.reset_warning().map(str::to_owned);
        left.departed = Some(removed.departed().clone());
    }
    Ok(left)
}

/// Say how it went, before local cleanup can fail: the removal is committed.
fn report(
    removal: Removal,
    options: &ConnectionOptions,
    chosen: &Chosen,
    surveyed: &Surveyed,
    confirmed: &Confirmed,
    left: &Left,
) -> Result<(), Error> {
    let selected = &chosen.selected;
    let Surveyed {
        observed, short, ..
    } = surveyed;
    let Left {
        reset_failure,
        departed,
        cloud_released,
        cloud_kept,
    } = left;
    // A Server that reset leaves no doubt, though its own row removal failed after. An
    // older Cloud doesn't say how it departed, so its warning still reads as doubt.
    let reset_doubt = match departed {
        Some(Departed::Reset { .. }) => None,
        _ => reset_failure.as_deref(),
    };
    crate::ui::stream(format_args!("{} Server {}.", removal.done(), selected.name));
    if removal == Removal::Forget {
        if confirmed.dead {
            Forgotten::Dead(&selected.name).warn();
        } else {
            Forgotten::Answering(&selected.name).warn();
        }
    }
    if *cloud_released == Some(true) {
        crate::ui::stream(
            "Cloud let go of the Cluster: this Organization has no Server now. Its Environments keep their config; nothing runs until you add a Server.",
        );
        crate::ui::hint(&Hint::Next("ployz server add".into()));
    }
    if let Some(reason) = reset_doubt
        && *cloud_kept == Some(false)
    {
        // Cloud lets go only of a Server the Cluster already forgot, so only its reset is in doubt.
        crate::ui::warn(error_chain::then_sentence(
            &format!("Cloud let go of Server {}, but {reason}", selected.name),
            "Reset does not erase volume data.",
        ));
    } else if let Some(reason) = reset_doubt {
        crate::ui::warn(error_chain::then_sentence(
            &error_chain::with_reason(
                format_args!("Server {} was not fully cleaned up or reset", selected.name),
                reason,
            ),
            "Reset does not erase volume data.",
        ));
    } else if removal == Removal::Reset {
        if let Some(Departed::Reset {
            row_warning: Some(warning),
        }) = departed
        {
            crate::ui::warn(error_chain::sentence(error_chain::with_reason(
                format_args!(
                    "Server {} reset, but its own row removal failed",
                    selected.name
                ),
                warning,
            )));
        }
        for loss in &observed.data_loss {
            crate::ui::stream(format_args!(
                "Reset left the data of Volume {} on {}.",
                volume_label(&confirmed.labels, loss),
                selected.name
            ));
        }
        if !observed.data_loss.is_empty() {
            crate::ui::stream(
                "Wipe or reinstall the Server's disk to erase that data; sudo ployz-uninstall keeps the data.",
            );
        }
    }
    let stopped = if removal == Removal::Reset && reset_doubt.is_none() {
        Stopped::Yes
    } else {
        Stopped::Unproven
    };
    for warning in short.warnings(stopped) {
        crate::ui::warn(warning);
    }
    if !short.no_copies.is_empty() {
        crate::ui::hint(&Hint::Next("ployz deploy".to_owned()));
    }

    // The removal is committed: print it before local cleanup can fail.
    crate::ui::emit(&json!({
        "server": super::server_json(selected),
        "reset_warning": reset_failure,
        "departed": departed,
        "data_loss": observed.data_loss,
        "under_replicated": short.under_replicated,
        "no_copies": short.no_copies,
        "cloud_released": cloud_released,
        "cloud_kept": cloud_kept,
        "next": (*cloud_released == Some(true)).then_some("ployz server add"),
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
    if reset_doubt.is_some() {
        return Err(Error::partial());
    }
    Ok(())
}

/// Cloud manages this Server, so Cloud removes it through its durable removal and drops
/// its own hold: its row for it, and the Cluster with its last Server. That needs a
/// Cloud credential, and for the last Server the reset; without either nothing happens.
async fn cloud_removal(
    matches: &ArgMatches,
    selected: &Machine,
    hold: CloudHold,
    removal: Removal,
) -> Result<Credential, Error> {
    if hold == CloudHold::Last && removal == Removal::Forget {
        return Err(Error::conflict(format!(
            "Server {} is the last Server and Cloud manages it. Stop its Ployz daemon and socket (systemctl stop ployz.socket ployz) before forgetting every Server, or remove it to reset it. No changes made.",
            selected.name
        ))
        .hint(Hint::Next("ployz server forget".into())));
    }
    let store = CredentialStore::beside(&crate::handlers::config_path(matches)?);
    match cloud_account::from_env(&store).await {
        Ok(credential) => Ok(credential),
        Err(LoginError::SignedOut) => Err(Error::coded(
            RpcErrorCode::Conflict,
            format!(
                "Cloud manages Server {}, so Cloud removes it and drops its hold on it. \
                 Sign in to Cloud first, or remove it from the dashboard. No changes made.",
                selected.name
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

/// Whether the removal stopped the removed Server's copies: a forgotten Server, or one
/// whose reset failed, may still run them.
#[derive(Clone, Copy)]
enum Stopped {
    Yes,
    Unproven,
}

impl LeftShort {
    fn warnings(&self, stopped: Stopped) -> Vec<String> {
        let mut warnings = Vec::new();
        if !self.no_copies.is_empty() {
            let services = super::super::joined(&self.no_copies);
            warnings.push(match stopped {
                Stopped::Yes => format!(
                    "Replicated Services now run no copies: {services}. Their replicas were not moved."
                ),
                Stopped::Unproven => format!(
                    "No copies of Replicated Services were seen on the remaining Servers: {services}. Their replicas were not moved."
                ),
            });
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

pub(super) fn select_machine(
    machines: &[ployz_core::MachineObservation],
    selector: &str,
) -> Result<Machine, Error> {
    let selector = MachineTarget::parse(selector)?;
    Ok(crate::cluster::visible_machine(&selector, machines)?
        .machine
        .clone())
}

fn machine_removal_refusal(error: RpcError, name: &str) -> Error {
    if error.code == RpcErrorCode::Unavailable {
        Error::from(error)
            .context(format!(
                "Server {name} could not be reset; forget it to take it out of the Cluster without a reset."
            ))
            .hint(Hint::Next(format!("ployz server forget {name} --confirm dead")))
    } else {
        error.into()
    }
}

/// A reset removal's refusal. A Server that reset before the Cluster could forget it needs
/// only `server forget` to finish.
fn reset_removal_refusal(error: RpcError, name: &str) -> Error {
    if LeftNotForgotten::from_rpc_error(&error).is_some() {
        Error::from(error)
            .context(format!("Server {name} reset but is still listed."))
            .hint(Hint::Next(format!("ployz server forget {name}")))
    } else {
        crate::handlers::data_loss::refusal_from_rpc(error)
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerKind, ContainerObservation, ContainerRuntimeObservation, HealthObservation,
        LiveServices, MachineId, MachineSuccess, PartialResult, QualifiedService, RpcError,
        RpcErrorCode, ServiceId, ServiceMode, ServiceName, derive_live_services,
    };
    use serde_json::json;

    use super::{
        Stopped, VolumeOwner, left_short, machine_removal_refusal, movable_services_on,
        qualified_labels, replicated_services_on, reset_removal_refusal, running_services,
        services_on,
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
    fn unreachable_removal_names_forget() {
        let error = RpcError::unavailable("Server fra-1 did not respond");
        let refusal = machine_removal_refusal(error, "fra-1");
        assert_eq!(
            refusal.to_string(),
            "Server fra-1 could not be reset; forget it to take it out of the Cluster without a reset."
        );
        assert_eq!(
            refusal.hints(),
            [Hint::Next(
                "ployz server forget fra-1 --confirm dead".into()
            )]
        );
        assert_eq!(refusal.causes(), ["Server fra-1 did not respond"]);
        assert_eq!(refusal.report().code, RpcErrorCode::Unavailable);
    }

    #[test]
    fn a_reset_server_still_listed_names_forget() {
        let error = RpcError::unavailable("the entry could not reach the Cluster store")
            .with_details(json!({ "left": true, "reset_warning": null }));
        let refusal = reset_removal_refusal(error, "fra-1");
        assert_eq!(
            refusal.to_string(),
            "Server fra-1 reset but is still listed."
        );
        assert_eq!(
            refusal.hints(),
            [Hint::Next("ployz server forget fra-1".into())]
        );
        assert_eq!(refusal.report().code, RpcErrorCode::Unavailable);
    }

    #[test]
    fn other_data_loss_errors_keep_their_message() {
        let error = RpcError::not_found("Server \"gone\" was not found");
        assert_eq!(
            machine_removal_refusal(error, "gone").to_string(),
            "Server \"gone\" was not found"
        );
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
    fn a_reset_says_services_with_no_copies_run_none_apart_from_those_left_short() {
        let short = left_short(&machine_id('a'), &api_only_on_a_web_on_both(), false);
        assert_eq!(
            short.warnings(Stopped::Yes),
            [
                "Replicated Services now run no copies: app/api. Their replicas were not moved.",
                "Replicated Services may now be under-replicated: app/web. Their replicas were not moved.",
            ]
        );
    }

    #[test]
    fn a_removal_that_left_copies_running_says_only_what_the_remaining_servers_showed() {
        let short = left_short(&machine_id('a'), &api_only_on_a_web_on_both(), false);
        assert_eq!(
            short.warnings(Stopped::Unproven),
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
        })
        .unwrap()
    }

    fn machine_id(value: char) -> MachineId {
        MachineId::parse(value.to_string().repeat(32)).unwrap()
    }
}
