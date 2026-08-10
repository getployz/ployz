use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use bollard::Docker;
use ployz_core::corrosion::CorrosionServiceName;
use ployz_core::corrosion::{PublishedService, Sha256Hex, service_key};
use ployz_core::ids::{CorrosionNamespaceName, DeployName};
use ployz_core::{LensCollection, LensSnapshot};
use ployz_e2e::dind::{DindMachine, public_lens, require};

const WAIT_BUDGET: Duration = Duration::from_secs(60);
const WAIT_DELAY: Duration = Duration::from_millis(250);

pub(super) struct PublicDeployRows {
    service: PublishedService,
    pub(super) endpoint_ip: Ipv4Addr,
    service_endpoint_deploys: Vec<DeployName>,
    encoded: String,
}

pub(super) async fn wait_for_public_deploy_rows(
    docker: &Docker,
    requester: &DindMachine,
    api_address: &str,
    namespace_name: &str,
    service_name: &str,
    operation_id: &DeployName,
) -> Result<PublicDeployRows, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("public lenses were not queried");
    while Instant::now() < deadline {
        let services = public_lens(docker, requester, api_address, LensCollection::Services).await;
        let endpoints =
            public_lens(docker, requester, api_address, LensCollection::Endpoints).await;
        let operations =
            public_lens(docker, requester, api_address, LensCollection::Operations).await;
        match (services, endpoints, operations) {
            (
                Ok(LensSnapshot::Services { rows: service_rows }),
                Ok(LensSnapshot::Endpoints {
                    rows: endpoint_rows,
                }),
                Ok(LensSnapshot::Operations {
                    rows: operation_rows,
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
                    let endpoint = endpoint_rows.iter().find_map(|row| {
                        row.endpoints.iter().find(|endpoint| {
                            endpoint.namespace_id == namespace_name
                                && endpoint.service_name == service_name
                                && &endpoint.deploy == operation_id
                        })
                    });
                    let operation = operation_rows.iter().find(|row| {
                        row.namespace_id == namespace_name && &row.deploy_name == operation_id
                    });
                    if let (Some(endpoint), Some(_operation)) = (endpoint, operation) {
                        let service_endpoint_deploys = endpoint_rows
                            .iter()
                            .flat_map(|row| &row.endpoints)
                            .filter(|endpoint| {
                                endpoint.namespace_id == namespace_name
                                    && endpoint.service_name == service_name
                            })
                            .map(|endpoint| endpoint.deploy.clone())
                            .collect::<Vec<_>>();
                        let service = service.document.clone();
                        let endpoint_ip = endpoint.ip;
                        let machines =
                            public_lens(docker, requester, api_address, LensCollection::Machines)
                                .await?;
                        let machine_status = public_lens(
                            docker,
                            requester,
                            api_address,
                            LensCollection::MachineStatus,
                        )
                        .await?;
                        let encoded = serde_json::to_string(&(
                            &service_rows,
                            &endpoint_rows,
                            &operation_rows,
                            &machines,
                            &machine_status,
                        ))
                        .map_err(|error| error.to_string())?;
                        return Ok(PublicDeployRows {
                            service,
                            endpoint_ip,
                            service_endpoint_deploys,
                            encoded,
                        });
                    }
                }
                last = "joined API lenses had not converged service intent and endpoint reality"
                    .to_owned();
            }
            (Ok(services), Ok(endpoints), Ok(operations)) => {
                last = format!(
                    "public API returned the wrong lenses: services={services:?} endpoints={endpoints:?} operations={operations:?}"
                )
            }
            (Err(error), _, _) => last = error,
            (_, Err(error), _) => last = error,
            (_, _, Err(error)) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("public deploy rows did not converge: {last}"))
}

pub(super) fn assert_public_rows_are_digest_pinned_and_secret_free(
    rows: &PublicDeployRows,
    requested_image: &str,
    secret_name: &str,
    secret_value: &str,
    expected_fingerprint: &Sha256Hex,
) -> Result<(), String> {
    require(
        rows.service.image.as_str() != requested_image
            && rows.service.image.as_str().contains("@sha256:"),
        format!(
            "namespace intent did not replace mutable image {requested_image} with a digest pin: {}",
            rows.service.image.as_str()
        ),
    )?;
    require(
        !rows.encoded.contains(secret_value)
            && rows.service.env_fingerprints.len() == 1
            && rows.service.env_fingerprints.get(secret_name) == Some(expected_fingerprint),
        format!(
            "public rows retained an environment value or the wrong fingerprint: {}",
            rows.encoded
        ),
    )
}

/// The converged post-cutover truth: namespace intent activates the second
/// deploy under a fresh digest pin while naming the first pin as previous,
/// and exactly one serving endpoint remains for the second deploy.
pub(super) fn assert_cutover_rows(
    second: &PublicDeployRows,
    first: &PublicDeployRows,
    second_operation: &DeployName,
) -> Result<(), String> {
    require(
        second.service.active_deploy == *second_operation,
        format!(
            "namespace intent did not activate the second deploy: {}",
            second.encoded
        ),
    )?;
    require(
        second.service.previous_image.as_ref() == Some(&first.service.image),
        format!(
            "namespace intent did not name the first image as previous_image: {}",
            second.encoded
        ),
    )?;
    require(
        second.service.image != first.service.image,
        format!(
            "second deploy did not re-resolve the mutable tag to a new digest: {}",
            second.encoded
        ),
    )?;
    require(
        matches!(
            second.service_endpoint_deploys.as_slice(),
            [deploy] if deploy == second_operation
        ),
        format!(
            "service did not converge to exactly one endpoint owned by the second deploy: {:?}",
            second.service_endpoint_deploys
        ),
    )
}
