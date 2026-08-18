//! The Deploy pipeline: snapshot → plan → execute.
//!
//! A Deploy Snapshot is gathered, a Deploy Preview is calculated, and a Deploy
//! Plan is executed to a Deploy Outcome. This module does not print, read
//! stdin, or exit the process.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::time::SystemTime;

use ployz_core::{
    MachineId, MachineObservation, PartialResult, PortPublication, ProjectName,
    RequestedServiceSpec, RpcError, ServiceMode, ServiceSelector, select_service,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    compose::{BuildService, ComposeProject},
    connect::{Client, ConnectError},
    dns::{DomainRequired, IngressDnsWarning, resolve_ingress_dns_warnings},
    failure::Failure,
    image::PushError,
};

use super::{
    DeployIntent, DeployOperation, DeployOutcome, DeployPreview, DeploySnapshot, DeployWarning,
    ExecutionError, ObservationKind, PlanError, PlanOptions, ServiceAttempt,
    exec::execute_operations, plan_deploy,
};

/// Snapshot, planning, or ingress-expansion failure before a Deploy executes.
///
/// Execution failure is a [`DeployOutcome::Failed`], not this error.
#[derive(Debug, Error)]
pub enum DeployError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Ingress(#[from] DomainRequired),
}

impl Client {
    /// Calculate a Deploy Preview for a Deploy Intent without executing it.
    ///
    /// Reuses the same planner, ingress expansion, and DNS warnings as the CLI.
    /// The preview is observer-relative and is not a handle: [`Self::deploy`]
    /// re-plans against a fresh snapshot rather than replaying these operations.
    ///
    /// # Errors
    ///
    /// Returns when snapshot gathering, ingress expansion, or planning fails.
    pub async fn preview(
        &mut self,
        mut intent: DeployIntent,
    ) -> Result<DeployPreview, DeployError> {
        let machines = self.machines().await?;
        let (snapshot, warnings) = gather_snapshot(self, machines).await?;
        prepare_intent(self, snapshot, warnings, &mut intent).await
    }

    /// Submit a Deploy Intent: gather a snapshot, plan, and execute.
    ///
    /// Reuses the same planner and Machine Proxy fan-out as the CLI. Execution
    /// failure is a [`DeployOutcome::Failed`] with the completed prefix, failed
    /// operation, and unexecuted suffix. This operation does not prompt, stream
    /// progress, mint operation IDs, or speak `ops.watch`. Always re-plans
    /// against a fresh snapshot; it does not replay a prior Deploy Preview.
    ///
    /// # Errors
    ///
    /// Returns when snapshot gathering, ingress expansion, or planning fails
    /// before execution starts.
    pub async fn deploy(
        &mut self,
        mut intent: DeployIntent,
    ) -> Result<DeployOutcome<ExecutionError>, DeployError> {
        let machines = self.machines().await?;
        let snapshot = self.deploy_snapshot(machines).await?.snapshot;
        expand_ingress(self, intent.target.iter_mut()).await?;
        let plan = plan_deploy(&intent, &snapshot)?;
        Ok(execute_deploy(self, &plan.operations, &intent.project_name).await)
    }
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

impl From<IngressDnsWarning> for DeployWarning {
    fn from(warning: IngressDnsWarning) -> Self {
        Self::IngressHostname(warning.to_string())
    }
}

pub(super) async fn plan_spec(
    client: &mut Client,
    requested: &RequestedServiceSpec,
    options: PlanOptions,
    project_name: &ProjectName,
) -> Result<DeployPreview, Failure> {
    let machines = list_machines(client).await?;
    let (snapshot, warnings) = gather_snapshot(client, machines).await?;
    let mut intent = DeployIntent::apply_one(project_name.clone(), requested.clone(), options);
    Ok(prepare_intent(client, snapshot, warnings, &mut intent).await?)
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
    apply: Vec<ServiceAttempt>,
    options: PlanOptions,
    project_name: &ProjectName,
) -> Result<DeployPreview, Failure> {
    project.resolve_secrets()?;
    let (snapshot, warnings) = gather_snapshot(client, machines).await?;
    super::reject_missing_external_volumes(project, &snapshot)?;
    let mut intent = DeployIntent::from_named_specs(
        project_name.clone(),
        &project.services,
        &project.dependencies,
        apply,
        options,
    );
    Ok(prepare_intent(client, snapshot, warnings, &mut intent).await?)
}

pub(super) async fn plan_scale(
    client: &mut Client,
    selector: &ServiceSelector,
    replicas: NonZeroU32,
    options: PlanOptions,
    project_name: &ProjectName,
) -> Result<DeployPreview, Failure> {
    let machines = list_machines(client).await?;
    let (snapshot, warnings) = gather_snapshot(client, machines).await?;
    let Some(requested) = choose_scale_spec(&snapshot, selector, replicas)? else {
        return Ok(DeployPreview {
            operations: Vec::new(),
            warnings,
        });
    };
    let mut intent = DeployIntent::apply_one(project_name.clone(), requested, options);
    Ok(prepare_intent(client, snapshot, warnings, &mut intent).await?)
}

async fn prepare_intent(
    client: &mut Client,
    snapshot: DeploySnapshot,
    mut warnings: Vec<DeployWarning>,
    intent: &mut DeployIntent,
) -> Result<DeployPreview, DeployError> {
    expand_ingress(client, intent.target.iter_mut()).await?;
    warnings.extend(hostname_warnings(intent.target.iter(), &snapshot.machines).await);
    let plan = plan_deploy(intent, &snapshot)?;
    // TODO(UT-085): services absent from this finite project are intentionally not removed.
    Ok(DeployPreview {
        operations: plan.operations,
        warnings,
    })
}

pub(super) async fn execute_deploy(
    client: &mut Client,
    operations: &[DeployOperation],
    project_name: &ProjectName,
) -> DeployOutcome<ExecutionError> {
    execute_operations(operations, client, &CancellationToken::new(), project_name).await
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

fn choose_scale_spec(
    snapshot: &DeploySnapshot,
    selector: &ServiceSelector,
    replicas: NonZeroU32,
) -> Result<Option<RequestedServiceSpec>, Failure> {
    let services = ployz_core::derive_services(snapshot.containers.iter().cloned());
    let service = select_service(&services, selector)?;
    let observed_container = service
        .containers
        .first()
        .ok_or_else(|| Failure::usage("cannot scale a service without regular containers"))?
        .as_observation();
    match observed_container.resolved_spec.mode {
        ServiceMode::Replicated { .. } => {}
        ServiceMode::Global => return Err(Failure::usage("global services cannot be scaled")),
    }
    if usize::try_from(replicas.get()) == Ok(service.containers.len()) {
        return Ok(None);
    }
    // TODO(UT-046): mixed historical specs use one observed regular container; there is no chooser.
    let mut requested = observed_container.resolved_spec.to_requested();
    requested.mode = ServiceMode::Replicated { replicas };
    Ok(Some(requested))
}

pub(super) async fn list_machines(client: &mut Client) -> Result<Vec<MachineObservation>, Failure> {
    Ok(client.machines().await?)
}

async fn gather_snapshot(
    client: &mut Client,
    machines: Vec<MachineObservation>,
) -> Result<(DeploySnapshot, Vec<DeployWarning>), DeployError> {
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
) -> Vec<DeployWarning> {
    result
        .failures
        .iter()
        .map(|failure| DeployWarning::ObservationFailed {
            kind,
            machine_id: failure.machine_id,
            message: failure.error.message.clone(),
        })
        .chain(
            result
                .omissions
                .iter()
                .map(|machine| DeployWarning::ObservationOmitted {
                    kind,
                    machine_id: *machine,
                }),
        )
        .collect()
}

async fn expand_ingress<'a>(
    client: &mut Client,
    specs: impl IntoIterator<Item = &'a mut RequestedServiceSpec>,
) -> Result<(), DeployError> {
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

async fn hostname_warnings<'a>(
    specs: impl IntoIterator<Item = &'a RequestedServiceSpec>,
    machines: &[MachineObservation],
) -> Vec<DeployWarning> {
    resolve_ingress_dns_warnings(specs, &machine_public_addresses(machines))
        .await
        .into_iter()
        .map(DeployWarning::from)
        .collect()
}

fn machine_public_addresses(machines: &[MachineObservation]) -> Vec<IpAddr> {
    machines
        .iter()
        .filter_map(|machine| machine.machine.public_ip)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
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
