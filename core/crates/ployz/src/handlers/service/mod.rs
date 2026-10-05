use std::collections::HashSet;

use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::cli::{base, positional, value};
use ployz_core::{
    ContainerAction, ContainerId, ContainerRef, ContainerRuntimeObservation, HealthObservation,
    LiveServices, MachineFailure, MachineId, QualifiedService, RpcError, ServiceObservation,
    ServiceSelector, select_service,
};
use serde::Serialize;

use crate::{
    cluster::ContainerObservationCondition,
    output::{self, Gaps, say},
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
            let live = client.live_services(EnvironmentValues::Redacted).await?;
            print_observation_warning(&live);
            let services = live.services();
            let mut containers = services
                .iter()
                .filter(|service| {
                    namespace
                        .as_ref()
                        .is_none_or(|namespace| service.identity.namespace == *namespace)
                })
                .flat_map(ployz_core::ServiceObservation::members)
                .collect::<Vec<_>>();
            sort_processes(&mut containers, &sort);
            let observations = containers
                .iter()
                .map(|container| container.as_observation())
                .collect::<Vec<_>>();
            output::finish_fanout(
                "containers",
                &observations,
                &Gaps::of(&live.containers),
                || {
                    say!("CONTAINER ID\tSERVICE\tKIND\tMACHINE\tSTATE");
                    for container in &containers {
                        let observation = container.as_observation();
                        // Scoped to one Environment, its Namespace on every row says nothing.
                        let service = match &namespace {
                            Some(_) => observation.service_name().to_string(),
                            None => observation.identity().to_string(),
                        };
                        say!(
                            "{}\t{service}\t{}\t{}\t{}",
                            short(observation.container_id.as_str()),
                            process_kind(*container),
                            observation.machine_id,
                            process_state(&observation.runtime)
                        );
                    }
                },
            )
        })
    })
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

/// An ID as `docker ps` shows it: the first 12 characters; `exec` and `--json` take the whole one.
fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
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
/// Both steps wait for their Container Observations, each bounded by the barrier timeout.
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
            let live = client.live_services(EnvironmentValues::Redacted).await?;
            print_observation_warning(&live);
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
                    let step =
                        apply_service_action(client, &live, services, action, signal, timeout)
                            .await?;
                    outcome = Some(match outcome {
                        Some(before) => before.then(step),
                        None => step,
                    });
                }
            }
            let outcome = outcome.expect("a lifecycle command has an action");
            output::emit(&ServiceActionResult {
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
    /// Why the action's effect was not confirmed.
    wait_error: Option<RpcError>,
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
    wait_error: Option<&'a RpcError>,
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
            wait_error: self.wait_error.as_ref(),
            next: None,
        }
    }

    /// This step's outcome followed by `later`'s.
    fn then(mut self, later: Self) -> Self {
        self.containers.extend(later.containers);
        self.container_failures.extend(later.container_failures);
        self.wait_error = self.wait_error.or(later.wait_error);
        self.partial |= later.partial;
        self
    }
}

async fn apply_service_action(
    client: &crate::connect::Client,
    live: &LiveServices<RpcError>,
    services: &[&ServiceObservation],
    action: ContainerAction,
    signal: Option<String>,
    timeout: Option<i32>,
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
            say!(
                "{}\t{}\t{}\t{}",
                action,
                service.identity,
                success.machine_id,
                success.value
            );
            rows.push(ChangedContainer {
                action: action.to_string(),
                service: service.identity.clone(),
                machine_id: success.machine_id,
                container_id: success.value,
            });
            if service_container_ids.contains(&success.value) {
                changed.push(success.value);
            }
        }
        for failure in outcomes.failures {
            eprintln!(
                "WARNING: {} failed for {} on {}: {}",
                action, failure.error.container_id, failure.machine_id, failure.error.error.message
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
    // The action already committed: an unconfirmed wait is part of the result.
    let wait_error = client
        .wait_for_container_observations(
            &changed,
            match action {
                ContainerAction::Start => ContainerObservationCondition::Serving,
                ContainerAction::Stop | ContainerAction::Remove => {
                    ContainerObservationCondition::Dropped
                }
            },
            &cancellation,
        )
        .await
        .err();
    if let Some(error) = &wait_error {
        eprintln!("WARNING: {action} was not confirmed: {}", error.message);
        partial = true;
    }
    if !live.containers.all_targets_succeeded() {
        eprintln!("WARNING: the Service selection came from a partial Live Observation");
        partial = true;
    }
    Ok(ServiceActionOutcome {
        containers: rows,
        container_failures,
        wait_error,
        partial,
    })
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
) -> Result<(Option<String>, Option<i32>), Error> {
    if !actions.contains(&ContainerAction::Stop) {
        return Ok((None, None));
    }
    let signal = matches.get_one::<String>("signal").cloned();
    let timeout = matches
        .get_one::<String>("timeout")
        .map(|value| value.parse::<i32>())
        .transpose()?;
    Ok((signal, timeout))
}

fn print_observation_warning(live: &LiveServices<RpcError>) {
    for line in observation_warning_lines(live) {
        eprintln!("{line}");
    }
}

fn observation_warning_lines(live: &LiveServices<RpcError>) -> Vec<String> {
    let mut lines =
        vec!["WARNING: Live Observation is observer-relative and not globally complete".into()];
    lines.extend(live.containers.failures.iter().map(|failure| {
        format!(
            "WARNING: Machine {} failed: {}",
            failure.machine_id, failure.error.message
        )
    }));
    lines.extend(
        live.containers
            .omissions
            .iter()
            .map(|machine_id| format!("WARNING: Machine {machine_id} was omitted")),
    );
    lines
}

mod authored;
#[cfg(test)]
mod tests;

pub(crate) fn command() -> Command {
    base("service", "Manage services")
        .arg_required_else_help(true)
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

pub(super) fn handler(path: &str) -> Option<super::Handler> {
    Some(match path {
        "add" => authored::add,
        "inspect" => authored::inspect,
        "ls" => authored::list,
        "port-forward" => super::operator::port_forward,
        "rename" => authored::rename,
        "restart" => restart,
        "rm" => authored::remove,
        "start" => |root| lifecycle(root, &[ContainerAction::Start]),
        "stop" => |root| lifecycle(root, &[ContainerAction::Stop]),
        _ => return None,
    })
}
