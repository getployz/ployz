//! The Deploy pipeline: snapshot → plan → execute.
//!
//! A Deploy Snapshot is gathered, a Deploy Preview is calculated, and a Deploy
//! Plan is executed to a Deploy Outcome. This module does not print, read
//! stdin, or exit the process.

use std::fmt::{self, Display, Formatter};
use std::num::NonZeroU32;
use std::time::SystemTime;

use ployz_core::{
    ListMachinesRequest, MachineId, MachineObservation, PartialResult, PortPublication,
    RequestedServiceSpec, ResolvedServiceSpec, RpcError, ServiceId, ServiceMode, UpdateConfig, op,
    select_service,
};
use tokio_util::sync::CancellationToken;

use crate::{
    compose::{BuildService, ComposeProject, plan_compose_deploy},
    connect::Client,
    failure::Failure,
    image::PushError,
};

use super::{
    DeployOperation, DeployOutcome, DeployPlan, DeploySnapshot, ExecutionError, PlanOptions,
    exec::execute_operations, plan_deploy,
};

/// Observer-relative plan-plus-warnings offered for confirmation before one Deploy executes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DeployPreview {
    pub operations: Vec<DeployOperation>,
    pub warnings: Vec<ObservationWarning>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PushOutcome {
    pub pushed: Vec<PushedImage>,
    pub failures: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PushedImage {
    pub image: String,
    pub machine_id: MachineId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ObservationKind {
    Container,
    Volume,
}

impl Display for ObservationKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Container => f.write_str("container"),
            Self::Volume => f.write_str("volume"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ObservationWarning {
    Failed {
        kind: ObservationKind,
        machine_id: MachineId,
        message: String,
    },
    Omitted {
        kind: ObservationKind,
        machine_id: MachineId,
    },
}

pub(super) async fn plan_spec(
    client: &mut Client,
    requested: &RequestedServiceSpec,
) -> Result<DeployPreview, Failure> {
    let machines = list_machines(client).await?;
    let (snapshot, warnings) = gather_snapshot(client, machines).await?;
    let mut requested = requested.clone();
    expand_ingress(client, std::iter::once(&mut requested)).await?;
    let plan = plan_deploy(
        &requested,
        &snapshot,
        ServiceId::random(),
        plan_options(false, false),
    )?;
    Ok(DeployPreview {
        operations: plan.operations().to_vec(),
        warnings,
    })
}

pub(super) async fn push_project_images(
    client: &mut Client,
    builds: &[BuildService],
    machines: &[MachineObservation],
) -> Result<PushOutcome, Failure> {
    let mut pushed = Vec::new();
    let mut failures = Vec::new();
    for service in builds {
        match push_image(client, service, machines).await {
            Ok((images, service_failures)) => {
                pushed.extend(images);
                failures.extend(service_failures);
            }
            Err(error) => failures.push(format!("{}: {error}", service.image)),
        }
    }
    Ok(PushOutcome { pushed, failures })
}

pub(super) async fn plan_project(
    client: &mut Client,
    project: &mut ComposeProject,
    machines: Vec<MachineObservation>,
    options: PlanOptions,
) -> Result<DeployPreview, Failure> {
    project.resolve_secrets()?;
    let (snapshot, warnings) = gather_snapshot(client, machines).await?;
    expand_ingress(client, project.services.values_mut()).await?;
    let compose = plan_compose_deploy(project, &snapshot, options)?;
    // TODO(UT-085): services absent from this finite project are intentionally not removed.
    Ok(DeployPreview {
        operations: compose
            .operations()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        warnings,
    })
}

pub(super) async fn plan_scale(
    client: &mut Client,
    selector: &str,
    replicas: NonZeroU32,
) -> Result<DeployPreview, Failure> {
    let machines = list_machines(client).await?;
    let (snapshot, warnings) = gather_snapshot(client, machines).await?;
    let operations = match scale_plan(&snapshot, selector, replicas)? {
        Some(plan) => plan.operations().to_vec(),
        None => Vec::new(),
    };
    Ok(DeployPreview {
        operations,
        warnings,
    })
}

pub(super) async fn execute_deploy(
    client: &mut Client,
    operations: &[DeployOperation],
) -> DeployOutcome<ExecutionError> {
    execute_operations(operations, client, &CancellationToken::new()).await
}

pub(super) fn plan_options(force_recreate: bool, skip_health_monitor: bool) -> PlanOptions {
    PlanOptions {
        force_recreate,
        skip_health_monitor,
        placement_seed: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64),
    }
}

fn scale_plan(
    snapshot: &DeploySnapshot,
    selector: &str,
    replicas: NonZeroU32,
) -> Result<Option<DeployPlan>, Failure> {
    let services = ployz_core::derive_services(snapshot.containers.iter().cloned());
    let service = select_service(&services, selector)?;
    let observed_container = service
        .containers
        .first()
        .ok_or_else(|| Failure::usage("cannot scale a service without regular containers"))?;
    match observed_container.resolved_spec.mode {
        ServiceMode::Replicated { .. } => {}
        ServiceMode::Global => return Err(Failure::usage("global services cannot be scaled")),
    }
    if usize::try_from(replicas.get()) == Ok(service.containers.len()) {
        return Ok(None);
    }
    // TODO(UT-046): mixed historical specs use one observed regular container; there is no chooser.
    let mut requested = requested_from_resolved(&observed_container.resolved_spec);
    requested.mode = ServiceMode::Replicated { replicas };
    Ok(Some(plan_deploy(
        &requested,
        snapshot,
        service.service_id,
        plan_options(false, false),
    )?))
}

fn requested_from_resolved(resolved: &ResolvedServiceSpec) -> RequestedServiceSpec {
    RequestedServiceSpec {
        name: resolved.name.clone(),
        mode: resolved.mode.clone(),
        container: resolved.container.clone(),
        placement: resolved.placement.clone(),
        ports: resolved.ports.clone(),
        volumes: resolved.volumes.clone(),
        mounts: resolved.mounts.clone(),
        configs: resolved.configs.clone(),
        pre_deploy: resolved.pre_deploy.clone(),
        caddy_config: resolved.caddy_config.clone(),
        update: UpdateConfig {
            order: Some(resolved.update.order),
            monitor_millis: resolved.update.monitor_millis,
        },
    }
}

pub(super) async fn list_machines(client: &mut Client) -> Result<Vec<MachineObservation>, Failure> {
    Ok(client
        .call::<op::ListMachines>(ListMachinesRequest {}, None)
        .await?
        .machines)
}

async fn gather_snapshot(
    client: &mut Client,
    machines: Vec<MachineObservation>,
) -> Result<(DeploySnapshot, Vec<ObservationWarning>), Failure> {
    let gathered = client.deploy_snapshot(machines).await?;
    let mut warnings = observation_warnings(ObservationKind::Container, &gathered.containers);
    warnings.extend(observation_warnings(
        ObservationKind::Volume,
        &gathered.volumes,
    ));
    Ok((gathered.snapshot, warnings))
}

fn observation_warnings<T>(
    kind: ObservationKind,
    result: &PartialResult<T, RpcError>,
) -> Vec<ObservationWarning> {
    result
        .failures
        .iter()
        .map(|failure| ObservationWarning::Failed {
            kind,
            machine_id: failure.machine_id,
            message: failure.error.message.clone(),
        })
        .chain(
            result
                .omissions
                .iter()
                .map(|machine| ObservationWarning::Omitted {
                    kind,
                    machine_id: *machine,
                }),
        )
        .collect()
}

async fn expand_ingress<'a>(
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

async fn push_image(
    client: &mut Client,
    service: &BuildService,
    machines: &[MachineObservation],
) -> Result<(Vec<PushedImage>, Vec<String>), PushError> {
    let targets = service
        .machines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let result =
        crate::image::push_using_machines(client, &service.image, None, &targets, machines).await?;
    let pushed = result
        .successes
        .iter()
        .map(|success| PushedImage {
            image: service.image.clone(),
            machine_id: success.machine_id,
        })
        .collect();
    let failures = result
        .failures
        .into_iter()
        .map(|failure| {
            format!(
                "{} on {}: {}",
                service.image, failure.machine_id, failure.error
            )
        })
        .chain(
            result
                .omissions
                .into_iter()
                .map(|machine| format!("{} on {machine}: no terminal response", service.image)),
        )
        .collect();
    Ok((pushed, failures))
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
