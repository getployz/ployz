use crate::cluster::{Call, Deadline};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};

use clap::{Arg, ArgAction, ArgMatches, Command, ValueHint};

use crate::cli::{
    base, log_flags, machine_policy_flags, many, positional, switch, value, volume_acceptance,
};
use crate::env;
use ployz_core::{
    AdvertisedEndpoint, BuildConcurrencyUpdate, EnvironmentValues, Hint, IngressDoors,
    InspectRequest, ListContainersRequest, Machine, MachineId, MachineName, MachineObservation,
    MachineTarget, MachineUpdate, PublicIpUpdate, RpcError, RpcErrorCode, SelectedEndpoint,
    UpdateMachineRequest, op,
};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    cluster::RoleWaitError,
    connect::{Client, SystemConnector, TARGET_RPC_TIMEOUT},
    context::Config,
    ui::progress::StartFailure,
};

use super::{Error, leaf_matches, route, string_values, with_client};

mod add;
pub(super) mod checkin;
pub(super) mod clean;
pub(super) mod drain;
mod endpoint;
pub(super) mod enroll;
pub(super) mod forget;
mod helpers;
pub(crate) mod init;
pub(super) mod reject;
mod remove;
mod show;
pub(super) mod token;
mod upgrade;
pub(super) mod upgrade_policy;
mod waiting;

use add::add;
pub(super) use enroll::{
    enroll_token, founding_cluster_name, joined_open, joined_unnamed, requested_storage,
};
pub(super) use helpers::{
    Joining, STARTING, check_listed, confirm, confirmation_missing, connect_direct, enroll,
    enroll_for_recovery, enroll_member, inspect_token, leave, machine_name, pinned, requested_name,
    token_request,
};
use init::init;
pub(super) use remove::remove;
pub(super) use show::{full_disks, list, show};
pub(super) use upgrade::upgrade;

pub(super) fn clear_build_cache(matches: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(matches);
    if leaf.get_one::<String>("connect").is_some() || leaf.get_one::<String>("context").is_some() {
        return Err(Error::usage(
            "cache clearing runs on this execution host; run it there as the builder user without --connect or --context",
        ));
    }
    ployz_build::clear_cache(&ployz_build::HostPolicy::default()).map_err(|error| {
        Error::caused(
            ployz_core::RpcErrorCode::Internal,
            "Could not clear this host's build cache.",
            error,
        )
    })?;
    crate::ui::finish(&json!({ "build_cache": { "cleared": true } }), || {
        crate::ui::stream(format_args!("Cleared this host user's Ployz build cache."));
    })
}

/// Whether this CLI holds a Device key, so its Management connections dial as that Device.
pub(super) fn holds_device_key(matches: &ArgMatches) -> Result<bool, Error> {
    let key = super::device::key::path(&super::config_path(matches)?);
    Ok(super::device::key::load(&key)?.is_some())
}

/// The Server `selector` names, for a Cloud route, among the Servers Cloud's dial sees.
/// An id resolves there too, since only the Engine knows the name prose says.
pub(super) fn cloud_machine(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
    selector: &str,
) -> Result<ployz_core::Machine, Error> {
    runtime.block_on(async {
        let mut client = route::connect(matches, None).await?;
        let machines = client.machines().await?;
        remove::select_machine(&machines, selector)
    })
}

/// The Servers Cloud's dial sees, as [`cloud_machine`] picks among them.
pub(super) fn servers(matches: &ArgMatches) -> Result<Vec<ployz_core::MachineObservation>, Error> {
    super::runtime()?.block_on(async {
        let mut client = route::connect(matches, None).await?;
        Ok(client.machines().await?)
    })
}

/// `rerun` with `--approval id`, the command that goes ahead once a human approved.
pub(super) fn approved_rerun(matches: &ArgMatches, args: &[&str], id: &str) -> String {
    let mut args = args.to_vec();
    args.extend(["--approval", id]);
    super::rerun(matches, &args)
}

pub(super) struct ConnectionOptions {
    config_path: PathBuf,
    context: Option<String>,
}

impl ConnectionOptions {
    pub(super) fn from_matches(matches: &ArgMatches) -> Result<Self, Error> {
        Ok(Self {
            config_path: super::config_path(matches)?,
            context: super::leaf_matches(matches)
                .get_one::<String>("context")
                .cloned(),
        })
    }

    pub(super) fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub(super) fn active_config(&self) -> Result<(Config, String), Error> {
        let config = self.load_config()?;
        let Some(name) = config
            .context_name(self.context.as_deref())
            .map(str::to_owned)
        else {
            return Err(Error::usage(format!(
                "current context is not set in Ployz config {}",
                config.path().display()
            )));
        };
        if !config.contexts.contains_key(&name) {
            return Err(Error::not_found(format!("context {name:?} not found")));
        }
        Ok((config, name))
    }

    pub(super) fn load_config(&self) -> Result<Config, Error> {
        Ok(Config::load(&self.config_path)?)
    }

    pub(super) fn load_or_empty_config(&self) -> Result<Config, Error> {
        Ok(Config::load_or_empty(&self.config_path)?)
    }
}

pub(super) fn target<'a>(matches: &'a ArgMatches, name: &str) -> Result<&'a str, Error> {
    matches
        .get_one::<String>(name)
        .map(String::as_str)
        .ok_or_else(|| Error::usage(format!("{name} is required")))
}

/// The command that takes out a Server that never joined. By ID, since a rerun of its add
/// can list a second Server under the same name.
pub(super) fn forget_unarrived(id: MachineId) -> String {
    shell_words::join([
        "ployz",
        "server",
        "forget",
        id.as_str(),
        "--confirm",
        "dead",
    ])
}

/// A Machine as every `server` command prints it: its WireGuard public key in base64,
/// as `wg` prints it, and without its Sealing state.
pub(super) fn machine_json(machine: &Machine) -> Value {
    let mut value = serde_json::to_value(machine).expect("a Machine serializes");
    if let Value::Object(fields) = &mut value {
        fields.insert("public_key".into(), json!(machine.public_key.to_string()));
        if let Some(Value::Object(runtime)) = fields.get_mut("runtime") {
            runtime.remove("sealing");
        }
    }
    value
}

/// A Server as `server add`, `init`, `set` and `rm` print it: the `server ls` and
/// `server show` shape, its Machine under `machine`.
pub(super) fn server_json(machine: &Machine) -> Value {
    json!({ "machine": machine_json(machine) })
}

pub(super) fn set(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selector = target(matches, "server")?;
    let update = parse_update(matches)?;
    let rerun = ingress_rerun(matches, selector);
    let selector = MachineTarget::parse(selector)?;
    let seed = update
        .advertised_endpoints
        .as_ref()
        .and_then(|endpoints| endpoints.first())
        .map(|endpoint| SelectedEndpoint(endpoint.0));
    let recovery = matches.clone();
    // `--yes` reads nothing: on the `--wg-endpoint` path the Cluster may be too degraded to list.
    let ask = update.accepts_ingress == Some(false) && !matches.get_flag("yes");
    let Some(address) = seed else {
        return with_client(root, |client| {
            Box::pin(async move {
                if ask {
                    let machines = client.machines().await?;
                    let doors = open_doors(client, &machines).await?;
                    confirm_ingress_off(&machines, &doors, &selector)?;
                }
                apply_update(client, selector, update, None, rerun, recovery).await
            })
        });
    };
    super::runtime()?.block_on(async {
        let context = matches.get_one::<String>("context").map(String::as_str);
        // Ask before seeding: a refusal leaves every Server unchanged.
        if ask {
            let mut client = route::connect(matches, context).await?;
            let machines = client.machines().await?;
            let doors = open_doors(&client, &machines).await?;
            confirm_ingress_off(&machines, &doors, &selector)?;
        }
        let entries = route::entries(matches, context).await?;
        let connector =
            SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches));
        let mut seeding = endpoint::seed(&entries, Arc::new(connector), &selector, address).await;
        if let Some(sentence) = seeding.sentence() {
            crate::ui::stream(sentence);
        }
        let Some(mut client) = seeding.updater.take() else {
            return Err(unseeded(&seeding));
        };
        for missed in &seeding.failures {
            crate::ui::warn(missed.sentence());
        }
        apply_update(
            &mut client,
            selector,
            update,
            Some(seeding),
            rerun,
            recovery,
        )
        .await
    })
}

/// The `server set` that finishes turning web traffic on for the Server `selector` names.
fn ingress_rerun(matches: &ArgMatches, selector: &str) -> String {
    super::rerun(
        matches,
        &["server", "set", selector, "--accepts-ingress=true"],
    )
}

/// The Doors each Server holds open now. A Server that didn't list its containers is
/// absent, and taken as serving web traffic when it has a public IP.
async fn open_doors(
    client: &Client,
    machines: &[MachineObservation],
) -> Result<BTreeMap<MachineId, IngressDoors>, Error> {
    let live = client
        .live_services_from(machines, EnvironmentValues::Redacted)
        .await?;
    Ok(crate::ingress::machine_doors(&live.services()))
}

/// Ask before turning web traffic off on the Server `selector` names when no other online
/// Server with an open Door would still take it.
fn confirm_ingress_off(
    machines: &[MachineObservation],
    doors: &BTreeMap<MachineId, IngressDoors>,
    selector: &MachineTarget,
) -> Result<(), Error> {
    let target = crate::cluster::visible_machine(selector, machines)?;
    let serves_web = |observed: &MachineObservation| {
        observed.machine.accepts_ingress
            && doors.get(&observed.machine.id).map_or_else(
                || observed.machine.public_ip.is_some(),
                |open| !open.0.is_empty(),
            )
    };
    let last = serves_web(target)
        && !machines.iter().any(|observed| {
            observed.machine.id != target.machine.id
                && serves_web(observed)
                && observed.invites_rpc()
        });
    if !last {
        return Ok(());
    }
    let name = &target.machine.name;
    let last_one = doors.get(&target.machine.id).map_or(
        "the last Server taking web traffic".to_owned(),
        |open| {
            let names = open.0.iter().map(|door| door.as_str()).collect::<Vec<_>>();
            format!("the last Server with a door open ({})", names.join(", "))
        },
    );
    crate::ui::confirm(
        &format!("Turn off web traffic on {name}, {last_one}? Every domain goes dark"),
        || {
            Error::coded(
                RpcErrorCode::ConfirmationRequired,
                format!(
                    "{name} is {last_one}; turning it off takes every domain dark. No changes \
                     made."
                ),
            )
            .hint(Hint::Retry(super::typed_with(&["--yes"])))
        },
        "Cancelled. Web traffic stays on.",
    )
}

/// No Server took the endpoint and none was reached to update the target: the first failure.
fn unseeded(seeding: &endpoint::Seeding) -> Error {
    seeding.failures.first().map_or_else(
        || Error::not_found("no Server took the endpoint"),
        |missed| {
            let what = if missed.error.code == RpcErrorCode::Unavailable {
                "did not answer"
            } else {
                "refused the endpoint"
            };
            Error::from(missed.error.clone()).context(format!("{} {what}.", missed.entry))
        },
    )
}

/// Update the target's record. With `seeding`, a target that cannot be reached still
/// leaves the other Servers dialing it, and the command succeeds.
pub(crate) async fn apply_update(
    client: &mut Client,
    selector: MachineTarget,
    update: MachineUpdate,
    seeding: Option<endpoint::Seeding>,
    rerun: String,
    recovery: ArgMatches,
) -> Result<(), Error> {
    let planned = crate::cluster::PlannedChange::of(&update);
    let accepts_ingress = update.accepts_ingress;
    let renamed = update.name.is_some();
    let was_off = match accepts_ingress {
        Some(true) => ingress_off(client, &selector).await,
        Some(false) | None => None,
    };
    let public_ip_removed = update.public_ip == PublicIpUpdate::Remove;
    // Setting or clearing the public IP pins it, so daemon start no longer looks one up.
    let public_ip_pinned = update.public_ip != PublicIpUpdate::Keep;
    let updated = client
        .call_shared::<op::UpdateMachine>(
            UpdateMachineRequest { update },
            Call::once()
                .to(&selector)
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(|error| (error.unreplied(), RpcError::from(error)));
    let mut machine = match (updated, &seeding) {
        (Ok(updated), _) => updated.machine,
        (Err((unreplied, error)), Some(seeding))
            if error.code == RpcErrorCode::Unavailable && !seeding.dialed_by.is_empty() =>
        {
            let name = seeding.target.as_ref().map_or_else(
                || selector.as_str().to_owned(),
                |target| target.name.to_string(),
            );
            crate::ui::warn(if unreplied {
                format!("{name} didn't answer, so its record may or may not have changed.")
            } else {
                format!("{name} couldn't take the change; its record is unchanged.")
            });
            return crate::ui::emit(&json!({
                "server": null,
                "endpoint": seeding,
                "target_error": error,
            }));
        }
        (Err((true, error)), _) => {
            return Err(Error::from(error).context(format!(
                "Server {selector} didn't answer, so the change may or may not have landed. Check it with ployz server ls once it's back, or run this on the Server."
            )));
        }
        (Err((false, error)), _) if error.code == RpcErrorCode::Unavailable => {
            return Err(Error::from(error)
                .context(format!(
                    "Server {selector} answered but couldn't take the change, so nothing changed."
                ))
                .hint(Hint::Retry(super::typed_with(&[]))));
        }
        (Err((_, error)), _) => return Err(error.into()),
    };
    // A deploy right after this plans from the Entry's list, so wait until it lists the change.
    let listed = if planned.is_empty() {
        Ok(())
    } else {
        crate::cluster::wait_for_change(client, &machine, &planned, &CancellationToken::new()).await
    };
    // Turning ingress on reports a missed connection as an unfinished follow-up instead.
    if let Err(error) = &listed
        && (accepts_ingress != Some(true) || matches!(error, RoleWaitError::ChangeNotObserved))
    {
        let reason = match error {
            RoleWaitError::NotObserved(_) | RoleWaitError::ChangeNotObserved => {
                "still hasn't listed the change".to_owned()
            }
            error @ (RoleWaitError::Cancelled | RoleWaitError::Connect(_)) => {
                format!("could not confirm the change ({error})")
            }
        };
        crate::ui::warn(format!(
            "Server {} changed, but the Server this command connects through {reason}. Check with ployz server ls before deploying.",
            machine.name
        ));
    }
    crate::ui::stream(format_args!("Updated Server {}.", machine.name));
    let next =
        public_ip_removed.then(|| format!("ployz server set {} --public-ip IP", machine.name));
    if let Some(next) = &next {
        crate::ui::stream(format_args!(
            "Server {} advertises no public IP now, and Ployz won't look one up.",
            machine.name
        ));
        crate::ui::hint(&Hint::Next(next.clone()));
    }
    // The update is committed; starting the Ingress Proxy for a new ingress role is a
    // follow-up. Turning the role off leaves the proxy serving: Hosted DNS stops
    // advertising the Server at its next sync, and `server rm` stops the proxy.
    if accepts_ingress != Some(true) {
        return crate::ui::emit(&json!({
            "server": server_json(&machine),
            "endpoint": seeding,
            "public_ip_pinned": public_ip_pinned,
            "next": next,
        }));
    }
    let unobserved = |error| match error {
        RoleWaitError::Connect(error) => Error::from(error),
        error @ (RoleWaitError::NotObserved(_)
        | RoleWaitError::ChangeNotObserved
        | RoleWaitError::Cancelled) => Error::from(error),
    };
    let followed = async {
        match listed {
            Ok(()) => {}
            // Only the ingress role gates the proxy start; another unlisted field only warned.
            // One more list, not another wait, so the command keeps the one wait's budget.
            Err(RoleWaitError::ChangeNotObserved) => {
                let ingress = crate::cluster::RoleSetting::IngressOn;
                if !crate::cluster::lists_role(client, &machine.id, ingress)
                    .await
                    .map_err(unobserved)?
                {
                    return Err(unobserved(RoleWaitError::NotObserved(ingress)));
                }
            }
            Err(error) => return Err(unobserved(error)),
        }
        Ok(crate::ingress::follow_roles(client).await?)
    }
    .await;
    let (ingress, follow_up) = match followed {
        Ok(ingress) => (Some(ingress), Ok(())),
        Err(error) => {
            // A rename in this same command leaves the typed selector naming nothing (#3449).
            let (selector, rerun) = if renamed {
                let id = machine.id.to_string();
                let rerun = ingress_rerun(&recovery, &id);
                (id, rerun)
            } else {
                (selector.as_str().to_owned(), rerun)
            };
            let error = match own_start_failure(&error, machine.id, was_off) {
                Some(failure) if nothing_serves(client, machine.id).await => {
                    turn_back_off(
                        client,
                        &mut machine,
                        failure,
                        error,
                        rerun,
                        &recovery,
                        &selector,
                    )
                    .await
                }
                _ => ingress_incomplete(
                    "Server updated",
                    set_hints(error, &recovery, machine.id, &selector, &rerun),
                    rerun,
                ),
            };
            (None, Err(error))
        }
    };
    crate::ui::emit_committed(
        json!({
            "server": server_json(&machine),
            "endpoint": seeding,
            "public_ip_pinned": public_ip_pinned,
            "next": next,
            "ingress": ingress,
        }),
        follow_up,
    )
}

/// The Server `selector` names, when its own record holds no ingress role. Every role change
/// lands on that record first and reaches its replicated row later, so a row, even the one
/// the Server itself answers `ListMachines` with, can lag a role Ployz Cloud just turned on.
/// A Server not read counts as holding the role, so a failed start never turns off a role
/// this command didn't turn on.
async fn ingress_off(client: &Client, selector: &MachineTarget) -> Option<MachineId> {
    let details = client
        .call_shared::<op::Inspect>(
            InspectRequest::default(),
            Call::once()
                .to(selector)
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .ok()?;
    details
        .machine
        .filter(|record| record.id == details.id && !record.accepts_ingress)
        .map(|record| record.id)
}

/// How the new Ingress Proxy failed to start on `machine`, when `machine`'s own record held no
/// ingress role before this command and the follow-up ended on its own there: not
/// interrupted, and not on another Server.
fn own_start_failure(
    error: &Error,
    machine: MachineId,
    was_off: Option<MachineId>,
) -> Option<StartFailure> {
    error
        .diagnostic()
        .filter(|diagnostic| {
            was_off == Some(machine)
                && !error.is_interrupted()
                && diagnostic.failed_machine == Some(machine)
        })
        .and_then(|diagnostic| diagnostic.start_failure)
}

/// Whether `machine` holds no Ingress Proxy Container, neither a Caddy nor a holder, in any
/// state, in its own listing. A created or exited one may still start, under Docker's
/// restart policy or the daemon's retry of a stranded proxy, and one another operator
/// started serves. An empty listing doesn't say whose it is, so `machine` is asked who it
/// is after it.
async fn nothing_serves(client: &Client, machine: MachineId) -> bool {
    let target = MachineTarget::from(&machine);
    let Ok(listed) = client
        .call_shared::<op::ListContainers>(
            ListContainersRequest::default(),
            Call::once()
                .to(&target)
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
    else {
        return false;
    };
    if listed
        .containers
        .iter()
        .any(|container| container.machine_id != machine || container.identity().is_ingress_door())
    {
        return false;
    }
    client
        .call_shared::<op::Inspect>(
            InspectRequest::default(),
            Call::once()
                .to(&target)
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .is_ok_and(|details| details.id == machine)
}

/// Turn web traffic back off on `machine` after its new Ingress Proxy was seen failing to
/// start there with no Ingress Proxy left in its place, as the dashboard switch does, so the Server
/// isn't advertised with nothing listening. `UpdateMachine` has no compare-and-set: this
/// one-field patch overwrites a role another writer set since, and leaves every other
/// field as it is.
async fn turn_back_off(
    client: &mut Client,
    machine: &mut Machine,
    failure: StartFailure,
    failed: Error,
    rerun: String,
    recovery: &ArgMatches,
    selector: &str,
) -> Error {
    let off = MachineUpdate {
        accepts_ingress: Some(false),
        ..MachineUpdate::default()
    };
    let name = machine.name.clone();
    match client
        .call_shared::<op::UpdateMachine>(
            UpdateMachineRequest { update: off },
            Call::once()
                .to(&MachineTarget::from(&machine.id))
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
    {
        Ok(updated) => {
            *machine = updated.machine;
            let headline = match failure {
                StartFailure::PortTaken => format!(
                    "Web traffic stays off on {name}: port 80 or 443 is in use. Free it, then try again."
                ),
                StartFailure::Unhealthy => super::ingress_headline(
                    &format!("Web traffic stays off on {name}; its Ingress Proxy didn't start"),
                    &failed,
                ),
            };
            set_hints(failed, recovery, machine.id, selector, &rerun)
                .context(headline)
                .hint(Hint::Next(rerun))
        }
        Err(error) => {
            let headline = super::ingress_headline(
                &format!(
                    "The Ingress Proxy didn't start on {name}, and turning web traffic back off \
                     failed, so it may still be on"
                ),
                &failed,
            );
            let off = super::rerun(
                recovery,
                &["server", "set", selector, "--accepts-ingress=false"],
            );
            let id = machine.id;
            let server = |machine: &MachineId| server_selector(*machine, id, selector);
            super::ingress_log_hints(failed, |args| super::rerun(recovery, args), server)
                .context(headline)
                .with_detail("rollback_error", json!(RpcError::from(error)))
                .hint(Hint::Next(off))
        }
    }
}

/// The Ingress Proxy hints for `server set` on `machine`, which its reader selected as
/// `selector` (#3341). The retry is left out when it would repeat `next`.
fn set_hints(
    error: Error,
    recovery: &ArgMatches,
    machine: MachineId,
    selector: &str,
    next: &str,
) -> Error {
    let command = |args: &[&str]| super::rerun(recovery, args);
    let server = |failed: &MachineId| server_selector(*failed, machine, selector);
    let retry = error
        .diagnostic()
        .and_then(|diagnostic| diagnostic.failed_machine)
        .map(|failed| command(&["server", "set", &server(&failed), "--accepts-ingress=true"]))
        .filter(|retry| retry != next)
        .map(Hint::Retry);
    super::ingress_log_hints(error, command, server).hint(retry)
}

/// How a hint selects Server `id`: as the reader selected `machine`, which already resolved
/// to one Server, or else by Machine ID, since another Server's name may be ambiguous.
fn server_selector(id: MachineId, machine: MachineId, selector: &str) -> String {
    if id == machine {
        selector.to_owned()
    } else {
        id.to_string()
    }
}

/// A committed Server change whose Ingress Proxy follow-up failed: name it and the exact rerun.
pub(crate) fn ingress_incomplete(committed: &str, error: Error, next: String) -> Error {
    let headline = super::ingress_headline(
        &format!("{committed}; the Ingress Proxy did not follow"),
        &error,
    );
    error.context(headline).hint(Hint::Next(next))
}

fn parse_update(matches: &ArgMatches) -> Result<MachineUpdate, Error> {
    let name = matches
        .get_one::<String>("name")
        .map(MachineName::parse)
        .transpose()?;
    let public_ip = match matches.get_one::<String>("public-ip").map(String::as_str) {
        None => PublicIpUpdate::Keep,
        Some("") | Some("none") => PublicIpUpdate::Remove,
        Some(value) => PublicIpUpdate::Set(helpers::public_ip(value)?),
    };
    let build_concurrency = match matches
        .get_one::<String>("build-concurrency")
        .map(String::as_str)
    {
        None => BuildConcurrencyUpdate::Keep,
        Some("auto") => BuildConcurrencyUpdate::Automatic,
        Some(value) => BuildConcurrencyUpdate::Set(value.parse()?),
    };
    let advertised_endpoints = if matches.get_many::<String>("wg-endpoint").is_some() {
        Some(parse_endpoints(&string_values(matches, "wg-endpoint"))?)
    } else {
        None
    };
    let update = MachineUpdate {
        name,
        public_ip,
        advertised_endpoints,
        build_concurrency,
        ..parse_policy(matches)?
    };
    if update.is_empty() {
        return Err(Error::usage("at least one setting flag is required"));
    }
    if update
        .advertised_endpoints
        .as_ref()
        .is_some_and(Vec::is_empty)
    {
        return Err(Error::usage("at least one WireGuard endpoint is required"));
    }
    Ok(update)
}

pub(super) fn parse_policy(matches: &ArgMatches) -> Result<MachineUpdate, Error> {
    let mut label_changes = parse_label_add(matches)?
        .into_iter()
        .map(|(key, value)| (key, Some(value)))
        .collect::<std::collections::BTreeMap<_, _>>();
    for key in string_values(matches, "label-rm") {
        let key = ployz_core::MachineLabelKey::parse(key)?;
        match label_changes.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(None);
            }
            std::collections::btree_map::Entry::Occupied(entry) if entry.get().is_some() => {
                return Err(Error::usage(format!(
                    "Server Label {} cannot be added and removed in the same patch",
                    entry.key()
                )));
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }
    }
    Ok(MachineUpdate {
        label_changes,
        accepts_builds: matches.get_one::<bool>("accepts-builds").copied(),
        accepts_services: matches.get_one::<bool>("accepts-services").copied(),
        accepts_ingress: matches.get_one::<bool>("accepts-ingress").copied(),
        ..Default::default()
    })
}

fn parse_label_add(
    matches: &ArgMatches,
) -> Result<
    std::collections::BTreeMap<ployz_core::MachineLabelKey, ployz_core::MachineLabelValue>,
    Error,
> {
    let mut labels = std::collections::BTreeMap::new();
    for label in string_values(matches, "label-add") {
        let (key, value) = label
            .split_once('=')
            .ok_or_else(|| Error::usage(format!("invalid label {label:?}: expected KEY=VALUE")))?;
        if labels.insert(key.parse()?, value.parse()?).is_some() {
            return Err(Error::usage(format!("duplicate label key {key:?}")));
        }
    }
    Ok(labels)
}

pub(super) fn enrollment_policy(
    matches: &ArgMatches,
) -> Result<ployz_core::InitialMachinePolicy, Error> {
    Ok(ployz_core::InitialMachinePolicy {
        labels: parse_label_add(matches)?,
        accepts_builds: matches
            .get_one::<bool>("accepts-builds")
            .copied()
            .unwrap_or(true),
        accepts_services: matches
            .get_one::<bool>("accepts-services")
            .copied()
            .unwrap_or(true),
        accepts_ingress: matches
            .get_one::<bool>("accepts-ingress")
            .copied()
            .unwrap_or(true),
    })
}

pub(super) fn parse_endpoints(values: &[String]) -> Result<Vec<AdvertisedEndpoint>, Error> {
    values
        .iter()
        .map(|value| {
            value
                .parse::<SocketAddr>()
                .or_else(|_| {
                    value
                        .parse::<IpAddr>()
                        .map(|address| SocketAddr::new(address, ployz_core::WIREGUARD_PORT))
                })
                .map(AdvertisedEndpoint)
                .map_err(|_| Error::usage(format!("invalid WireGuard endpoint {value:?}")))
        })
        .collect()
}

pub(super) fn group(command: Command) -> Command {
    base(command, "Manage Servers")
}

pub(super) fn clear_build_cache_command(command: Command) -> Command {
    base(command, "Clear this execution host user's Ployz build cache")
            .long_about("Clear this execution host user's Ployz build cache. Run on the build host as the user running its Builds (including the daemon). Refuses active or quarantined builder ownership; preserves completed images and unrelated Docker data. No daemon is required.\n\nHost configuration: ~/.ployz/build.yaml. Optional cpu_cores and memory_bytes limit BuildKit and Railpack preparation, independently of Service runtime limits. Both are disabled when omitted. Optional cache_bytes and min_free_bytes are retention/GC targets, not hard peak disk quotas. Unconfigured GC uses pinned BuildKit defaults.")
}

pub(super) fn drain_command(command: Command) -> Command {
    base(command, "Stop placing Services on a Server and move its Services' Containers off it")
                .long_about("Turn off the Server's services role, then move each replicated Service's Containers off it one at a time: a new Container starts on another Server from the same image and serves before the old one stops. No hooks run and no Deployment is recorded. Rerun to act on what remains. Turning the services role back on does not move anything back. Signed in to Cloud without --context or --connect, Cloud runs the drain, asking a human first when the Organization wants that.")
                .arg(positional("server", true).help("Server name or ID"))
                .arg(crate::cli::approval())
}

pub(super) fn show_command(command: Command) -> Command {
    base(
        command,
        "Show a Server: telemetry, round-trip times and its latest upgrade attempt, or a server waiting to join",
    )
    .arg(positional("server", true).help("Server name or ID, or a server waiting to join"))
}

pub(super) fn machine_logs_command(command: Command) -> Command {
    log_flags(base(command, "Show Server daemon logs")).arg(
        Arg::new("service")
            .num_args(0..)
            .action(ArgAction::Append)
            .help("ployz, docker or corrosion [default: ployz]"),
    )
}

pub(super) fn list_command(command: Command) -> Command {
    base(command, "List Servers")
        .long_about(
            "List the Cluster's Servers. With --waiting, list the servers waiting to join \
             your Organization in Ployz Cloud instead: name, provider, IP, status and age.",
        )
        .arg(switch("waiting", None).help("List servers waiting to join instead"))
}

pub(super) fn remove_command(command: Command) -> Command {
    base(command, "Remove a Server")
                .long_about("Remove a Server from the Cluster and reset it; for a Server that is gone or can't be reset, use ployz server forget. Type the Server's name with --confirm, or in a terminal when it asks; elsewhere it fails with confirmation_required, naming what goes and the exact command to retry. When Cloud manages the Server, Cloud removes it even with --context or --connect, asking a human first when the Organization wants that.")
                .arg(value("confirm", None).value_name("SERVER").help("The Server's name, typed to confirm its removal"))
                .arg(positional("server", true).help("Server name or ID"))
                .arg(crate::cli::approval())
                .arg(
                    volume_acceptance().help("Accept loss of Cluster access: repeat once per exact volume name; reset does not erase volume data on the host"),
                )
}

pub(super) fn set_command(command: Command) -> Command {
    machine_policy_flags(base(command, "Change a Server's name, labels, roles, public IP or build concurrency")
                .long_about("Change a Server's name, labels, roles, public IP or build concurrency. --accepts-ingress=true also starts the Ingress Proxy on that Server; rerun the same command to finish a start that failed. --accepts-ingress=false stops advertising the Server in Hosted DNS from the next sync, but its Ingress Proxy keeps serving until `ployz server rm`. When no other online Server takes web traffic, through public ports or a tunnel, --accepts-ingress=false asks first; pass --yes to skip the question, which non-interactive runs need."))
                .arg(many("label-rm", None).value_name("KEY").help("Remove the label KEY; repeat for more"))
                .arg(value("name", None).help("Its new name"))
                .arg(value("public-ip", None).value_name("IP|none").help("The public IP it advertises; none advertises none"))
                .arg(
                    value("build-concurrency", None)
                        .value_name("N|auto")
                        .help("Builds this Server runs at once; auto follows its roles and RAM"),
                )
                .arg(many("wg-endpoint", None).value_name("IP[:PORT]").help(
                    "Where peers reach this Server over WireGuard; every other Server the command reaches dials the first one now, even if this Server is unreachable",
                ))
                .arg(crate::cli::yes("Turn off web traffic on the last Server taking it without asking"))
                .arg(positional("server", true).help("Server name or ID"))
}

pub(super) fn upgrade_command(command: Command) -> Command {
    base(command, "Upgrade the daemon on explicitly selected Servers, one at a time")
                .long_about("Upgrade the daemon on explicitly selected Servers, one at a time, stopping at the first failure. When every daemon upgraded and any selected Server holds the ingress role, every Server with that role then moves its Ingress Proxy to the Caddy that release ships. Signed in to Cloud without --context or --connect, Cloud runs each upgrade along the Organization's Release Channel; an exact version needs --context.")
                .arg_required_else_help(true)
                .arg(
                    positional("version", true)
                        .value_name("VERSION")
                        .value_parser(clap::value_parser!(ployz_core::MachineRelease))
                        .help("stable, beta, or an exact version such as 0.3.0"),
                )
                .arg(
                    positional("server", true)
                        .num_args(1..)
                        .action(ArgAction::Append)
                        .help("Server name or ID, in upgrade order"),
                )
                .arg(switch("allow-downgrade", None).help(
                    "Move a Server to an older version than it runs without asking",
                ))
}

pub(super) fn provisioning_flags(command: Command) -> Command {
    machine_policy_flags(command)
        .arg(
            value("name", None)
                .help("Name the Server; it takes its hostname when omitted"),
        )
        .arg(switch("no-install", None).help("Skip installing Ployz; the Server runs it already"))
        .arg(
            value("storage", None)
                .value_parser(clap::value_parser!(ployz_core::StoragePreparation))
                .help(
                    "zfs sets up Managed volumes [Cloud default]; zfs:<device> uses an empty disk; none is Docker only",
                ),
        )
        .arg(
            value("public-ip", None)
                .default_value("auto")
                .help("The public IP it advertises; auto finds it, none advertises none"),
        )
        .arg(
            value("ssh-key", Some('i'))
                .default_value("~/.ssh/id_ed25519")
                .value_hint(ValueHint::FilePath)
                .help("SSH private key for DESTINATION"),
        )
        .arg(
            value("version", None)
                .env(env::DAEMON_VERSION)
                .default_value(env!("CARGO_PKG_VERSION"))
                .value_parser(clap::value_parser!(ployz_core::MachineRelease))
                .help("Daemon release to install: stable, beta, or an exact version"),
        )
        .arg(
            many("wg-endpoint", None)
                .value_name("IP[:PORT]")
                .help("Where peers reach this Server over WireGuard"),
        )
        .arg(
            value("wg-mtu", None)
                .value_parser(clap::value_parser!(u32).range(1..))
                .help("MTU of this Server's WireGuard interface"),
        )
        .arg(crate::cli::yes(
            "Don't ask: reset an initialized Server, and take --storage none unless given",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn setting_services_waits_until_the_entry_lists_the_new_role() {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };

        use ployz_core::{
            MachineList, MachineUpdated, MembershipObservation, OpaquePayload, RpcRequestBody,
            RpcResponse,
        };
        use tonic::{Request, Response};

        for accepts_services in [true, false] {
            let mut stale = web("machine-3", false, None, MembershipObservation::Up);
            stale.machine.accepts_services = !accepts_services;
            let mut updated = stale.clone();
            updated.machine.accepts_services = accepts_services;
            let id = updated.machine.id;
            let visible = Arc::new(AtomicBool::new(false));
            let (answered, mut changed) = tokio::sync::oneshot::channel();
            let answered = Arc::new(Mutex::new(Some(answered)));
            let (mut client, server) = crate::connect::test_support::rpc_client({
                let visible = Arc::clone(&visible);
                move |request: Request<OpaquePayload>| {
                    let visible = Arc::clone(&visible);
                    let answered = Arc::clone(&answered);
                    let stale = stale.clone();
                    let updated = updated.clone();
                    async move {
                        #[expect(
                            clippy::wildcard_enum_match_arm,
                            reason = "the fixture answers only UpdateMachine and ListMachines"
                        )]
                        let response = match request.into_inner().decode_request().unwrap().body {
                            RpcRequestBody::UpdateMachine(_) => {
                                if let Some(answered) = answered.lock().unwrap().take() {
                                    answered.send(()).unwrap();
                                }
                                RpcResponse::from(MachineUpdated {
                                    machine: updated.machine,
                                })
                            }
                            RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                                enrollment: None,
                                machines: vec![if visible.load(Ordering::SeqCst) {
                                    updated
                                } else {
                                    stale
                                }],
                            }),
                            other => panic!("unexpected RPC: {other:?}"),
                        };
                        Ok(Response::new(response.encode().unwrap()))
                    }
                }
            })
            .await;
            let root = crate::cli::command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "set",
                    id.as_str(),
                    if accepts_services {
                        "--accepts-services=true"
                    } else {
                        "--accepts-services=false"
                    },
                ])
                .unwrap();
            let recovery = root
                .subcommand_matches("server")
                .and_then(|server| server.subcommand_matches("set"))
                .unwrap()
                .clone();
            let setting = apply_update(
                &mut client,
                MachineTarget::from(&id),
                MachineUpdate {
                    accepts_services: Some(accepts_services),
                    ..Default::default()
                },
                None,
                String::new(),
                recovery,
            );
            tokio::pin!(setting);
            tokio::select! {
                _ = &mut changed => {}
                result = &mut setting => panic!("server set finished before the Entry observed the new services role: {result:?}"),
            }
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut setting)
                    .await
                    .is_err(),
                "server set acknowledged before the Entry listed accepts_services={accepts_services}"
            );
            visible.store(true, Ordering::SeqCst);
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut setting)
                .await
                .unwrap()
                .unwrap();
            server.abort();
        }
    }

    /// A deploy right after `server set` plans from the Entry's list, so every changed field
    /// it reads must be listed there before the command returns.
    #[tokio::test(start_paused = true)]
    async fn every_planned_field_waits_until_the_entry_lists_it() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use ployz_core::{
            MachineLabelKey, MachineLabelValue, MachineList, MachineUpdated, MembershipObservation,
            OpaquePayload, RpcRequestBody, RpcResponse,
        };
        use tonic::{Request, Response};

        type Step = fn(&mut Machine);
        let zone = MachineLabelKey::parse("zone").unwrap();
        let east = MachineLabelValue::parse("east").unwrap();
        let add_zone: Step = |machine| {
            machine.labels.insert(
                MachineLabelKey::parse("zone").unwrap(),
                MachineLabelValue::parse("east").unwrap(),
            );
        };
        let builds_off: Step = |machine| machine.accepts_builds = false;
        // Each case: what the Server held before, then the changes the Entry lists one at a time.
        let cases: [(&str, Step, Vec<Step>, MachineUpdate); 5] = [
            (
                "a new label",
                |_| {},
                vec![add_zone],
                MachineUpdate {
                    label_changes: [(zone.clone(), Some(east.clone()))].into(),
                    ..Default::default()
                },
            ),
            (
                "a removed label",
                add_zone,
                vec![|machine| machine.labels.clear()],
                MachineUpdate {
                    label_changes: [(zone.clone(), None)].into(),
                    ..Default::default()
                },
            ),
            (
                "builds off",
                |_| {},
                vec![builds_off],
                MachineUpdate {
                    accepts_builds: Some(false),
                    ..Default::default()
                },
            ),
            (
                "ingress off",
                |_| {},
                vec![|machine| machine.accepts_ingress = false],
                MachineUpdate {
                    accepts_ingress: Some(false),
                    ..Default::default()
                },
            ),
            (
                "a label and builds off, listed one after the other",
                |_| {},
                vec![add_zone, builds_off],
                MachineUpdate {
                    label_changes: [(zone, Some(east))].into(),
                    accepts_builds: Some(false),
                    ..Default::default()
                },
            ),
        ];
        for (change, before, steps, update) in cases {
            let mut first = web("machine-3", true, None, MembershipObservation::Up);
            before(&mut first.machine);
            let id = first.machine.id;
            let mut listed = vec![first];
            for step in &steps {
                let mut next = listed.last().unwrap().clone();
                step(&mut next.machine);
                listed.push(next);
            }
            let saved = listed.last().unwrap().machine.clone();
            let stage = Arc::new(AtomicUsize::new(0));
            let (mut client, server) = crate::connect::test_support::rpc_client({
                let stage = Arc::clone(&stage);
                move |request: Request<OpaquePayload>| {
                    let stage = stage.load(Ordering::SeqCst);
                    let listed = listed.get(stage).unwrap().clone();
                    let saved = saved.clone();
                    async move {
                        #[expect(
                            clippy::wildcard_enum_match_arm,
                            reason = "server set sends only the update and the list"
                        )]
                        let response = match request.into_inner().decode_request().unwrap().body {
                            RpcRequestBody::UpdateMachine(_) => {
                                RpcResponse::from(MachineUpdated { machine: saved })
                            }
                            RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                                enrollment: None,
                                machines: vec![listed],
                            }),
                            other => panic!("unexpected RPC: {other:?}"),
                        };
                        Ok(Response::new(response.encode().unwrap()))
                    }
                }
            })
            .await;
            let recovery = crate::cli::command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "set",
                    id.as_str(),
                    "--accepts-builds=false",
                ])
                .unwrap()
                .subcommand_matches("server")
                .and_then(|server| server.subcommand_matches("set"))
                .unwrap()
                .clone();
            let setting = apply_update(
                &mut client,
                MachineTarget::from(&id),
                update,
                None,
                String::new(),
                recovery,
            );
            tokio::pin!(setting);
            for listed in 0..steps.len() {
                assert!(
                    tokio::time::timeout(std::time::Duration::from_secs(5), &mut setting)
                        .await
                        .is_err(),
                    "server set returned when the Entry listed {listed} of the changes in {change}"
                );
                stage.store(listed + 1, Ordering::SeqCst);
            }
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut setting)
                .await
                .unwrap()
                .unwrap();
            server.abort();
        }
    }

    #[test]
    fn setting_services_warns_when_the_entry_never_lists_the_new_role() {
        use ployz_core::{
            MachineList, MachineUpdated, MembershipObservation, OpaquePayload, RpcRequestBody,
            RpcResponse,
        };
        use tonic::{Request, Response};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let mode = crate::ui::test_mode(true);
        let (result, captured) = crate::ui::captured(|| {
            runtime.block_on(async {
                let mut stale = web("machine-3", false, None, MembershipObservation::Up);
                stale.machine.accepts_services = false;
                let mut updated = stale.clone();
                updated.machine.accepts_services = true;
                let id = updated.machine.id;
                let (mut client, server) = crate::connect::test_support::rpc_client(
                    move |request: Request<OpaquePayload>| {
                        let stale = stale.clone();
                        let updated = updated.clone();
                        async move {
                            #[expect(
                                clippy::wildcard_enum_match_arm,
                                reason = "the fixture answers only UpdateMachine and ListMachines"
                            )]
                            let response = match request.into_inner().decode_request().unwrap().body
                            {
                                RpcRequestBody::UpdateMachine(_) => {
                                    RpcResponse::from(MachineUpdated {
                                        machine: updated.machine,
                                    })
                                }
                                RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                                    enrollment: None,
                                    machines: vec![stale],
                                }),
                                other => panic!("unexpected RPC: {other:?}"),
                            };
                            Ok(Response::new(response.encode().unwrap()))
                        }
                    },
                )
                .await;
                let root = crate::cli::command()
                    .try_get_matches_from([
                        "ployz",
                        "server",
                        "set",
                        id.as_str(),
                        "--accepts-services=true",
                        "--json",
                    ])
                    .unwrap();
                let recovery = root
                    .subcommand_matches("server")
                    .and_then(|server| server.subcommand_matches("set"))
                    .unwrap()
                    .clone();
                let started = tokio::time::Instant::now();
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(41),
                    apply_update(
                        &mut client,
                        MachineTarget::from(&id),
                        MachineUpdate {
                            accepts_services: Some(true),
                            ..Default::default()
                        },
                        None,
                        String::new(),
                        recovery,
                    ),
                )
                .await
                .expect("server set finishes within the bounded wait");
                assert!(started.elapsed() >= std::time::Duration::from_secs(30));
                server.abort();
                result
            })
        });
        drop(mode);
        result.unwrap();
        let captured = captured.unwrap();
        assert_eq!(
            captured.pointer("/server/machine/accepts_services"),
            Some(&json!(true))
        );
        assert_eq!(
            captured.pointer("/warnings/0"),
            Some(&json!(
                "Server machine-3 changed, but the Server this command connects through still hasn't listed the change. Check with ployz server ls before deploying."
            ))
        );
    }

    /// Only a start that failed on its own, on the Server whose role this command turned on,
    /// can turn the role back off.
    #[test]
    fn only_an_uninterrupted_start_failure_on_the_server_turned_on_counts() {
        let one = MachineId::random();
        let failed_on = |machine| {
            Error::coded(RpcErrorCode::Internal, "ingress: create Container failed.")
                .with_diagnostic(crate::ui::progress::Diagnostic {
                    failed_machine: Some(machine),
                    start_failure: Some(StartFailure::PortTaken),
                    ..Default::default()
                })
        };
        assert_eq!(
            own_start_failure(&failed_on(one), one, Some(one)),
            Some(StartFailure::PortTaken)
        );
        assert_eq!(own_start_failure(&failed_on(one), one, None), None);
        assert_eq!(
            own_start_failure(&failed_on(one), one, Some(MachineId::random())),
            None,
            "the record read before was another Server's"
        );
        assert_eq!(
            own_start_failure(&failed_on(one).interrupted(), one, Some(one)),
            None,
            "an interrupt may have cut the Deploy short"
        );
        assert_eq!(
            own_start_failure(&failed_on(MachineId::random()), one, Some(one)),
            None,
            "a failure on another Server"
        );
    }

    /// `server set` hints select its Server as the reader did, not by Machine ID, and don't
    /// repeat `next:` as `retry:` (#3341). Another Server stays selected by Machine ID.
    #[test]
    fn set_hints_select_the_server_as_typed() {
        use crate::ui::progress::{Diagnostic, LogTail};
        use ployz_core::QualifiedService;
        let (target, other) = (MachineId::random(), MachineId::random());
        let tail = |machine| LogTail {
            service: QualifiedService::system_ingress(),
            name: QualifiedService::system_ingress().name,
            machine,
            server: "machine-3".into(),
            log: None,
        };
        let failed_on = |machine| {
            Error::unavailable("failed to start").with_diagnostic(Diagnostic {
                failed_machine: Some(machine),
                logs: vec![tail(machine)],
                ..Default::default()
            })
        };
        let root = crate::cli::command()
            .try_get_matches_from([
                "ployz",
                "server",
                "set",
                "machine-3",
                "--accepts-ingress=true",
            ])
            .unwrap();
        let recovery = leaf_matches(&root);
        let next = super::super::rerun(
            recovery,
            &["server", "set", "machine-3", "--accepts-ingress=true"],
        );
        let hints =
            |error, next: &str| set_hints(error, recovery, target, "machine-3", next).hints();

        assert_eq!(
            hints(failed_on(target), &next),
            [Hint::Inspect(
                "ployz logs ployz-system/ingress --machine machine-3".into()
            )]
        );
        assert_eq!(
            hints(
                failed_on(target),
                "ployz server set machine-3 --accepts-builds=true"
            ),
            [
                Hint::Inspect("ployz logs ployz-system/ingress --machine machine-3".into()),
                Hint::Retry("ployz server set machine-3 '--accepts-ingress=true'".into()),
            ]
        );
        assert_eq!(
            hints(failed_on(other), &next),
            [
                Hint::Inspect(format!("ployz logs ployz-system/ingress --machine {other}")),
                Hint::Retry(format!("ployz server set {other} '--accepts-ingress=true'")),
            ]
        );
    }

    #[test]
    fn endpoints_accept_socket_or_ip_and_apply_the_default_port() {
        assert_eq!(
            parse_endpoints(&["192.0.2.1".into(), "[2001:db8::1]:6000".into()]).unwrap(),
            [
                AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap()),
                AdvertisedEndpoint("[2001:db8::1]:6000".parse().unwrap()),
            ]
        );
        assert!(parse_endpoints(&["not-an-address".into()]).is_err());
    }

    #[test]
    fn server_json_never_carries_the_sealing_state() {
        let mut machine = crate::global_fixtures::machine('a', "node-a");
        machine.runtime.sealing = Some(ployz_core::Sealing::Held {
            public_key: ployz_core::SealingPublicKey::from_bytes([7; 32]),
        });
        let runtime = |json: &Value| json.get("runtime").cloned().unwrap();
        assert!(
            runtime(&serde_json::to_value(&machine).unwrap())
                .get("sealing")
                .is_some()
        );
        let server = server_json(&machine).get("machine").cloned().unwrap();
        for json in [machine_json(&machine), server] {
            let runtime = runtime(&json);
            assert!(runtime.get("sealing").is_none(), "{json}");
            assert_eq!(
                runtime.get("hostname").and_then(Value::as_str),
                Some(machine.runtime.hostname.as_str())
            );
        }
    }

    #[test]
    fn enrolling_pins_only_the_addresses_the_operator_names() {
        let pinned = |flags: &[&str]| {
            let matches = crate::cli::command()
                .try_get_matches_from(
                    ["ployz", "server", "add", "--standalone", "root@host"]
                        .iter()
                        .chain(flags),
                )
                .unwrap();
            helpers::pinned(&helpers::token_request(leaf_matches(&matches)).unwrap())
        };
        let pins = |public_ip, advertised_endpoints| ployz_core::PinnedAddresses {
            public_ip,
            advertised_endpoints,
        };
        assert_eq!(pinned(&[]), pins(false, false));
        assert_eq!(pinned(&["--public-ip=auto"]), pins(false, false));
        assert_eq!(pinned(&["--public-ip=none"]), pins(true, false));
        assert_eq!(pinned(&["--public-ip=192.0.2.9"]), pins(true, false));
        assert_eq!(pinned(&["--wg-endpoint=192.0.2.1"]), pins(false, true));
    }

    #[test]
    fn a_public_ip_nobody_can_reach_is_refused() {
        let set = |ip: &str| {
            let matches = crate::cli::command()
                .try_get_matches_from(["ployz", "server", "set", "node-a", "--public-ip", ip])
                .unwrap();
            parse_update(leaf_matches(&matches)).map(|update| update.public_ip)
        };
        let add = |ip: &str| {
            let matches = crate::cli::command()
                .try_get_matches_from(["ployz", "server", "add", "root@node-a", "--public-ip", ip])
                .unwrap();
            helpers::token_request(leaf_matches(&matches)).map(|_| ())
        };
        for unreachable in [
            "0.0.0.0",
            "::",
            "127.0.0.1",
            "::1",
            "169.254.1.1",
            "fe80::1",
            "224.0.0.1",
            "ff02::1",
            "255.255.255.255",
            "::ffff:127.0.0.1",
        ] {
            let refused = set(unreachable).unwrap_err();
            assert_eq!(
                refused.report().code,
                RpcErrorCode::InvalidArgument,
                "{unreachable}"
            );
            assert!(refused.to_string().contains(unreachable), "{refused}");
            assert!(add(unreachable).is_err(), "{unreachable}");
        }
        for allowed in [
            "203.0.113.9",
            "2001:db8::9",
            "10.0.0.5",
            "192.168.1.2",
            "fd00::5",
        ] {
            assert_eq!(
                set(allowed).unwrap(),
                PublicIpUpdate::Set(allowed.parse().unwrap())
            );
            assert!(add(allowed).is_ok(), "{allowed}");
        }
    }

    #[test]
    fn set_cli_rejects_an_empty_patch_and_maps_none_to_public_ip_removal() {
        let command = crate::cli::command();
        let empty = command
            .clone()
            .try_get_matches_from(["ployz", "server", "set", "node-a"])
            .unwrap();
        assert!(parse_update(leaf_matches(&empty)).is_err());

        let remove = command
            .try_get_matches_from(["ployz", "server", "set", "node-a", "--public-ip=none"])
            .unwrap();
        assert_eq!(
            parse_update(leaf_matches(&remove)).unwrap().public_ip,
            PublicIpUpdate::Remove
        );

        let concurrency = |value: &str| {
            let matches = crate::cli::command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "set",
                    "node-a",
                    &format!("--build-concurrency={value}"),
                ])
                .unwrap();
            parse_update(leaf_matches(&matches)).map(|update| update.build_concurrency)
        };
        assert_eq!(
            concurrency("auto").unwrap(),
            BuildConcurrencyUpdate::Automatic
        );
        assert_eq!(
            concurrency("3").unwrap(),
            BuildConcurrencyUpdate::Set("3".parse().unwrap())
        );
        assert!(concurrency("0").is_err());
    }

    #[test]
    fn set_cli_validates_labels_and_preserves_independent_roles() {
        let parse = |flags: &[&str]| {
            let mut args = vec!["ployz", "server", "set", "node-a"];
            args.extend_from_slice(flags);
            let matches = crate::cli::command().try_get_matches_from(args).unwrap();
            parse_update(leaf_matches(&matches))
        };
        let patch = parse(&[
            "--label-add",
            "region=west",
            "--label-rm",
            "old",
            "--accepts-builds=true",
            "--accepts-services=false",
            "--accepts-ingress=true",
        ])
        .unwrap();
        assert_eq!(
            patch
                .label_changes
                .get("region")
                .and_then(Option::as_ref)
                .map(ployz_core::MachineLabelValue::as_str),
            Some("west")
        );
        assert_eq!(patch.label_changes.get("old"), Some(&None));
        assert_eq!(patch.accepts_builds, Some(true));
        assert_eq!(patch.accepts_services, Some(false));
        assert_eq!(patch.accepts_ingress, Some(true));
        for flags in [
            vec!["--label-add", "region"],
            vec!["--label-add", "=west"],
            vec!["--label-add", "rack/zone=west"],
            vec!["--label-add", "region="],
            vec!["--label-add", "region=é"],
            vec!["--label-add", "region=🦀"],
            vec!["--label-add", "region= west"],
            vec!["--label-add", "region=west "],
            vec!["--label-add", "region=west", "--label-add", "region=east"],
            vec!["--label-add", "region=west", "--label-rm", "region"],
        ] {
            assert!(parse(&flags).is_err(), "{flags:?}");
        }
    }

    #[test]
    fn singular_server_cli_target_rejects_star_and_keeps_all_as_identity() {
        let command = crate::cli::command();
        let star = command
            .clone()
            .try_get_matches_from(["ployz", "server", "set", "*", "--name", "edge"])
            .unwrap();
        assert!(MachineTarget::parse(target(leaf_matches(&star), "server").unwrap()).is_err());

        let named_all = command
            .try_get_matches_from(["ployz", "server", "set", "all", "--name", "edge"])
            .unwrap();
        assert_eq!(
            MachineTarget::parse(target(leaf_matches(&named_all), "server").unwrap())
                .unwrap()
                .as_str(),
            "all"
        );
    }

    fn web(
        name: &str,
        accepts_ingress: bool,
        public_ip: Option<&str>,
        membership: ployz_core::MembershipObservation,
    ) -> MachineObservation {
        MachineObservation::new(
            Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress,
                id: ployz_core::MachineId::random(),
                name: MachineName::parse(name).unwrap(),
                subnet: "10.210.0.0/24".parse().unwrap(),
                public_key: ployz_core::WireGuardPublicKey([1; 32]),
                public_ip: public_ip.map(|ip| ip.parse().unwrap()),
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                join_approval: None,
                build_concurrency: None,
            },
            membership,
        )
    }

    #[test]
    fn web_traffic_off_asks_only_when_no_other_online_server_takes_it() {
        use ployz_core::MembershipObservation::{Down, Suspect, Unknown, Up};
        let fra1 = MachineTarget::parse("fra-1").unwrap();
        let unlisted = BTreeMap::new();
        let off = |other: MachineObservation| {
            let machines = [web("fra-1", true, Some("203.0.113.1"), Down), other];
            confirm_ingress_off(&machines, &unlisted, &fra1).map_err(|error| error.report())
        };

        // Tests run in Plain mode, so the question is a refusal naming --yes.
        let refused = off(web("fra-2", false, Some("203.0.113.2"), Up)).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
        assert!(refused.message.contains("fra-1"), "{}", refused.message);
        assert!(
            refused
                .details
                .get("retry")
                .and_then(Value::as_str)
                .is_some_and(|retry| retry.ends_with(" --yes")),
            "{:?}",
            refused.details
        );
        for offline in [Down, Unknown] {
            let refused = off(web("fra-2", true, Some("203.0.113.2"), offline)).unwrap_err();
            assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
        }
        let refused = off(web("fra-2", true, None, Up)).unwrap_err();
        assert_eq!(
            refused.code,
            RpcErrorCode::ConfirmationRequired,
            "no public IP serves no web"
        );

        for online in [Up, Suspect] {
            off(web("fra-2", true, Some("203.0.113.2"), online)).unwrap();
        }
        let already_off = [web("fra-1", false, Some("203.0.113.1"), Up)];
        confirm_ingress_off(&already_off, &unlisted, &fra1).unwrap();
    }

    #[test]
    fn web_traffic_off_counts_open_doors_over_public_ips() {
        use ployz_core::MembershipObservation::{Down, Up};
        let fra1 = MachineTarget::parse("fra-1").unwrap();
        let other = web("fra-2", true, None, Up);
        let machines = [web("fra-1", true, Some("203.0.113.1"), Down), other.clone()];
        let tunnel = BTreeMap::from([(other.machine.id, IngressDoors::tunnel())]);
        confirm_ingress_off(&machines, &tunnel, &fra1)
            .expect("a Server with no public IP takes web traffic through its tunnel");

        let other = web("fra-2", true, Some("203.0.113.2"), Up);
        let machines = [web("fra-1", true, Some("203.0.113.1"), Down), other.clone()];
        let closed = BTreeMap::from([(other.machine.id, IngressDoors(Default::default()))]);
        let refused = confirm_ingress_off(&machines, &closed, &fra1)
            .map_err(|error| error.report())
            .unwrap_err();
        assert_eq!(
            refused.code,
            RpcErrorCode::ConfirmationRequired,
            "a public IP with no open Door serves no web"
        );

        let last = web("fra-1", true, None, Up);
        let machines = [last.clone(), web("fra-2", false, None, Up)];
        let tunnel = BTreeMap::from([(last.machine.id, IngressDoors::tunnel())]);
        let refused = confirm_ingress_off(&machines, &tunnel, &fra1)
            .map_err(|error| error.report())
            .unwrap_err();
        assert_eq!(
            refused.message,
            "fra-1 is the last Server with a door open (tunnel); turning it off takes every \
             domain dark. No changes made."
        );
    }
}
