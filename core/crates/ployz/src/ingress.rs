//! Ingress Proxy identity and deployment boundaries.

use std::collections::BTreeSet;

use futures_util::future::join_all;
use ployz_core::{
    ContainerObservation, DeployOutcome, EnvironmentValues, FailedOperation, MachineObservation,
    PlacementConstraint, QualifiedService, RequestedServiceSpec, caddy_service_spec,
};
use ployz_store::{IngressAction, IngressFailure, IngressRepair};
use tokio_util::sync::CancellationToken;

use crate::{
    connect::Client,
    deploy::{DeployIntent, Outcome, SlotAction, SlotRepair},
    failure::Failure,
};

mod caddy;
pub use caddy::IngressImageError;

/// Build the Caddy ingress Service Spec.
///
/// # Errors
///
/// Returns when the Caddy image cannot be discovered.
pub async fn service_spec(
    image: Option<String>,
    constraints: BTreeSet<PlacementConstraint>,
) -> Result<RequestedServiceSpec, IngressImageError> {
    let image = match image {
        Some(image) => image,
        None => caddy::latest_image().await?,
    };
    Ok(caddy_service_spec(image, constraints))
}

/// The Caddy image a reconcile of the Ingress Proxy runs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum IngressImage {
    /// The image the Ingress Proxy runs now; the latest when it runs nowhere.
    Keep,
    /// The latest stable Caddy 2 image.
    Latest,
    /// An operator-pinned image.
    Given(String),
}

impl IngressImage {
    pub(crate) fn given_or(given: Option<String>, otherwise: Self) -> Self {
        given.map_or(otherwise, Self::Given)
    }
}

/// Deploy the Ingress Proxy so it runs on exactly the Servers that hold the ingress role,
/// keeping its observed placement constraints. `None`: no Ingress Proxy runs and no Server
/// holds the role, so nothing was deployed.
///
/// # Errors
///
/// Fails when observation, image discovery or the deploy fails.
pub(crate) async fn follow_roles(
    client: &mut Client,
    image: IngressImage,
) -> Result<Option<Outcome>, Failure> {
    let (_, Some(requested)) = wanted(client, image).await? else {
        return Ok(None);
    };
    Ok(Some(
        crate::deploy::apply_requested(client, &requested, false, false, "the Cluster").await?,
    ))
}

/// Bring back the Ingress Proxy on every Server with the ingress role where it is
/// missing or stopped, with the image it runs elsewhere. Never replaces, stops or
/// removes one; each Server's repair runs on its own, and a failure only reports.
///
/// # Errors
///
/// Fails when observation, image discovery or planning fails.
pub(crate) async fn fill_slots(
    client: &mut Client,
    cancel: &CancellationToken,
) -> Result<Vec<IngressRepair>, Failure> {
    let repairs = slot_repairs(client).await?;
    let client = &*client;
    Ok(
        join_all(repairs.into_iter().map(|(shown, repair)| async move {
            let outcome = client.confirm(&repair.plan, cancel, None).await;
            IngressRepair {
                failure: failure(&outcome),
                ..shown
            }
        }))
        .await,
    )
}

/// The repairs [`fill_slots`] would run, by Server; runs nothing.
///
/// # Errors
///
/// Fails when observation, image discovery or planning fails.
pub(crate) async fn planned_slots(client: &mut Client) -> Result<Vec<IngressRepair>, Failure> {
    Ok(slot_repairs(client)
        .await?
        .into_iter()
        .map(|(shown, _)| shown)
        .collect())
}

async fn slot_repairs(client: &mut Client) -> Result<Vec<(IngressRepair, SlotRepair)>, Failure> {
    let (machines, Some(requested)) = wanted(client, IngressImage::Keep).await? else {
        return Ok(Vec::new());
    };
    let intent = DeployIntent::apply_one(
        QualifiedService::system_ingress().namespace,
        requested,
        crate::deploy::plan_options(false, false),
    );
    let repairs = client.plan_slot_repairs(intent).await?;
    Ok(repairs
        .into_iter()
        .map(|repair| {
            let server = machines
                .iter()
                .find(|entry| entry.machine.id == repair.machine_id)
                .map_or_else(
                    || repair.machine_id.to_string(),
                    |entry| entry.machine.name.to_string(),
                );
            let shown = IngressRepair {
                machine: repair.machine_id,
                server,
                action: match repair.action {
                    SlotAction::Create => IngressAction::Create,
                    SlotAction::Start => IngressAction::Start,
                },
                failure: None,
            };
            (shown, repair)
        })
        .collect())
}

fn failure(outcome: &Outcome) -> Option<IngressFailure> {
    let DeployOutcome::Failed { failed, .. } = outcome else {
        return None;
    };
    let cause = match failed {
        FailedOperation::Operation { error, .. } | FailedOperation::Replacement { error, .. } => {
            error.to_string()
        }
    };
    Some(
        if cause.contains("address already in use") || cause.contains("port is already allocated") {
            IngressFailure::PortTaken { cause }
        } else {
            IngressFailure::Failed { cause }
        },
    )
}

/// The Cluster's Machines and the Ingress Proxy spec to run with `image`, keeping its
/// observed placement constraints. No spec when no Ingress Proxy runs and no Server holds
/// the role.
async fn wanted(
    client: &mut Client,
    image: IngressImage,
) -> Result<(Vec<MachineObservation>, Option<RequestedServiceSpec>), Failure> {
    let machines = client.machines().await?;
    let live = client
        .live_services_from(&machines, EnvironmentValues::Redacted)
        .await?;
    let observed = live
        .services()
        .into_iter()
        .find(|service| service.identity == QualifiedService::system_ingress())
        .and_then(|service| {
            service.observed_global_slot_spec().map(|spec| Running {
                image: spec.container.image.clone(),
                constraints: spec.placement.constraints.clone(),
            })
        });
    let any_role = machines.iter().any(|entry| entry.machine.accepts_ingress);
    let Some((image, constraints)) = desired(image, observed.as_ref(), any_role) else {
        return Ok((machines, None));
    };
    let spec = service_spec(image, constraints).await?;
    Ok((machines, Some(spec)))
}

/// What the observed Ingress Proxy runs.
struct Running {
    image: String,
    constraints: BTreeSet<PlacementConstraint>,
}

/// The image (`None`: discover the latest) and constraints the Ingress Proxy should run with.
fn desired(
    image: IngressImage,
    observed: Option<&Running>,
    any_role: bool,
) -> Option<(Option<String>, BTreeSet<PlacementConstraint>)> {
    if observed.is_none() && !any_role {
        return None;
    }
    let image = match image {
        IngressImage::Given(image) => Some(image),
        IngressImage::Keep => observed.map(|running| running.image.clone()),
        IngressImage::Latest => None,
    };
    let constraints = observed
        .map(|running| running.constraints.clone())
        .unwrap_or_default();
    Some((image, constraints))
}

/// True when this observation is the reserved Ingress Proxy Service.
#[must_use]
pub fn is_system_ingress(observation: &ContainerObservation) -> bool {
    observation.identity() == QualifiedService::system_ingress()
}

#[cfg(test)]
mod tests {
    use ployz_core::{PlacementConstraint, ServiceMode};

    use super::*;

    #[tokio::test]
    async fn builds_the_caddy_service_spec() {
        let constraints: BTreeSet<_> =
            [PlacementConstraint::parse("node.labels.edge==true").unwrap()].into();
        let caddy = service_spec(
            Some("registry.test/caddy@sha256:caddy".into()),
            constraints.clone(),
        )
        .await
        .unwrap();

        assert_eq!(caddy.name, QualifiedService::system_ingress().name);
        assert_eq!(caddy.mode, ServiceMode::Global);
        assert_eq!(caddy.placement.constraints, constraints);
        assert_eq!(
            caddy.container.command,
            ["caddy", "run", "-c", "/config/caddy/Caddyfile"]
        );
        assert_eq!(caddy.ports.len(), 3);
    }

    fn running(image: &str) -> Running {
        Running {
            image: image.into(),
            constraints: [PlacementConstraint::parse("node.labels.edge==true").unwrap()].into(),
        }
    }

    #[test]
    fn a_role_change_keeps_the_running_image_and_constraints() {
        let observed = running("caddy:2.9.1");
        let (image, constraints) = desired(IngressImage::Keep, Some(&observed), false).unwrap();
        assert_eq!(image.as_deref(), Some("caddy:2.9.1"));
        assert_eq!(constraints, observed.constraints);
    }

    #[test]
    fn the_first_ingress_role_deploys_the_latest_image_unconstrained() {
        assert_eq!(
            desired(IngressImage::Keep, None, true),
            Some((None, BTreeSet::new()))
        );
    }

    #[test]
    fn no_proxy_and_no_role_deploys_nothing() {
        assert_eq!(desired(IngressImage::Latest, None, false), None);
        assert_eq!(
            desired(IngressImage::Given("caddy:2".into()), None, false),
            None
        );
    }

    #[test]
    fn an_upgrade_moves_to_the_latest_or_pinned_image_keeping_constraints() {
        let observed = running("caddy:2.9.1");
        let (latest, constraints) = desired(IngressImage::Latest, Some(&observed), true).unwrap();
        assert_eq!(latest, None);
        assert_eq!(constraints, observed.constraints);
        let (pinned, _) = desired(
            IngressImage::Given("caddy:2.10.2".into()),
            Some(&observed),
            true,
        )
        .unwrap();
        assert_eq!(pinned.as_deref(), Some("caddy:2.10.2"));
    }
}
