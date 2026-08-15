use std::time::SystemTime;

use ployz_core::{
    ListMachinesRequest, PartialResult, PortPublication, RequestedServiceSpec, RpcError, ServiceId,
    op,
};
use tokio_util::sync::CancellationToken;

use crate::{connect::Client, failure::Failure};

use super::{
    DeployOperation, DeployOutcome, DeploySnapshot, ExecutionError, FailedOperation, PlanOptions,
    ReplacementOperation, execute_operations, plan_deploy,
};

pub(crate) async fn apply_requested(
    client: &mut Client,
    requested: &RequestedServiceSpec,
) -> Result<(), Failure> {
    let machines = list_machines(client).await?;
    let snapshot = take_snapshot(client, machines).await?;
    let mut requested = requested.clone();
    expand_ingress(client, std::iter::once(&mut requested)).await?;
    let plan = plan_deploy(
        &requested,
        &snapshot,
        ServiceId::random(),
        plan_options(false, false),
    )?;
    render(plan.operations(), client.connection());
    finish(execute_operations(plan.operations(), client, &CancellationToken::new()).await)
}

pub(crate) async fn list_machines(
    client: &mut Client,
) -> Result<Vec<ployz_core::MachineObservation>, Failure> {
    Ok(client
        .call::<op::ListMachines>(ListMachinesRequest {}, None)
        .await?
        .machines)
}

pub(crate) async fn take_snapshot(
    client: &mut Client,
    machines: Vec<ployz_core::MachineObservation>,
) -> Result<DeploySnapshot, Failure> {
    let gathered = client.deploy_snapshot(machines).await?;
    report_observation_warnings("container", &gathered.containers);
    report_observation_warnings("volume", &gathered.volumes);
    Ok(gathered.snapshot)
}

fn report_observation_warnings<T>(kind: &str, result: &PartialResult<T, RpcError>) {
    for failure in &result.failures {
        eprintln!(
            "WARNING: {kind} observation failed on {}: {}",
            failure.machine_id, failure.error.message
        );
    }
    for machine in &result.omissions {
        eprintln!("WARNING: {kind} observation omitted {machine}");
    }
}

pub(crate) async fn expand_ingress<'a>(
    client: &mut Client,
    specs: impl IntoIterator<Item = &'a mut RequestedServiceSpec>,
) -> Result<(), Failure> {
    let specs: Vec<_> = specs.into_iter().collect();
    if !specs.iter().any(|spec| needs_ingress_expansion(spec)) {
        return Ok(());
    }
    let domain = client.domain_if_reserved().await?;
    for spec in specs {
        crate::dns::expand_ingress_ports(spec, domain.as_deref())?;
    }
    Ok(())
}

fn needs_ingress_expansion(requested: &RequestedServiceSpec) -> bool {
    requested
        .ports
        .iter()
        .any(|port| matches!(port, PortPublication::Ingress { .. }))
}

pub(crate) fn plan_options(force_recreate: bool, skip_health_monitor: bool) -> PlanOptions {
    PlanOptions {
        force_recreate,
        skip_health_monitor,
        placement_seed: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64),
    }
}

pub(crate) fn render(operations: &[DeployOperation], connection: &crate::context::Connection) {
    println!("Plan for {connection}:");
    for operation in operations {
        println!("  {}", operation_summary(operation));
    }
}

pub(crate) fn finish(outcome: DeployOutcome<ExecutionError>) -> Result<(), Failure> {
    println!("Completed {} operation(s).", outcome.completed.len());
    let Some(failed) = &outcome.failed else {
        return Ok(());
    };
    Err(Failure::usage(format!(
        "Deploy stopped; completed: [{}]; failed: {}; unexecuted: [{}]",
        operation_list(&outcome.completed),
        failed_summary(failed),
        operation_list(&outcome.unexecuted),
    )))
}

fn failed_summary(failed: &FailedOperation<ExecutionError>) -> String {
    match failed {
        FailedOperation::Operation { operation, error } => {
            format!("{}: {error}", operation_summary(operation))
        }
        FailedOperation::ReplacementHealth {
            operation,
            error,
            compensation,
        } => format!(
            "{}: {error}; compensation: {compensation:?}",
            replacement_summary(operation),
        ),
    }
}

fn operation_summary(operation: &DeployOperation) -> String {
    match operation {
        DeployOperation::CreateVolume { machine_id, volume } => {
            format!("create volume {} on {machine_id}", volume.reference)
        }
        DeployOperation::RunContainer {
            machine_id, spec, ..
        } => format!("run {} on {machine_id}", spec.name),
        DeployOperation::StopContainer {
            machine_id,
            container_id,
        } => format!("stop {container_id} on {machine_id}"),
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
        } => format!("remove {container_id} on {machine_id}"),
        DeployOperation::ReplaceContainer(operation) => replacement_summary(operation),
        DeployOperation::StopHook {
            machine_id,
            container_id,
        } => format!("stop hook {container_id} on {machine_id}"),
        DeployOperation::RunHook {
            machine_id, spec, ..
        } => format!("run pre-deploy hook for {} on {machine_id}", spec.name),
        DeployOperation::Sequence { operations } => format!("{} operations", operations.len()),
    }
}

fn replacement_summary(operation: &ReplacementOperation) -> String {
    format!(
        "replace {} for {} on {}",
        operation.old_container_id, operation.spec.name, operation.machine_id
    )
}

fn operation_list(operations: &[DeployOperation]) -> String {
    operations
        .iter()
        .map(operation_summary)
        .collect::<Vec<_>>()
        .join(", ")
}
