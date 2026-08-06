//! Machine-local endpoint-network convergence driven by the accepted machines lens.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use ployz_core::LensSnapshot;
use ployz_core::corrosion::MachineTransport;
use ployz_core::ids::MachineRowId;
use ployz_core::network::MachineEndpointSubnet;
use tokio::sync::watch;

use crate::WireGuardMtuPolicy;
use crate::roles::api::execution::docker::runner::DockerManagedContainerRunner;
use crate::roles::api::runner::{
    MachineEndpointNetworkConvergenceError, MachineEndpointNetworkConvergenceOutcome,
};

use super::server::LensState;

const ENDPOINT_NETWORK_OVERALL_TIMEOUT: Duration = Duration::from_secs(30);
const ENDPOINT_NETWORK_MAX_ATTEMPTS: usize = 5;
const ENDPOINT_NETWORK_RETRY_INITIAL: Duration = Duration::from_millis(100);
const ENDPOINT_NETWORK_RETRY_MAX: Duration = Duration::from_secs(1);
const ENDPOINT_BRIDGE_IFNAME: &str = "br-ployz";
const WIREGUARD_IFNAME: &str = "ployz0";

pub(super) async fn run(
    updates: watch::Receiver<Option<LensState>>,
    local_machine_id: MachineRowId,
    runner: Arc<DockerManagedContainerRunner>,
    shutdown: watch::Receiver<bool>,
) -> Result<(), EndpointNetworkFoldError> {
    run_with(updates, local_machine_id, shutdown, move |subnet| {
        let runner = Arc::clone(&runner);
        async move { runner.converge_endpoint_network(&subnet).await }
    })
    .await
}

pub(super) fn runner_for_subnet(subnet: &MachineEndpointSubnet) -> DockerManagedContainerRunner {
    DockerManagedContainerRunner::lazy_local_defaults(
        subnet.as_string(),
        ENDPOINT_BRIDGE_IFNAME.to_owned(),
        WIREGUARD_IFNAME.to_owned(),
        WireGuardMtuPolicy::Auto,
    )
}

async fn run_with<Converge, ConvergeFuture>(
    mut updates: watch::Receiver<Option<LensState>>,
    local_machine_id: MachineRowId,
    mut shutdown: watch::Receiver<bool>,
    mut converge: Converge,
) -> Result<(), EndpointNetworkFoldError>
where
    Converge: FnMut(MachineEndpointSubnet) -> ConvergeFuture,
    ConvergeFuture: Future<
        Output = Result<
            MachineEndpointNetworkConvergenceOutcome,
            MachineEndpointNetworkConvergenceError,
        >,
    >,
{
    let mut applied = None;
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let current = { updates.borrow_and_update().clone() };
        if let Some(state) = current {
            let subnet = local_endpoint_subnet(&state, &local_machine_id)?;
            if applied.as_ref() != Some(&subnet) {
                let outcome = converge_with_retry(&subnet, &mut converge).await?;
                tracing::info!(%local_machine_id, subnet = %subnet.as_string(), ?outcome, "API fold converged the machine endpoint network");
                applied = Some(subnet);
            }
        }

        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            changed = updates.changed() => {
                if changed.is_err() {
                    return Err(EndpointNetworkFoldError::MachinesLensEnded);
                }
            }
        }
    }
}

async fn converge_with_retry<Converge, ConvergeFuture>(
    subnet: &MachineEndpointSubnet,
    converge: &mut Converge,
) -> Result<MachineEndpointNetworkConvergenceOutcome, EndpointNetworkFoldError>
where
    Converge: FnMut(MachineEndpointSubnet) -> ConvergeFuture,
    ConvergeFuture: Future<
        Output = Result<
            MachineEndpointNetworkConvergenceOutcome,
            MachineEndpointNetworkConvergenceError,
        >,
    >,
{
    let subnet = subnet.clone();
    tokio::time::timeout(ENDPOINT_NETWORK_OVERALL_TIMEOUT, async {
        let mut delay = ENDPOINT_NETWORK_RETRY_INITIAL;
        let mut last_error = None;
        for attempt in 1..=ENDPOINT_NETWORK_MAX_ATTEMPTS {
            match converge(subnet.clone()).await {
                Ok(outcome) => return Ok(outcome),
                Err(error) => {
                    tracing::warn!(subnet = %subnet.as_string(), attempt, error = %error, "API endpoint-network convergence will retry");
                    last_error = Some(error);
                }
            }
            if attempt < ENDPOINT_NETWORK_MAX_ATTEMPTS {
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2).min(ENDPOINT_NETWORK_RETRY_MAX);
            }
        }
        let Some(last_error) = last_error else {
            unreachable!("the fixed endpoint-network attempt count is nonzero")
        };
        Err(EndpointNetworkFoldError::ConvergenceExhausted {
            subnet: subnet.clone(),
            attempts: ENDPOINT_NETWORK_MAX_ATTEMPTS,
            detail: last_error.to_string(),
        })
    })
    .await
    .map_err(|_| EndpointNetworkFoldError::ConvergenceTimedOut {
        subnet: subnet.clone(),
        timeout: ENDPOINT_NETWORK_OVERALL_TIMEOUT,
    })?
}

fn local_endpoint_subnet(
    state: &LensState,
    local_machine_id: &MachineRowId,
) -> Result<MachineEndpointSubnet, EndpointNetworkFoldError> {
    let snapshot =
        state
            .as_ref()
            .map_err(|refusal| EndpointNetworkFoldError::MachinesLensRefused {
                detail: format!("{refusal:?}"),
            })?;
    let LensSnapshot::Machines { rows, .. } = snapshot else {
        return Err(EndpointNetworkFoldError::UnexpectedLensSnapshot);
    };
    let Some(local) = rows.iter().find(|row| &row.id == local_machine_id) else {
        return Err(EndpointNetworkFoldError::LocalMachineMissing {
            machine_id: local_machine_id.clone(),
        });
    };
    Ok(match &local.document.transport {
        MachineTransport::Wireguard { subnet_v4, .. }
        | MachineTransport::Tailscale { subnet_v4, .. } => subnet_v4.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum EndpointNetworkFoldError {
    #[error("the machines lens refused endpoint-network convergence: {detail}")]
    MachinesLensRefused { detail: String },
    #[error("endpoint-network convergence received a non-machines lens snapshot")]
    UnexpectedLensSnapshot,
    #[error("the accepted machines lens no longer contains local machine {machine_id}")]
    LocalMachineMissing { machine_id: MachineRowId },
    #[error("the machines lens ended during endpoint-network convergence")]
    MachinesLensEnded,
    #[error(
        "endpoint-network convergence for {subnet:?} failed after {attempts} attempts: {detail}"
    )]
    ConvergenceExhausted {
        subnet: MachineEndpointSubnet,
        attempts: usize,
        detail: String,
    },
    #[error("endpoint-network convergence for {subnet:?} exceeded {timeout:?}")]
    ConvergenceTimedOut {
        subnet: MachineEndpointSubnet,
        timeout: Duration,
    },
}

#[cfg(test)]
mod tests {
    use ployz_core::LensSnapshot;
    use serde_json::json;

    use super::*;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

    fn machine_id() -> MachineRowId {
        MachineRowId::try_new(MACHINE).expect("machine id")
    }

    fn machines_snapshot(subnet: &str) -> LensSnapshot {
        serde_json::from_value(json!({
            "collection": "machines",
            "cluster": {
                "v": 1,
                "cluster_id": CLUSTER,
                "written_by": { "kind": "peer", "peer_id": PEER },
                "written_at": "2026-08-05T10:00:00Z",
                "name": "test",
                "storage_default": "plain",
                "hostname_mode": { "mode": "disabled" },
                "prefix": "10.210.0.0/16",
                "provider": "tailscale",
                "acme_directory_url": "https://acme.test/directory",
                "acme_contact": null
            },
            "rows": [{
                "id": MACHINE,
                "document": {
                    "v": 1,
                    "cluster_id": CLUSTER,
                    "written_by": { "kind": "peer", "peer_id": PEER },
                    "written_at": "2026-08-05T10:00:00Z",
                    "name": "machine-one",
                    "lifecycle": "active",
                    "transport": { "kind": "tailscale", "ip": "100.64.0.1", "subnet_v4": subnet },
                    "storage": { "mode": "plain", "reason": { "kind": "default" } }
                }
            }]
        }))
        .expect("machines lens fixture")
    }

    #[tokio::test]
    async fn machines_lens_applies_initial_subnet_and_only_changed_subnets() {
        let (updates_tx, updates) = watch::channel(Some(Ok(machines_snapshot("10.210.1.0/24"))));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (applied_tx, mut applied) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run_with(updates, machine_id(), shutdown, move |subnet| {
            let applied_tx = applied_tx.clone();
            async move {
                applied_tx.send(subnet).expect("test observes effect");
                Ok(MachineEndpointNetworkConvergenceOutcome::Unchanged)
            }
        }));

        assert_eq!(
            applied.recv().await,
            Some(MachineEndpointSubnet::try_new("10.210.1.0/24").expect("subnet"))
        );
        updates_tx
            .send(Some(Ok(machines_snapshot("10.210.1.0/24"))))
            .expect("unchanged snapshot");
        tokio::task::yield_now().await;
        updates_tx
            .send(Some(Ok(machines_snapshot("10.210.2.0/24"))))
            .expect("changed snapshot");
        assert_eq!(
            applied.recv().await,
            Some(MachineEndpointSubnet::try_new("10.210.2.0/24").expect("subnet"))
        );
        shutdown_tx.send(true).expect("shutdown");
        task.await.expect("task joins").expect("fold succeeds");
        assert_eq!(
            applied.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn endpoint_network_failures_retry_to_a_typed_bound() {
        let subnet = MachineEndpointSubnet::try_new("10.210.1.0/24").expect("subnet");
        let mut attempts = 0;

        let error = converge_with_retry(&subnet, &mut |_| {
            attempts += 1;
            std::future::ready(Err(MachineEndpointNetworkConvergenceError::Inspect {
                message: "Docker unavailable".to_owned(),
            }))
        })
        .await
        .expect_err("retry budget is finite");

        assert_eq!(attempts, ENDPOINT_NETWORK_MAX_ATTEMPTS);
        assert_eq!(
            error,
            EndpointNetworkFoldError::ConvergenceExhausted {
                subnet,
                attempts: ENDPOINT_NETWORK_MAX_ATTEMPTS,
                detail: "could not inspect the local Docker endpoint network: Docker unavailable"
                    .to_owned(),
            }
        );
    }

    #[test]
    fn missing_local_machine_is_a_typed_fold_failure() {
        let mut snapshot = machines_snapshot("10.210.1.0/24");
        let LensSnapshot::Machines { rows, .. } = &mut snapshot else {
            unreachable!("fixture is a machines lens")
        };
        rows.clear();

        assert_eq!(
            local_endpoint_subnet(&Ok(snapshot), &machine_id()),
            Err(EndpointNetworkFoldError::LocalMachineMissing {
                machine_id: machine_id(),
            })
        );
    }
}
