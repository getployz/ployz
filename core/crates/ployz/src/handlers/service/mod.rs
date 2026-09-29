use std::collections::HashSet;

use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::cli::{base, log_flags, positional, switch, trailing, value};
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
    with_client(root, |client| {
        Box::pin(async move {
            let live = client.live_services(EnvironmentValues::Redacted).await?;
            print_observation_warning(&live);
            let services = live.services();
            let mut containers = services
                .iter()
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
                        say!(
                            "{}\t{}\t{}\t{}\t{}",
                            observation.container_id,
                            observation.identity(),
                            process_kind(*container),
                            observation.machine_id,
                            observation.runtime
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
        ContainerRef::Service(_) => "ServiceContainer",
        ContainerRef::Hook(_) => "PreDeployHook",
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
            health: HealthObservation::Unhealthy,
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

/// Start or stop observed Services.
///
/// # Errors
///
/// Returns a connection, RPC, usage, or wait error.
pub fn change(root: &ArgMatches, action: ContainerAction) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let selectors = change_selectors(leaf)?;
    let (signal, timeout) = stop_options(leaf, action)?;
    with_client(root, |client| {
        Box::pin(async move {
            let live = client.live_services(EnvironmentValues::Redacted).await?;
            print_observation_warning(&live);
            let observed = live.services();
            let services = select_services(&observed, &selectors)?;
            let outcome =
                apply_service_action(client, &live, &services, action, signal, timeout).await?;
            output::emit(&outcome.result(&live))?;
            service_action_result(outcome.partial)
        })
    })
}

fn service_action_result(partial: bool) -> Result<(), Error> {
    if partial {
        Err(Error::usage("Service lifecycle completed partially"))
    } else {
        Ok(())
    }
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
}

impl ServiceActionOutcome {
    fn result<'a>(&'a self, live: &'a LiveServices<RpcError>) -> ServiceActionResult<'a> {
        ServiceActionResult {
            containers: &self.containers,
            container_failures: &self.container_failures,
            failures: &live.containers.failures,
            omitted: &live.containers.omissions,
            wait_error: self.wait_error.as_ref(),
        }
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

fn change_selectors(matches: &ArgMatches) -> Result<Vec<ServiceSelector>, Error> {
    matches
        .get_many::<String>("service")
        .ok_or_else(|| Error::usage("at least one Service selector is required"))?
        .map(|selector| ServiceSelector::parse(selector.as_str()).map_err(Into::into))
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
    action: ContainerAction,
) -> Result<(Option<String>, Option<i32>), Error> {
    if action != ContainerAction::Stop {
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
        .subcommand(service_exec())
        .subcommand(authored::inspect_command())
        .subcommand(authored::ls_command())
        .subcommand(service_logs())
        .subcommand(service_proxy())
        .subcommand(service_ps())
        .subcommand(authored::rename_command())
        .subcommand(authored::rm_command())
        .subcommand(service_start())
        .subcommand(service_stop())
}

fn service_exec() -> Command {
    base("exec", "Execute a command in a service container")
        .arg(value("container", None))
        .arg(switch("detach", Some('d')))
        .arg(switch("no-tty", Some('T')))
        .arg(positional("service", true))
        .arg(trailing("command"))
}

fn service_logs() -> Command {
    log_flags(base("logs", "Show logs")).arg(
        Arg::new("service-or-container")
            .required(true)
            .num_args(1..)
            .action(ArgAction::Append),
    )
}

fn service_proxy() -> Command {
    base("proxy", "Proxy a local port to a service")
        .arg(positional("service", true))
        .arg(positional("port", true))
}

fn service_ps() -> Command {
    base("ps", "List service containers").arg(
        value("sort", None)
            .default_value("service")
            .value_parser(["service", "machine", "health"]),
    )
}

fn services() -> Arg {
    Arg::new("service")
        .required(true)
        .num_args(1..)
        .action(ArgAction::Append)
}

fn service_start() -> Command {
    base("start", "Start services").arg(services())
}

fn service_stop() -> Command {
    base("stop", "Stop services")
        .arg(services())
        .arg(value("signal", None).default_value("SIGTERM"))
        .arg(value("timeout", Some('t')).default_value("10"))
}

pub(super) fn handler(path: &str) -> Option<(super::Handler, super::Json)> {
    use super::Json::{Refused, Supported};
    Some(match path {
        "add" => (authored::add, Supported),
        "exec" => (super::operator::exec, Refused),
        "inspect" => (authored::inspect, Supported),
        "logs" => (super::operator::service_logs, Supported),
        "ls" => (authored::list, Supported),
        "proxy" => (super::operator::proxy, Refused),
        "ps" => (processes, Supported),
        "rename" => (authored::rename, Supported),
        "rm" => (authored::remove, Supported),
        "start" => (|root| change(root, ContainerAction::Start), Supported),
        "stop" => (|root| change(root, ContainerAction::Stop), Supported),
        _ => return None,
    })
}
