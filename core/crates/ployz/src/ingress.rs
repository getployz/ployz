//! Ingress Proxy identity and deployment boundaries.

use std::{collections::BTreeSet, future::Future};

use futures_util::future::join_all;
use ployz_core::{
    ContainerId, ContainerLocator, ContainerNow, ContainerObservation, ContainerWant,
    DeployOutcome, EnvironmentValues, FailedOperation, HealthFailure, MachineObservation,
    PlacementConstraint, QualifiedService, RequestedServiceSpec, RpcError, RunTarget,
    caddy_service_spec,
};
use ployz_store::{IngressAction, IngressFailure, IngressRepair};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::{
    cluster::HealthWaitError,
    connect::{Client, TARGET_RPC_TIMEOUT},
    deploy::{DeployIntent, DeployPlan, Outcome, SlotAction},
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

/// What an ingress start did, or would do, on each Server that holds the ingress role.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct IngressCheck {
    pub repairs: Vec<IngressRepair>,
    /// One cause per ingress Server whose containers went unobserved, naming it.
    pub unchecked: Vec<String>,
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
) -> Result<IngressCheck, Failure> {
    let Slots { runs, unchecked } = slot_repairs(client).await?;
    if cancel.is_cancelled() {
        return Ok(IngressCheck::default());
    }
    let client = &*client;
    let settled = join_all(runs.into_iter().map(|(shown, work)| async move {
        let failure = match work {
            None => return Ok(shown),
            Some(Work::Create(plan)) => failure(&client.confirm(&plan, cancel, None).await),
            Some(Work::Start(container_id)) => {
                match started(
                    client.ensure_container(
                        &shown.machine,
                        ContainerWant::Running {
                            container: RunTarget::Existing(ContainerLocator::Id(container_id)),
                        },
                        Some(TARGET_RPC_TIMEOUT),
                    ),
                    client.wait_until_ready(&shown.machine, &container_id, cancel),
                    cancel,
                )
                .await
                {
                    Started::Settled(failure) => failure,
                    Started::Cancelled => {
                        return Err(format!(
                            "{} was still starting its Ingress Proxy when the Deploy was cancelled",
                            shown.server
                        ));
                    }
                }
            }
        };
        Ok(IngressRepair { failure, ..shown })
    }))
    .await;
    let mut check = IngressCheck {
        repairs: Vec::new(),
        unchecked,
    };
    for slot in settled {
        match slot {
            Ok(repair) => check.repairs.push(repair),
            Err(cause) => check.unchecked.push(cause),
        }
    }
    Ok(check)
}

/// The repairs [`fill_slots`] would run, by Server; runs nothing.
///
/// # Errors
///
/// Fails when observation, image discovery or planning fails.
pub(crate) async fn planned_slots(client: &mut Client) -> Result<IngressCheck, Failure> {
    let Slots { runs, unchecked } = slot_repairs(client).await?;
    Ok(IngressCheck {
        repairs: runs.into_iter().map(|(shown, _)| shown).collect(),
        unchecked,
    })
}

enum Work {
    Create(DeployPlan),
    Start(ContainerId),
}

/// Each Server's repair with its work, or `None` with the failure that kept it from
/// planning; and the Servers left unchecked.
#[derive(Default)]
struct Slots {
    runs: Vec<(IngressRepair, Option<Work>)>,
    unchecked: Vec<String>,
}

async fn slot_repairs(client: &mut Client) -> Result<Slots, Failure> {
    let (machines, Some(requested)) = wanted(client, IngressImage::Keep).await? else {
        return Ok(Slots::default());
    };
    let intent = DeployIntent::apply_one(
        QualifiedService::system_ingress().namespace,
        requested,
        crate::deploy::plan_options(false, false),
    );
    let mut slots = Slots::default();
    for repair in client.plan_ingress_repairs(intent).await? {
        let server = machines
            .iter()
            .find(|entry| entry.machine.id == repair.machine_id)
            .map_or_else(
                || repair.machine_id.to_string(),
                |entry| entry.machine.name.to_string(),
            );
        let shown = |action, failure| IngressRepair {
            machine: repair.machine_id,
            server: server.clone(),
            action,
            failure,
        };
        let run = match repair.action {
            SlotAction::Unchecked(cause) => {
                slots
                    .unchecked
                    .push(format!("{server} went unobserved ({cause})"));
                continue;
            }
            SlotAction::Start(container_id) => (
                shown(IngressAction::Start, None),
                Some(Work::Start(container_id)),
            ),
            SlotAction::Dead(container_id) => (
                shown(
                    IngressAction::Start,
                    Some(IngressFailure::Failed {
                        cause: format!(
                            "its Ingress Proxy container is dead, which Docker won't start; \
                             remove it there with `docker rm -f {container_id}`"
                        ),
                    }),
                ),
                None,
            ),
            SlotAction::Create(Ok(plan)) => {
                (shown(IngressAction::Create, None), Some(Work::Create(plan)))
            }
            SlotAction::Create(Err(error)) => (
                shown(
                    IngressAction::Create,
                    Some(IngressFailure::Failed {
                        cause: error.to_string(),
                    }),
                ),
                None,
            ),
        };
        slots.runs.push(run);
    }
    Ok(slots)
}

/// How starting a stopped Ingress Proxy ended.
#[derive(Debug, PartialEq)]
enum Started {
    /// The failure of the start itself, or of the container staying up through the
    /// readiness window `ployz service start` waits; none once it stayed up.
    Settled(Option<IngressFailure>),
    /// The Deploy was cancelled before it knew whether the container stayed up.
    Cancelled,
}

async fn started(
    ensured: impl Future<Output = Result<ContainerNow, RpcError>>,
    ready: impl Future<Output = Result<(), HealthWaitError<RpcError>>>,
    cancel: &CancellationToken,
) -> Started {
    let ensured = tokio::select! {
        ensured = ensured => ensured,
        () = cancel.cancelled() => return Started::Cancelled,
    };
    if let Err(error) = ensured {
        return Started::Settled(Some(classify(crate::ui::row(&error))));
    }
    Started::Settled(match ready.await {
        Ok(()) => None,
        Err(HealthWaitError::Health(HealthFailure::Cancelled)) => return Started::Cancelled,
        Err(HealthWaitError::Health(failure)) => Some(IngressFailure::Failed {
            cause: format!("it stopped right after starting: {failure}"),
        }),
        Err(HealthWaitError::Inspect(error)) => Some(classify(format!(
            "it started, but checking that it stayed up failed: {}",
            crate::ui::row(&error)
        ))),
    })
}

fn failure(outcome: &Outcome) -> Option<IngressFailure> {
    let DeployOutcome::Failed { failed, .. } = outcome else {
        return None;
    };
    Some(classify(match failed {
        FailedOperation::Operation { error, .. } | FailedOperation::Replacement { error, .. } => {
            error.to_string()
        }
    }))
}

fn classify(cause: String) -> IngressFailure {
    if cause.contains("address already in use") || cause.contains("port is already allocated") {
        IngressFailure::PortTaken { cause }
    } else {
        IngressFailure::Failed { cause }
    }
}

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
    use std::future::ready;

    use ployz_core::{ContainerRuntimeObservation, PlacementConstraint, RpcErrorCode, ServiceMode};

    use super::*;

    fn absent() -> ContainerNow {
        ContainerNow::Absent {
            of: ContainerLocator::Id(ContainerId::parse("1".repeat(64)).unwrap()),
        }
    }

    #[tokio::test]
    async fn a_start_that_stays_up_is_repaired() {
        assert_eq!(
            started(
                ready(Ok(absent())),
                ready(Ok(())),
                &CancellationToken::new()
            )
            .await,
            Started::Settled(None)
        );
    }

    #[tokio::test]
    async fn a_cancelled_deploy_stops_waiting_on_a_start() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            started(std::future::pending(), ready(Ok(())), &cancel).await,
            Started::Cancelled
        );
    }

    #[tokio::test]
    async fn a_deploy_cancelled_while_a_start_proves_it_stays_up_leaves_it_unchecked() {
        let cancel = CancellationToken::new();
        let cancelled_wait = async {
            cancel.cancel();
            Err(HealthWaitError::Health(HealthFailure::Cancelled))
        };
        assert_eq!(
            started(ready(Ok(absent())), cancelled_wait, &cancel).await,
            Started::Cancelled
        );
    }

    #[tokio::test]
    async fn a_start_refused_for_a_held_port_says_the_port_is_taken() {
        let refused = RpcError {
            code: RpcErrorCode::Internal,
            message: "Docker operation failed".into(),
            details: serde_json::Value::Null,
            cause: vec![
                "Docker responded with status code 500: driver failed programming external \
                 connectivity on endpoint ployz-system-caddy: Error starting userland proxy: \
                 listen tcp4 0.0.0.0:80: bind: address already in use"
                    .into(),
            ],
        };
        let Started::Settled(Some(IngressFailure::PortTaken { cause })) = started(
            ready(Err(refused)),
            ready(Ok(())),
            &CancellationToken::new(),
        )
        .await
        else {
            panic!("a held port is a taken port");
        };
        assert!(
            cause.contains("0.0.0.0:80: bind: address already in use"),
            "{cause}"
        );
    }

    #[tokio::test]
    async fn a_start_that_exits_right_away_fails_with_the_exit() {
        let exited = HealthWaitError::Health(HealthFailure::Runtime {
            observation: ContainerRuntimeObservation::Exited { code: 1 },
            last_check: None,
        });
        let Started::Settled(Some(IngressFailure::Failed { cause })) = started(
            ready(Ok(absent())),
            ready(Err(exited)),
            &CancellationToken::new(),
        )
        .await
        else {
            panic!("an exited start is a failed repair");
        };
        assert!(cause.contains("exited with code 1"), "{cause}");
    }

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
