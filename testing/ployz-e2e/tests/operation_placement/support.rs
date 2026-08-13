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
use ployz_core::corrosion::{CorrosionServiceName, PublishedService, service_key};
use ployz_core::ids::{CorrosionNamespaceName, DeployName, MachineName};
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

/// The converged post-deploy truth for one service: its published intent and
/// one `(machine, ip)` entry per observed endpoint owned by the given deploy.
pub(super) struct PlacedRows {
    pub(super) service: PublishedService,
    pub(super) endpoints: Vec<(MachineName, Ipv4Addr)>,
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

/// Waits until service intent belongs to the given deploy and exactly
/// `expected_endpoints` fresh endpoints exist for it (older revisions cleaned).
pub(super) async fn wait_for_placed_rows(
    docker: &Docker,
    requester: &DindMachine,
    api_address: &str,
    namespace_name: &str,
    service_name: &str,
    operation_id: &DeployName,
    expected_endpoints: usize,
) -> Result<PlacedRows, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("public lenses were not queried");
    while Instant::now() < deadline {
        let services = public_lens(docker, requester, api_address, LensCollection::Services).await;
        let endpoints =
            public_lens(docker, requester, api_address, LensCollection::Endpoints).await;
        match (services, endpoints) {
            (
                Ok(LensSnapshot::Services { rows: service_rows }),
                Ok(LensSnapshot::Endpoints {
                    rows: endpoint_rows,
                }),
            ) => {
                let namespace_name = CorrosionNamespaceName::try_new(namespace_name)
                    .map_err(|error| error.to_string())?;
                let service_name = CorrosionServiceName::try_new(service_name)
                    .map_err(|error| error.to_string())?;
                let service_key = service_key(&namespace_name, &service_name);
                let service = service_rows.iter().find(|row| {
                    row.key == service_key && &row.document.active_deploy == operation_id
                });
                if let Some(service) = service {
                    let placed = endpoint_rows
                        .iter()
                        .flat_map(|(machine_name, row)| {
                            row.endpoints
                                .iter()
                                .map(move |endpoint| (machine_name, endpoint))
                        })
                        .filter(|(_, endpoint)| {
                            endpoint.namespace_id == namespace_name
                                && endpoint.service_name == service_name
                        })
                        .collect::<Vec<_>>();
                    if placed.len() == expected_endpoints
                        && placed
                            .iter()
                            .all(|(_, endpoint)| &endpoint.deploy == operation_id)
                    {
                        return Ok(PlacedRows {
                            service: service.document.clone(),
                            endpoints: placed
                                .iter()
                                .map(|(machine_id, endpoint)| ((*machine_id).clone(), endpoint.ip))
                                .collect(),
                        });
                    }
                    last = format!(
                        "service {service_name} had {} endpoint(s), wanted {expected_endpoints} owned by {operation_id}",
                        placed.len()
                    );
                } else {
                    last = format!(
                        "service intent for {service_name} under deploy {operation_id} had not converged"
                    );
                }
            }
            (Ok(services), Ok(endpoints)) => {
                last = format!(
                    "public API returned the wrong lenses: services={services:?} endpoints={endpoints:?}"
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
