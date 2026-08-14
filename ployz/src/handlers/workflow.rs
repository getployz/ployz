use std::{num::NonZeroU32, time::SystemTime};

use clap::ArgMatches;
use ployz_core::{RequestedServiceSpec, ServiceId, ServiceMode, select_service};
use tokio_util::sync::CancellationToken;

use crate::{
    compose::{
        BuildOptions, BuildService, ComposeError, ComposeProject, LoadOptions, execute_build,
        load_project, plan_build, plan_compose_deploy,
    },
    connect::Client,
    deploy::{
        DeployOperation, DeployOutcome, DeploySnapshot, ExecutionError, FailedOperation,
        ObservedDockerVolume, PlanOptions, ReplacementOperation, execute_operations, plan_deploy,
    },
};

use super::{
    Error,
    build::{push_targets, report_push},
    confirm, connect_client, leaf_matches, required, runtime, string_values,
};

pub(super) fn run(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let requested = run_spec(matches)?;
    let context = matches.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let mut client = connect_client(root, context).await?;
        let outcome = run_connected(
            &mut RemoteWorkflow {
                client: &mut client,
                auto_confirm: true,
            },
            &requested,
        )
        .await?;
        finish(outcome)
    })
}

pub(super) fn deploy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let (mut project, builds) = prepare_deploy(matches, |_| {}, execute_build)?;
    let context = project
        .selected_context(
            matches.get_one::<String>("context").map(String::as_str),
            matches.get_one::<String>("connect").map(String::as_str),
        )
        .map(str::to_owned);
    let yes = matches.get_flag("yes");
    let options = plan_options(
        matches.get_flag("recreate"),
        matches.get_flag("skip-health"),
    );
    runtime()?.block_on(async {
        let mut client = connect_client(root, context.as_deref()).await?;
        let outcome = deploy_connected(
            &mut RemoteWorkflow {
                client: &mut client,
                auto_confirm: yes,
            },
            &mut project,
            &builds,
            options,
        )
        .await?;
        outcome.map_or(Ok(()), finish)
    })
}

fn prepare_deploy(
    matches: &ArgMatches,
    mut record: impl FnMut(WorkflowStage),
    build: impl FnOnce(&[BuildService], &BuildOptions, &LoadOptions) -> Result<(), ComposeError>,
) -> Result<(ComposeProject, Vec<BuildService>), Error> {
    record(WorkflowStage::Load);
    let load = LoadOptions {
        command: "deploy".into(),
        files: string_values(matches, "file")
            .into_iter()
            .map(Into::into)
            .collect(),
        profiles: string_values(matches, "profile"),
        ..Default::default()
    };
    let selected = string_values(matches, "service");
    let project = load_project(&load).map_err(|error| error.to_string())?;
    for warning in &project.warnings {
        eprintln!("WARNING: {warning}");
    }
    record(WorkflowStage::Select);
    let build_options = BuildOptions {
        build_args: string_values(matches, "build-arg"),
        deps: true,
        no_cache: matches.get_flag("no-cache"),
        pull: matches.get_flag("build-pull"),
        services: selected.clone(),
        ..Default::default()
    };
    record(WorkflowStage::Build);
    let mut builds = plan_build(&project, &build_options).map_err(|error| error.to_string())?;
    if matches.get_flag("no-build") {
        builds.clear();
    } else {
        build(&builds, &build_options, &load).map_err(|error| error.to_string())?;
    }
    let project = project
        .select_services(&selected)
        .map_err(|error| error.to_string())?;
    Ok((project, builds))
}

pub(super) fn scale(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let replicas = parse_u32(matches, "replicas")?;
    // TODO(EO-012): reject zero before resolving configuration or connecting to a Machine.
    let replicas = NonZeroU32::new(replicas)
        .ok_or_else(|| Error::from("replicas must be greater than zero"))?;
    let selector = required(matches, "service")?;
    let yes = matches.get_flag("yes");
    let context = matches.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let mut client = connect_client(root, context).await?;
        let outcome = scale_connected(
            &mut RemoteWorkflow {
                client: &mut client,
                auto_confirm: yes,
            },
            &selector,
            replicas,
        )
        .await?;
        outcome.map_or(Ok(()), finish)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkflowStage {
    Load,
    Select,
    Build,
    Snapshot,
    Push,
    ResolveSecrets,
    Plan,
    Render,
    Confirm,
    Execute,
}

trait WorkflowIo {
    fn record(&mut self, _stage: WorkflowStage) {}
    async fn machines(&mut self) -> Result<Vec<ployz_core::MachineObservation>, Error>;
    async fn push(
        &mut self,
        service: &BuildService,
        machines: &[ployz_core::MachineObservation],
    ) -> Result<(), Error>;
    async fn snapshot(
        &mut self,
        machines: Option<Vec<ployz_core::MachineObservation>>,
    ) -> Result<DeploySnapshot, Error>;
    fn render(&mut self, operations: &[DeployOperation]);
    fn confirmation_required(&self) -> bool;
    fn confirm(&mut self) -> Result<bool, Error>;
    async fn execute(&mut self, operations: &[DeployOperation]) -> DeployOutcome<ExecutionError>;
}

struct RemoteWorkflow<'a> {
    client: &'a mut Client,
    auto_confirm: bool,
}

impl WorkflowIo for RemoteWorkflow<'_> {
    async fn machines(&mut self) -> Result<Vec<ployz_core::MachineObservation>, Error> {
        self.client
            .list_machines()
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn push(
        &mut self,
        service: &BuildService,
        machines: &[ployz_core::MachineObservation],
    ) -> Result<(), Error> {
        let targets = push_targets(&[], &service.machines);
        let result = crate::image::push_using_machines(
            self.client,
            &service.image,
            None,
            &targets,
            machines,
        )
        .await
        .map_err(|error| format!("{}: {error}", service.image))?;
        let failures = report_push(&service.image, result);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; ").into())
        }
    }

    async fn snapshot(
        &mut self,
        machines: Option<Vec<ployz_core::MachineObservation>>,
    ) -> Result<DeploySnapshot, Error> {
        snapshot(self.client, machines).await
    }

    fn render(&mut self, operations: &[DeployOperation]) {
        render(operations, self.client.connection());
    }

    fn confirmation_required(&self) -> bool {
        !self.auto_confirm
    }

    fn confirm(&mut self) -> Result<bool, Error> {
        confirm()
    }

    async fn execute(&mut self, operations: &[DeployOperation]) -> DeployOutcome<ExecutionError> {
        execute_operations(operations, self.client, &CancellationToken::new()).await
    }
}

async fn run_connected(
    io: &mut impl WorkflowIo,
    requested: &RequestedServiceSpec,
) -> Result<DeployOutcome<ExecutionError>, Error> {
    io.record(WorkflowStage::Snapshot);
    let snapshot = io.snapshot(None).await?;
    io.record(WorkflowStage::Plan);
    let plan = plan_deploy(
        requested,
        &snapshot,
        ServiceId::random(),
        plan_options(false, false),
    )
    .map_err(|error| error.to_string())?;
    io.record(WorkflowStage::Render);
    io.render(plan.operations());
    io.record(WorkflowStage::Execute);
    Ok(io.execute(plan.operations()).await)
}

pub(super) async fn deploy_requested(
    client: &mut Client,
    requested: &RequestedServiceSpec,
) -> Result<(), Error> {
    finish(
        run_connected(
            &mut RemoteWorkflow {
                client,
                auto_confirm: true,
            },
            requested,
        )
        .await?,
    )
}

async fn deploy_connected(
    io: &mut impl WorkflowIo,
    project: &mut crate::compose::ComposeProject,
    builds: &[BuildService],
    options: PlanOptions,
) -> Result<Option<DeployOutcome<ExecutionError>>, Error> {
    let machines = io.machines().await?;
    let mut failures = Vec::new();
    for service in builds {
        io.record(WorkflowStage::Push);
        if let Err(error) = io.push(service, &machines).await {
            failures.push(error.to_string());
        }
    }
    if !failures.is_empty() {
        return Err(format!("image push failed: {}", failures.join("; ")).into());
    }
    io.record(WorkflowStage::ResolveSecrets);
    project
        .resolve_secrets()
        .map_err(|error| error.to_string())?;
    io.record(WorkflowStage::Snapshot);
    let snapshot = io.snapshot(Some(machines)).await?;
    io.record(WorkflowStage::Plan);
    let compose =
        plan_compose_deploy(project, &snapshot, options).map_err(|error| error.to_string())?;
    // TODO(UT-085): services absent from this finite project are intentionally not removed.
    let operations = compose
        .operations()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    if operations.is_empty() {
        println!("No changes.");
        return Ok(None);
    }
    // TODO(UT-086): this is a best-effort preview over one observer-relative snapshot.
    confirm_and_execute(io, &operations).await
}

async fn scale_connected(
    io: &mut impl WorkflowIo,
    selector: &str,
    replicas: NonZeroU32,
) -> Result<Option<DeployOutcome<ExecutionError>>, Error> {
    io.record(WorkflowStage::Snapshot);
    let snapshot = io.snapshot(None).await?;
    let services = ployz_core::derive_services(snapshot.containers.iter().cloned());
    let service = select_service(&services, selector).map_err(|error| error.to_string())?;
    let observed_container = service
        .containers
        .first()
        .ok_or_else(|| Error::from("cannot scale a service without regular containers"))?;
    let ServiceMode::Replicated { .. } = observed_container.resolved_spec.mode else {
        return Err("global services cannot be scaled".into());
    };
    if usize::try_from(replicas.get()) == Ok(service.containers.len()) {
        println!("No changes.");
        return Ok(None);
    }
    // TODO(UT-046): mixed historical specs use one observed regular container; there is no chooser.
    let mut requested = requested_from_resolved(&observed_container.resolved_spec);
    requested.mode = ServiceMode::Replicated { replicas };
    io.record(WorkflowStage::Plan);
    let plan = plan_deploy(
        &requested,
        &snapshot,
        service.service_id.clone(),
        plan_options(false, false),
    )
    .map_err(|error| error.to_string())?;
    confirm_and_execute(io, plan.operations()).await
}

async fn confirm_and_execute(
    io: &mut impl WorkflowIo,
    operations: &[DeployOperation],
) -> Result<Option<DeployOutcome<ExecutionError>>, Error> {
    io.record(WorkflowStage::Render);
    io.render(operations);
    if io.confirmation_required() {
        io.record(WorkflowStage::Confirm);
        if !io.confirm()? {
            println!("Cancelled. No changes were made.");
            return Ok(None);
        }
    }
    io.record(WorkflowStage::Execute);
    Ok(Some(io.execute(operations).await))
}

async fn snapshot(
    client: &mut Client,
    machines: Option<Vec<ployz_core::MachineObservation>>,
) -> Result<DeploySnapshot, Error> {
    let machines = match machines {
        Some(machines) => machines,
        None => client
            .list_machines()
            .await
            .map_err(|error| error.to_string())?,
    };
    let live = client
        .live_services_from(&machines)
        .await
        .map_err(|error| error.to_string())?;
    report_partial("container", &live.containers);
    let containers = live
        .containers
        .successes
        .into_iter()
        .flat_map(|success| success.value)
        .collect();
    let volumes = client.list_volumes(&machines).await;
    report_partial("volume", &volumes);
    let volumes = volumes
        .successes
        .into_iter()
        .flat_map(|success| success.value)
        .map(|volume| ObservedDockerVolume {
            id: volume.id,
            driver: volume.driver,
            options: volume.options,
        })
        .collect();
    Ok(DeploySnapshot {
        machines,
        containers,
        volumes,
    })
}

fn report_partial<T>(kind: &str, result: &ployz_core::PartialResult<T, ployz_core::RpcError>) {
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

fn render(operations: &[DeployOperation], connection: &crate::context::Connection) {
    println!("Plan for {connection}:");
    for operation in operations {
        println!("  {}", operation_summary(operation));
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

fn finish(outcome: DeployOutcome<ExecutionError>) -> Result<(), Error> {
    println!("Completed {} operation(s).", outcome.completed.len());
    let Some(failed) = &outcome.failed else {
        return Ok(());
    };
    Err(format!(
        "Deploy stopped; completed: [{}]; failed: {}; unexecuted: [{}]",
        operation_list(&outcome.completed),
        failed_summary(failed),
        operation_list(&outcome.unexecuted),
    )
    .into())
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

fn plan_options(force_recreate: bool, skip_health_monitor: bool) -> crate::deploy::PlanOptions {
    PlanOptions {
        force_recreate,
        skip_health_monitor,
        placement_seed: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64),
    }
}

#[path = "workflow_input.rs"]
mod input;
use input::{parse_u32, requested_from_resolved, run_spec};

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
