//! Placement-scenario helpers: multi-container row convergence, the typed
//! operator mesh client for volume-bearing deploy requests (the CLI carries
//! no volume flag), and machine-local volume assertions.

use std::net::Ipv4Addr;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::time::{Duration, Instant};

use bollard::Docker;
use hyper::Method;
use ployz::commands::SshTarget;
use ployz::mesh::context::{LoadedOperatorContext, OperatorContextStore};
use ployz::mesh::http::{JsonReply, MeshApiClient};
use ployz::mesh::{BuiltinWireguardDial, BuiltinWireguardPeer, MeshConnector, MeshDialTimeouts};
use ployz_core::corrosion::ServiceDocument;
use ployz_core::ids::{MachineRowId, OperationRowId};
use ployz_core::{
    DEPLOY_ROUTE, DeployAccepted, DeployRefusal, DeployRequest, LensCollection, LensSnapshot,
};
use ployz_e2e::dind::{DindMachine, exec_in_container, require};

use super::deploy_support::public_lens;

const API_PORT: u16 = 2_020;
const WAIT_BUDGET: Duration = Duration::from_secs(60);
const WAIT_DELAY: Duration = Duration::from_millis(250);
/// Suffix of the deterministic stable storage name for the declared volume
/// `data` (`ployz-n{len}-{namespace}-v4-data`).
pub(super) const DATA_VOLUME_SUFFIX: &str = "-v4-data";

/// The converged post-deploy truth for one service: its row document and one
/// `(machine, ip)` entry per container row owned by the given operation.
pub(super) struct PlacedRows {
    pub(super) service: ServiceDocument,
    pub(super) containers: Vec<(MachineRowId, Ipv4Addr)>,
}

/// Sends one typed deploy request over the operator's persisted WireGuard
/// mesh identity — the same authenticated public seam the CLI drives.
pub(super) async fn mesh_deploy(
    config_home: &Path,
    target: &SshTarget,
    request: &DeployRequest,
) -> Result<JsonReply<DeployAccepted, DeployRefusal>, String> {
    let store = OperatorContextStore::new(config_home);
    let loaded = store
        .load_target(target)
        .map_err(|error| error.to_string())?;
    let LoadedOperatorContext::BuiltinWireguard(context) = loaded else {
        return Err("operation-placement proof requires builtin WireGuard".to_owned());
    };
    let machine_address = context.machine_address;
    let dial = BuiltinWireguardDial::new(
        context.private_key.bytes(),
        context.source_address,
        BuiltinWireguardPeer {
            public_key: context.machine_public_key,
            endpoint: context.machine_endpoint,
            allowed_subnet: context.machine_allowed_subnet,
        },
        context.target.as_str().to_owned(),
    );
    let connector = MeshConnector::builtin_wireguard(dial, MeshDialTimeouts::default());
    let stream = connector
        .connect(SocketAddr::new(IpAddr::V6(machine_address), API_PORT))
        .await
        .map_err(|error| error.to_string())?;
    MeshApiClient::default()
        .request_json_with_refusal(stream, Method::POST, DEPLOY_ROUTE, Some(request))
        .await
        .map_err(|error| error.to_string())
}

/// Waits until the service row belongs to the given operation and exactly
/// `expected_containers` container rows exist for the service, all owned by
/// that operation (older revisions cleaned).
pub(super) async fn wait_for_placed_rows(
    docker: &Docker,
    requester: &DindMachine,
    api_address: &str,
    service_name: &str,
    operation_id: &OperationRowId,
    expected_containers: usize,
) -> Result<PlacedRows, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("public lenses were not queried");
    while Instant::now() < deadline {
        let services = public_lens(docker, requester, api_address, LensCollection::Services).await;
        let containers =
            public_lens(docker, requester, api_address, LensCollection::Containers).await;
        match (services, containers) {
            (
                Ok(LensSnapshot::Services { rows: service_rows }),
                Ok(LensSnapshot::Containers {
                    rows: container_rows,
                }),
            ) => {
                let service = service_rows.iter().find(|row| {
                    row.document.name.as_str() == service_name
                        && &row.document.operation_id == operation_id
                });
                if let Some(service) = service {
                    let placed = container_rows
                        .iter()
                        .filter(|row| row.document.service_id == service.id)
                        .collect::<Vec<_>>();
                    if placed.len() == expected_containers
                        && placed
                            .iter()
                            .all(|row| &row.document.deploy == operation_id)
                    {
                        return Ok(PlacedRows {
                            service: service.document.clone(),
                            containers: placed
                                .iter()
                                .map(|row| (row.document.machine_id.clone(), row.document.ip))
                                .collect(),
                        });
                    }
                    last = format!(
                        "service {service_name} had {} container row(s), wanted {expected_containers} owned by {operation_id}",
                        placed.len()
                    );
                } else {
                    last = format!(
                        "service row for {service_name} under operation {operation_id} had not converged"
                    );
                }
            }
            (Ok(services), Ok(containers)) => {
                last = format!(
                    "public API returned the wrong lenses: services={services:?} containers={containers:?}"
                );
            }
            (Err(error), _) | (_, Err(error)) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("placed rows did not converge: {last}"))
}

/// Whether the machine's inner Docker holds the deterministic `data` volume.
pub(super) async fn machine_holds_data_volume(
    docker: &Docker,
    machine: &DindMachine,
) -> Result<bool, String> {
    let outcome = exec_in_container(
        docker,
        &machine.container_id,
        &["docker", "volume", "ls", "--format", "{{.Name}}"],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(
        outcome.success(),
        format!("volume listing failed on {}: {outcome:?}", machine.name),
    )?;
    Ok(outcome
        .stdout
        .lines()
        .any(|line| line.trim().ends_with(DATA_VOLUME_SUFFIX)))
}

/// Bounded wait until an HTTP GET from inside `client` serves the expected
/// body — the host-published-port reachability check.
pub(super) async fn wait_for_http_body(
    docker: &Docker,
    client: &DindMachine,
    url: &str,
    expected_body: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("HTTP was not attempted");
    while Instant::now() < deadline {
        let outcome = exec_in_container(
            docker,
            &client.container_id,
            &[
                "curl",
                "--noproxy",
                "*",
                "--fail",
                "--silent",
                "--show-error",
                "--max-time",
                "5",
                url,
            ],
        )
        .await
        .map_err(|error| error.to_string())?;
        if outcome.success() && outcome.stdout.contains(expected_body) {
            return Ok(());
        }
        last = format!("{outcome:?}");
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!(
        "{url} did not serve {expected_body:?} from {}: {last}",
        client.name
    ))
}
