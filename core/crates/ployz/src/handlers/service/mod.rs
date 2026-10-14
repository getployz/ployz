use std::collections::{HashMap, HashSet};

use super::catalog::{Approval::*, Runnable, cloud};
use clap::{Arg, ArgAction, ArgMatches, Command};
use futures_util::future::join_all;

use crate::cli::{base, positional, value};
use ployz_core::rpc::op;
use ployz_core::{
    ContainerAction, ContainerId, ContainerObservation, ContainerRef, ContainerRuntimeObservation,
    HealthFailure, HealthObservation, LiveServices, MachineFailure, MachineId, MachineName,
    MachineTarget, QualifiedService, ReadServingPointerRequest, RpcError, RpcErrorCode,
    ServiceObservation, ServiceSelector, select_service,
};
use serde::Serialize;

use crate::{
    cluster::HealthWaitError,
    ui::{Cell, Gaps, Table, Tone},
};
use ployz_core::EnvironmentValues;

use super::{Error, cancellation_on_ctrl_c, leaf_matches, with_client};

/// List observed Service and hook Containers.
///
/// # Errors
///
/// Returns a connection, RPC, or serialization error.
pub fn processes(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let sort = matches
        .get_one::<String>("sort")
        .cloned()
        .ok_or_else(|| Error::usage("sort order is required"))?;
    let namespace = super::operator::scope(root)?.map(|scoped| scoped.namespace);
    with_client(root, |client| {
        Box::pin(async move {
            let machines = client.machines().await?;
            let live = client
                .live_services_from(&machines, EnvironmentValues::Redacted)
                .await?;
            let names: HashMap<MachineId, &MachineName> = machines
                .iter()
                .map(|observed| (observed.machine.id, &observed.machine.name))
                .collect();
            let services = live.services();
            let services = services
                .iter()
                .filter(|service| {
                    namespace
                        .as_ref()
                        .is_none_or(|namespace| service.identity.namespace == *namespace)
                })
                .collect::<Vec<_>>();
            let retiring = retiring(client, &live, &services).await;
            let mut containers = services
                .iter()
                .flat_map(|service| service.members())
                .collect::<Vec<_>>();
            sort_processes(&mut containers, &sort);
            let observations = process_rows(&containers, &retiring);
            let gaps = Gaps::of(&live.containers)
                .named(names.iter().map(|(machine_id, name)| (*machine_id, *name)));
            let table = process_table(&containers, namespace.is_some(), &names, &retiring);
            crate::ui::finish_fanout("containers", &observations, &gaps, || {
                crate::ui::rows(&table);
            })
        })
    })
}

/// Containers the Serving Pointers of `services` list as retiring, read from
/// one Machine that answered for its Containers. A pointer that cannot be read
/// adds nothing, so its Containers show their own state.
async fn retiring(
    client: &crate::connect::Client,
    live: &LiveServices<RpcError>,
    services: &[&ServiceObservation],
) -> HashSet<ContainerId> {
    let Some(target) = live
        .containers
        .successes
        .iter()
        .map(|success| &success.machine_id)
        .min()
        .map(MachineTarget::from)
    else {
        return HashSet::new();
    };
    let reads = services.iter().map(|service| {
        let mut client = client.clone();
        let target = target.clone();
        async move {
            client
                .read::<op::ReadServingPointer>(
                    ReadServingPointerRequest {
                        service: service.identity.clone(),
                    },
                    &target,
                )
                .await
        }
    });
    join_all(reads)
        .await
        .into_iter()
        .flatten()
        .filter_map(|read| read.pointer)
        .flat_map(|pointer| pointer.retiring)
        .collect()
}

/// A running Service Container its Serving Pointer has replaced, still up
/// until every Machine stops routing to it.
fn is_draining(container: ContainerRef<'_>, retiring: &HashSet<ContainerId>) -> bool {
    let observation = container.as_observation();
    matches!(container, ContainerRef::Service(_))
        && matches!(
            observation.runtime,
            ContainerRuntimeObservation::Running { .. }
        )
        && retiring.contains(&observation.container_id)
}

/// One `ps --json` entry: the observation, flagged while it drains.
#[derive(Serialize)]
struct Process<'a> {
    #[serde(flatten)]
    container: &'a ContainerObservation,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    draining: bool,
}

fn process_rows<'a>(
    containers: &[ContainerRef<'a>],
    retiring: &HashSet<ContainerId>,
) -> Vec<Process<'a>> {
    containers
        .iter()
        .map(|container| Process {
            container: container.as_observation(),
            draining: is_draining(*container, retiring),
        })
        .collect()
}

/// Each Container next to how many it stands for: `None` for itself, `Some(n)`
/// for the `n` draining Containers of one Service on one Server, kept at the
/// position of the first.
fn collapse_draining<'a>(
    containers: &[ContainerRef<'a>],
    retiring: &HashSet<ContainerId>,
) -> Vec<(ContainerRef<'a>, Option<usize>)> {
    let mut entries: Vec<(ContainerRef<'a>, Option<usize>)> = Vec::new();
    let mut groups: HashMap<(QualifiedService, MachineId), usize> = HashMap::new();
    for container in containers {
        if !is_draining(*container, retiring) {
            entries.push((*container, None));
            continue;
        }
        let observation = container.as_observation();
        let key = (observation.identity(), observation.machine_id);
        match groups.get(&key).and_then(|index| entries.get_mut(*index)) {
            Some((_, Some(count))) => *count += 1,
            _ => {
                groups.insert(key, entries.len());
                entries.push((*container, Some(1)));
            }
        }
    }
    entries
}

/// One row per Container, except that a Service's draining Containers on one
/// Server share a `draining N` row. Other rows that read the same get the short
/// Container ID, which `exec` takes.
fn process_table(
    containers: &[ContainerRef<'_>],
    scoped: bool,
    names: &HashMap<MachineId, &MachineName>,
    retiring: &HashSet<ContainerId>,
) -> Table {
    let entries = collapse_draining(containers, retiring);
    let rows: Vec<[String; 3]> = entries
        .iter()
        .map(|(container, _)| {
            let observation = container.as_observation();
            // Scoped to one Environment, its Namespace on every row says nothing.
            let service = if scoped {
                observation.service_name().to_string()
            } else {
                observation.identity().to_string()
            };
            [
                service,
                process_kind(*container).to_owned(),
                server(names, &observation.machine_id),
            ]
        })
        .collect();
    let mut seen: HashMap<&[String; 3], usize> = HashMap::new();
    for (row, _) in rows
        .iter()
        .zip(&entries)
        .filter(|(_, (_, draining))| draining.is_none())
    {
        *seen.entry(row).or_default() += 1;
    }
    let clashes = |row: &[String; 3]| seen.get(row).is_some_and(|count| *count > 1);
    let clash = seen.values().any(|count| *count > 1);
    let mut header = vec!["SERVICE", "KIND", "SERVER", "STATE"];
    if clash {
        header.push("CONTAINER");
    }
    let mut table = Table::new(header, "No Containers running yet.");
    for ((container, draining), row) in entries.iter().zip(&rows) {
        let observation = container.as_observation();
        let mut cells: Vec<Cell> = row.iter().map(Cell::from).collect();
        cells.push(match draining {
            Some(count) => Cell::status(format!("draining {count}"), Tone::Change),
            None => Cell::status(process_state(&observation.runtime), state_tone(*container)),
        });
        if clash {
            cells.push(if draining.is_none() && clashes(row) {
                Cell::from(crate::ui::short_id(observation.container_id.as_str()))
            } else {
                Cell::from("")
            });
        }
        table.row(cells);
    }
    table
}

/// Green when the Container does its job, red when it needs a look.
fn state_tone(container: ContainerRef<'_>) -> Tone {
    match health_rank(container) {
        0 => Tone::Bad,
        1 => Tone::Change,
        _ => Tone::Good,
    }
}

fn sort_processes(containers: &mut [ContainerRef<'_>], sort: &str) {
    containers.sort_by(|left, right| {
        let left_observation = left.as_observation();
        let right_observation = right.as_observation();
        let primary = match sort {
            "health" => health_rank(*left).cmp(&health_rank(*right)).then_with(|| {
                left_observation
                    .identity()
                    .cmp(&right_observation.identity())
            }),
            "machine" => left_observation
                .machine_id
                .as_str()
                .cmp(right_observation.machine_id.as_str())
                .then_with(|| {
                    left_observation
                        .identity()
                        .cmp(&right_observation.identity())
                }),
            _ => left_observation
                .identity()
                .cmp(&right_observation.identity()),
        };
        primary.then_with(|| {
            left_observation
                .container_id
                .as_str()
                .cmp(right_observation.container_id.as_str())
        })
    });
}

fn process_kind(container: ContainerRef<'_>) -> &'static str {
    match container {
        ContainerRef::Service(_) => "service",
        ContainerRef::Hook(_) => "pre-deploy hook",
    }
}

/// A runtime state in words: `running`, `running, unhealthy`, `exited with code 1`.
fn process_state(runtime: &ContainerRuntimeObservation) -> String {
    match runtime {
        ContainerRuntimeObservation::Running {
            health: HealthObservation::NotConfigured,
        } => "running".into(),
        ContainerRuntimeObservation::Running { health } => format!("running, {}", health.as_str()),
        other @ (ContainerRuntimeObservation::Created
        | ContainerRuntimeObservation::Paused
        | ContainerRuntimeObservation::Restarting
        | ContainerRuntimeObservation::Exited { .. }
        | ContainerRuntimeObservation::Removing
        | ContainerRuntimeObservation::Dead
        | ContainerRuntimeObservation::Unknown { .. }) => other.to_string(),
    }
}

fn health_rank(container: ContainerRef<'_>) -> u8 {
    if let ContainerRef::Hook(container) = container
        && matches!(
            container.as_observation().runtime,
            ContainerRuntimeObservation::Exited { code: 0 }
        )
    {
        return 3;
    }
    runtime_health_rank(&container.as_observation().runtime)
}

fn runtime_health_rank(runtime: &ContainerRuntimeObservation) -> u8 {
    match runtime {
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy | HealthObservation::Failing,
        }
        | ContainerRuntimeObservation::Dead => 0,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        } => 2,
        ContainerRuntimeObservation::Running { .. } => 3,
        ContainerRuntimeObservation::Created
        | ContainerRuntimeObservation::Paused
        | ContainerRuntimeObservation::Restarting
        | ContainerRuntimeObservation::Exited { .. }
        | ContainerRuntimeObservation::Removing
        | ContainerRuntimeObservation::Unknown { .. } => 1,
    }
}

/// Stop, then start, the addressed Environment's Services. Every Container is started
/// again even when stopping one failed, so a failed restart never leaves a Service down.
fn restart(root: &ArgMatches) -> Result<(), Error> {
    lifecycle(root, &[ContainerAction::Stop, ContainerAction::Start])
}

/// Start or stop the addressed Environment's Services.
fn lifecycle(root: &ArgMatches, actions: &'static [ContainerAction]) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let namespace = super::operator::scope(root)?;
    let selectors = change_selectors(leaf, namespace.as_ref())?;
    let (signal, timeout) = stop_options(leaf, actions)?;
    let hint = super::store::next(leaf, &["ps"]);
    with_client(root, |client| {
        Box::pin(async move {
            let machines = client.machines().await?;
            let live = client
                .live_services_from(&machines, EnvironmentValues::Redacted)
                .await?;
            let names: HashMap<MachineId, &MachineName> = machines
                .iter()
                .map(|observed| (observed.machine.id, &observed.machine.name))
                .collect();
            Gaps::of(&live.containers)
                .named(names.iter().map(|(machine_id, name)| (*machine_id, *name)))
                .warn();
            let observed = live.services();
            let services = select_services(&observed, &selectors)?;
            // A restart rolls: each Container is stopped, then started and serving, before the next one stops,
            // so a Service with more than one replica keeps serving throughout.
            let rolling: Vec<ServiceObservation> =
                if actions == [ContainerAction::Stop, ContainerAction::Start] {
                    services
                        .iter()
                        .flat_map(|service| {
                            service
                                .containers
                                .iter()
                                .map(|container| ServiceObservation {
                                    containers: vec![container.clone()],
                                    hook_containers: Vec::new(),
                                    ..(*service).clone()
                                })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
            let batches: Vec<Vec<&ServiceObservation>> = if rolling.is_empty() {
                vec![services]
            } else {
                rolling.iter().map(|one| vec![one]).collect()
            };
            let mut outcome: Option<ServiceActionOutcome> = None;
            for services in &batches {
                for &action in actions {
                    let (signal, timeout) = match action {
                        ContainerAction::Stop => (signal.clone(), timeout),
                        ContainerAction::Start | ContainerAction::Remove => (None, None),
                    };
                    let step = apply_service_action(
                        client, &live, &names, services, action, signal, timeout,
                    )
                    .await?;
                    outcome = Some(match outcome {
                        Some(before) => before.then(step),
                        None => step,
                    });
                }
            }
            let outcome = outcome.expect("a lifecycle command has an action");
            crate::ui::emit(&ServiceActionResult {
                next: outcome.partial.then_some(hint.as_str()),
                ..outcome.result(&live)
            })?;
            if outcome.partial {
                Err(Error::partial())
            } else {
                Ok(())
            }
        })
    })
}

struct ServiceActionOutcome {
    /// One entry per Container the action reached.
    containers: Vec<ChangedContainer>,
    /// One entry per Container the action failed on.
    container_failures: Vec<ContainerFailure>,
    partial: bool,
}

#[derive(Serialize)]
struct ChangedContainer {
    action: String,
    service: QualifiedService,
    machine_id: MachineId,
    container_id: ContainerId,
}

#[derive(Serialize)]
struct ContainerFailure {
    machine_id: MachineId,
    container_id: ContainerId,
    error: RpcError,
}

/// The `--json` result of start and stop.
#[derive(Serialize)]
struct ServiceActionResult<'a> {
    containers: &'a [ChangedContainer],
    /// Containers the action failed on.
    container_failures: &'a [ContainerFailure],
    /// Machines whose Live Observation failed before the action.
    failures: &'a [MachineFailure<RpcError>],
    omitted: &'a [MachineId],
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<&'a str>,
}

impl ServiceActionOutcome {
    fn result<'a>(&'a self, live: &'a LiveServices<RpcError>) -> ServiceActionResult<'a> {
        ServiceActionResult {
            containers: &self.containers,
            container_failures: &self.container_failures,
            failures: &live.containers.failures,
            omitted: &live.containers.omissions,
            next: None,
        }
    }

    /// This step's outcome followed by `later`'s.
    fn then(mut self, later: Self) -> Self {
        self.containers.extend(later.containers);
        self.container_failures.extend(later.container_failures);
        self.partial |= later.partial;
        self
    }
}

async fn apply_service_action(
    client: &crate::connect::Client,
    live: &LiveServices<RpcError>,
    names: &HashMap<MachineId, &MachineName>,
    services: &[&ServiceObservation],
    action: ContainerAction,
    signal: Option<String>,
    timeout: Option<u16>,
) -> Result<ServiceActionOutcome, Error> {
    let service_container_ids = services
        .iter()
        .copied()
        .flat_map(|service| service.containers_for(action))
        .map(|container| container.as_observation().container_id)
        .collect::<HashSet<_>>();
    let mut changed = Vec::new();
    let mut rows = Vec::new();
    let mut container_failures = Vec::new();
    let mut partial = false;
    for service in services {
        let outcomes = client
            .change_observed_service(service, action, signal.clone(), timeout)
            .await;
        for success in outcomes.successes {
            crate::ui::stream(format_args!(
                "{} {} on {}.",
                past(action),
                service.identity,
                server(names, &success.machine_id)
            ));
            rows.push(ChangedContainer {
                action: action.to_string(),
                service: service.identity.clone(),
                machine_id: success.machine_id,
                container_id: success.value,
            });
            if service_container_ids.contains(&success.value) {
                changed.push((success.machine_id, success.value));
            }
        }
        for failure in outcomes.failures {
            crate::ui::warn_cause(
                format_args!(
                    "Could not {action} {} on {}",
                    service.identity,
                    server(names, &failure.machine_id)
                ),
                &failure.error.error,
            );
            container_failures.push(ContainerFailure {
                machine_id: failure.machine_id,
                container_id: failure.error.container_id,
                error: failure.error.error,
            });
            partial = true;
        }
    }
    let cancellation = cancellation_on_ctrl_c();
    let _parent = cancellation.clone().drop_guard();
    if action == ContainerAction::Start {
        // A start waits for each healthcheck the way a Deploy does.
        let waits = join_all(changed.iter().map(|(machine_id, container_id)| async {
            client
                .wait_until_ready(machine_id, container_id, &cancellation)
                .await
        }))
        .await;
        for ((machine_id, container_id), wait) in changed.into_iter().zip(waits) {
            let error = match wait {
                Ok(()) => continue,
                Err(HealthWaitError::Inspect(error)) => error,
                Err(HealthWaitError::Health(failure)) => unhealthy(&failure),
            };
            crate::ui::warn_cause(
                format_args!(
                    "{container_id} on {} did not become healthy",
                    server(names, &machine_id)
                ),
                &error,
            );
            container_failures.push(ContainerFailure {
                machine_id,
                container_id,
                error,
            });
            partial = true;
        }
    }
    if !live.containers.all_targets_succeeded() {
        partial = true;
    }
    Ok(ServiceActionOutcome {
        containers: rows,
        container_failures,
        partial,
    })
}

fn unhealthy(failure: &HealthFailure) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: format!("The Container's healthcheck did not pass: {failure}."),
        details: serde_json::to_value(failure).unwrap_or_default(),
        cause: Vec::new(),
    }
}

fn change_selectors(
    matches: &ArgMatches,
    namespace: Option<&super::operator::Scoped>,
) -> Result<Vec<ServiceSelector>, Error> {
    matches
        .get_many::<String>("service")
        .ok_or_else(|| Error::usage("at least one Service selector is required"))?
        .map(|selector| {
            super::operator::in_scope(ServiceSelector::parse(selector.as_str())?, namespace)
        })
        .collect()
}

fn select_services<'a>(
    services: &'a [ployz_core::ServiceObservation],
    selectors: &[ServiceSelector],
) -> Result<Vec<&'a ployz_core::ServiceObservation>, Error> {
    let mut seen = HashSet::new();
    let mut selected = Vec::new();
    for selector in selectors {
        let service = select_service(services, selector)?;
        if seen.insert(&service.identity) {
            selected.push(service);
        }
    }
    Ok(selected)
}

fn stop_options(
    matches: &ArgMatches,
    actions: &[ContainerAction],
) -> Result<(Option<String>, Option<u16>), Error> {
    if !actions.contains(&ContainerAction::Stop) {
        return Ok((None, None));
    }
    let signal = matches.get_one::<String>("signal").cloned();
    let timeout = matches
        .get_one::<String>("timeout")
        .map(|value| value.parse::<u16>())
        .transpose()?;
    Ok((signal, timeout))
}

/// A Server by name, else by its short ID.
fn server(names: &HashMap<MachineId, &MachineName>, machine_id: &MachineId) -> String {
    names.get(machine_id).map_or_else(
        || crate::ui::short_id(machine_id.as_str()).to_owned(),
        ToString::to_string,
    )
}

const fn past(action: ContainerAction) -> &'static str {
    match action {
        ContainerAction::Start => "Started",
        ContainerAction::Stop => "Stopped",
        ContainerAction::Remove => "Removed",
    }
}

mod authored;
#[cfg(test)]
mod tests;

pub(crate) fn command() -> Command {
    base("service", "Manage services")
        .subcommand(authored::add_command())
        .subcommand(authored::inspect_command())
        .subcommand(authored::ls_command())
        .subcommand(service_port_forward())
        .subcommand(authored::rename_command())
        .subcommand(service_restart())
        .subcommand(authored::rm_command())
        .subcommand(service_start())
        .subcommand(service_stop())
}

fn service_port_forward() -> Command {
    super::store::scoped(base(
        "port-forward",
        "Forward a local port to a Service's container until interrupted",
    ))
    .arg(positional("service", true))
    .arg(positional("port", true).help("REMOTE, or LOCAL:REMOTE; LOCAL 0 picks a free port"))
}

fn services() -> Arg {
    Arg::new("service")
        .required(true)
        .num_args(1..)
        .action(ArgAction::Append)
}

fn service_restart() -> Command {
    stop_flags(base(
        "restart",
        "Stop, then start, a Service's containers; its configuration is unchanged",
    ))
}

fn service_start() -> Command {
    super::store::scoped(base("start", "Start a Service's stopped containers")).arg(services())
}

fn service_stop() -> Command {
    stop_flags(base(
        "stop",
        "Stop a Service's containers; they stay until started or deployed",
    ))
}

fn stop_flags(command: Command) -> Command {
    super::store::scoped(command)
        .arg(services())
        .arg(value("signal", None).default_value("SIGTERM"))
        .arg(value("timeout", Some('t')).default_value("10"))
}

pub(super) fn handler(path: &str) -> Option<Runnable> {
    Some(match path {
        "add" => cloud(Never, authored::add),
        "inspect" => cloud(Never, authored::inspect),
        "ls" => cloud(Never, authored::list),
        "port-forward" => cloud(Never, super::operator::port_forward),
        "rename" => cloud(Never, authored::rename),
        "restart" => cloud(Never, restart),
        "rm" => cloud(Never, authored::remove),
        "start" => cloud(Never, |root| lifecycle(root, &[ContainerAction::Start])),
        "stop" => cloud(Always, |root| lifecycle(root, &[ContainerAction::Stop])),
        _ => return None,
    })
}
