use std::net::Ipv4Addr;
use std::process::{Child, Output};
use std::time::{Duration, Instant};

use bollard::Docker;
use ployz_core::corrosion::{ServiceDocument, Sha256Hex};
use ployz_core::ids::OperationRowId;
use ployz_core::{LensCollection, LensSnapshot};
use ployz_e2e::dind::{DindMachine, fetch_gateway_http, public_lens, require, require_success};

const WAIT_BUDGET: Duration = Duration::from_secs(60);
const WAIT_DELAY: Duration = Duration::from_millis(250);
const CLI_BUDGET: Duration = Duration::from_secs(180);

pub(super) struct PublicDeployRows {
    service: ServiceDocument,
    pub(super) container_ip: Ipv4Addr,
    service_container_deploys: Vec<OperationRowId>,
    encoded: String,
}

/// The two distinguishable HTTP bodies observed across a blue/green cutover.
pub(super) struct RevisionBodies<'a> {
    pub(super) first: &'a str,
    pub(super) second: &'a str,
}

/// The revision-gated cutover leaves its evidence in claim, commit, drain,
/// clean order; each marker is the `ops watch` rendering of one durable event.
pub(super) fn assert_replay_orders_cutover_evidence(replay: &str) -> Result<(), String> {
    let mut remainder = replay;
    for marker in [
        "operation claim won",
        "rows committed",
        "drained",
        "incumbent removed",
    ] {
        let Some(offset) = remainder.find(marker) else {
            return Err(format!(
                "cutover replay lacked {marker:?} in claim/commit/drain/clean order: {replay}"
            ));
        };
        remainder = &remainder[offset + marker.len()..];
    }
    Ok(())
}

pub(super) async fn wait_for_public_deploy_rows(
    docker: &Docker,
    requester: &DindMachine,
    api_address: &str,
    service_name: &str,
    operation_id: &OperationRowId,
) -> Result<PublicDeployRows, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("public lenses were not queried");
    while Instant::now() < deadline {
        let services = public_lens(docker, requester, api_address, LensCollection::Services).await;
        let containers =
            public_lens(docker, requester, api_address, LensCollection::Containers).await;
        let operations =
            public_lens(docker, requester, api_address, LensCollection::Operations).await;
        match (services, containers, operations) {
            (
                Ok(LensSnapshot::Services { rows: service_rows }),
                Ok(LensSnapshot::Containers {
                    rows: container_rows,
                }),
                Ok(LensSnapshot::Operations {
                    rows: operation_rows,
                }),
            ) => {
                let service = service_rows.iter().find(|row| {
                    row.document.name.as_str() == service_name
                        && &row.document.operation_id == operation_id
                });
                if let Some(service) = service {
                    let container = container_rows.iter().find(|row| {
                        row.document.service_id == service.id
                            && &row.document.deploy == operation_id
                    });
                    let operation = operation_rows.iter().find(|row| &row.id == operation_id);
                    if let (Some(container), Some(_operation)) = (container, operation) {
                        let service_container_deploys = container_rows
                            .iter()
                            .filter(|row| row.document.service_id == service.id)
                            .map(|row| row.document.deploy.clone())
                            .collect::<Vec<_>>();
                        let service = service.document.clone();
                        let container_ip = container.document.ip;
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
                            &container_rows,
                            &operation_rows,
                            &machines,
                            &machine_status,
                        ))
                        .map_err(|error| error.to_string())?;
                        return Ok(PublicDeployRows {
                            service,
                            container_ip,
                            service_container_deploys,
                            encoded,
                        });
                    }
                }
                last = "joined API lenses had not converged the service and container".to_owned();
            }
            (Ok(services), Ok(containers), Ok(operations)) => {
                last = format!(
                    "public API returned the wrong lenses: services={services:?} containers={containers:?} operations={operations:?}"
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
            "service row did not replace mutable image {requested_image} with a digest pin: {}",
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

/// The converged post-cutover truth: the service row activates the second
/// deploy under a fresh digest pin while naming the first pin as previous,
/// and exactly one container row survives, owned by the second operation.
pub(super) fn assert_cutover_rows(
    second: &PublicDeployRows,
    first: &PublicDeployRows,
    second_operation: &OperationRowId,
) -> Result<(), String> {
    require(
        second.service.active_deploy == *second_operation,
        format!(
            "service row did not activate the second deploy: {}",
            second.encoded
        ),
    )?;
    require(
        second.service.previous_image.as_ref() == Some(&first.service.image),
        format!(
            "service row did not name the first image as previous_image: {}",
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
            second.service_container_deploys.as_slice(),
            [deploy] if deploy == second_operation
        ),
        format!(
            "service did not converge to exactly one container row owned by the second deploy: {:?}",
            second.service_container_deploys
        ),
    )
}

/// Continuously samples the gateway while the active-deploy row flips. Every
/// request must complete successfully; the first body cannot return after the
/// flip, and the second cannot appear before it.
pub(super) struct GatewayCutover<'a> {
    pub api_address: &'a str,
    pub service_name: &'a str,
    pub first_operation: &'a OperationRowId,
    pub hostname: &'a str,
    pub bodies: RevisionBodies<'a>,
}

pub(super) async fn watch_gateway_cutover_traffic(
    docker: &Docker,
    gateway: &DindMachine,
    cutover: GatewayCutover<'_>,
    deploy: Child,
) -> Result<Output, String> {
    let deadline = Instant::now() + CLI_BUDGET;
    let mut deploy = Some(deploy);
    let mut deploy_output = None;
    let mut served_first = false;
    let mut served_second = false;
    while deploy_output.is_none() || !served_second {
        if Instant::now() >= deadline {
            if let Some(mut child) = deploy.take() {
                let _ = child.kill();
            }
            return Err(format!(
                "gateway cutover did not complete within {CLI_BUDGET:?}: deploy_finished={} served_first={served_first} served_second={served_second}",
                deploy_output.is_some(),
            ));
        }
        if let Some(mut child) = deploy.take() {
            if child
                .try_wait()
                .map_err(|error| format!("could not poll second deploy: {error}"))?
                .is_some()
            {
                let output = child
                    .wait_with_output()
                    .map_err(|error| format!("could not collect second deploy output: {error}"))?;
                require_success(&output, "second deploy")?;
                deploy_output = Some(output);
            } else {
                deploy = Some(child);
            }
        }

        let active_before =
            active_deploy(docker, gateway, cutover.api_address, cutover.service_name).await?;
        let body = fetch_gateway_http(docker, gateway, cutover.hostname)
            .await
            .map_err(|error| format!("gateway request failed during cutover: {error}"))?;
        let active_after =
            active_deploy(docker, gateway, cutover.api_address, cutover.service_name).await?;
        let definitely_before_flip =
            active_before == *cutover.first_operation && active_after == *cutover.first_operation;
        let definitely_after_flip =
            active_before != *cutover.first_operation && active_after != *cutover.first_operation;
        if body.contains(cutover.bodies.second) {
            require(
                !definitely_before_flip,
                format!("second revision served before active_deploy flipped: {body}"),
            )?;
            served_second = true;
        } else if body.contains(cutover.bodies.first) {
            require(
                !definitely_after_flip && !served_second,
                format!("first revision served after active_deploy flipped: {body}"),
            )?;
            served_first = true;
        } else {
            return Err(format!(
                "gateway cutover served an unexpected successful body: {body}"
            ));
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    require(
        served_first,
        "the first revision never served during the gateway cutover window",
    )?;
    deploy_output.ok_or_else(|| "second deploy output was not collected".to_owned())
}

async fn active_deploy(
    docker: &Docker,
    requester: &DindMachine,
    api_address: &str,
    service_name: &str,
) -> Result<OperationRowId, String> {
    let snapshot = public_lens(docker, requester, api_address, LensCollection::Services).await?;
    let LensSnapshot::Services { rows } = snapshot else {
        return Err("services route returned another lens collection".to_owned());
    };
    rows.into_iter()
        .find(|row| row.document.name.as_str() == service_name)
        .map(|row| row.document.active_deploy)
        .ok_or_else(|| format!("services lens omitted {service_name}"))
}
