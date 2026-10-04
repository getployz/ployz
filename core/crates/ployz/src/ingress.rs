//! Ingress Proxy identity and deployment boundaries.

use std::{collections::BTreeSet, time::Duration};

use ployz_core::{
    ContainerId, ContainerObservation, EnvironmentValues, GetIngressProxyConfigRequest, MachineId,
    MachineTarget, MarkContainerStoppingRequest, PlacementConstraint, PortPublication,
    QualifiedService, RequestedServiceSpec, caddy_service_spec, op,
};

use crate::{
    connect::{Client, TARGET_RPC_TIMEOUT},
    deploy::Outcome,
    failure::Failure,
};

const WITHDRAW_POLL: Duration = Duration::from_millis(250);
const WITHDRAW_CAP: Duration = Duration::from_secs(10);

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
        return Ok(None);
    };
    let requested = service_spec(image, constraints).await?;
    Ok(Some(
        crate::deploy::apply_requested(client, &requested, false, false, "the Cluster").await?,
    ))
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

/// Take one Container out of every Ingress Proxy before it is stopped: mark it stopping on
/// its Server, then wait until no reachable ingress-role Server's loaded config routes to it. A proxy
/// that cannot be read counts as still routing. After [`WITHDRAW_CAP`] the stop goes ahead
/// with a warning naming the unconfirmed Servers. Marking failures (an older daemon, a
/// Container already gone) skip the wait; the stop reports its own error.
pub(crate) async fn withdraw(client: &Client, machine_id: &MachineId, container_id: &ContainerId) {
    let Ok(details) = client
        .invoke::<op::MarkContainerStopping>(
            MarkContainerStoppingRequest {
                container_id: *container_id,
            },
            &MachineTarget::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
    else {
        return;
    };
    let upstreams = routed_upstreams(&details.container);
    if upstreams.is_empty() {
        return;
    }
    let Ok(machines) = client.clone().machines().await else {
        return;
    };
    let mut pending = machines
        .into_iter()
        // A Server known down gets no config either; waiting on it only costs the cap.
        .filter(|entry| entry.machine.accepts_ingress && entry.membership.invites_rpc())
        .map(|entry| entry.machine)
        .collect::<Vec<_>>();
    let confirmed = tokio::time::timeout(WITHDRAW_CAP, async {
        loop {
            let routes = futures_util::future::join_all(pending.iter().map(|machine| async {
                client
                    .invoke::<op::GetIngressProxyConfig>(
                        GetIngressProxyConfigRequest {},
                        &MachineTarget::from(&machine.id),
                        Some(WITHDRAW_CAP),
                    )
                    .await
                    .map_or(true, |proxy| {
                        proxy
                            .config()
                            .split_whitespace()
                            .any(|token| upstreams.iter().any(|upstream| upstream == token))
                    })
            }))
            .await;
            let mut routes = routes.into_iter();
            pending.retain(|_| routes.next().unwrap_or(true));
            if pending.is_empty() {
                return;
            }
            tokio::time::sleep(WITHDRAW_POLL).await;
        }
    })
    .await;
    if confirmed.is_err() {
        let names = pending
            .iter()
            .map(|machine| machine.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        crate::output::warn(format!(
            "stopping Container {container_id} before Ingress Proxies on {names} confirmed they stopped routing to it"
        ));
    }
}

/// The `ip:port` upstreams an Ingress Proxy would route to this Container.
fn routed_upstreams(container: &ContainerObservation) -> Vec<String> {
    let Some(address) = container.address else {
        return Vec::new();
    };
    container
        .resolved_spec
        .ports
        .iter()
        .filter_map(|port| match port {
            PortPublication::Ingress { container_port, .. } => {
                Some(format!("{}:{container_port}", address.0))
            }
            PortPublication::Host { .. } => None,
        })
        .collect()
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
