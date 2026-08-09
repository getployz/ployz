use std::time::{Duration, Instant};

use bollard::Docker;
use ployz_core::corrosion::{
    GatewayObservationDocument, GatewayRouteAvailability, GatewayRouteProjectionOutcome,
    RouteBindingDocument, SqliteValue,
};
use ployz_core::ids::{MachineRowId, RouteBindingRowId};
use ployz_core::machine::GatewayServingStatus;
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, DindMachine, MachineSpec, artifact_dir, connect_docker,
    corrosion_access, corrosion_query, create_namespace_and_deploy, e2e_enabled, found_and_join,
    keep_requested, machine_image, require, run_cli, start_mutable_registry,
    wait_for_gateway_status,
};
use ployz_e2e::dind::{OperatorFixture, assert_gateway_http};

const NAMESPACE: &str = "production";
const SERVICE: &str = "web";
const SECRET_NAME: &str = "GATEWAY_E2E_SECRET";
const SECRET_VALUE: &str = "gateway-e2e-sentinel";
const BODY: &str = "Welcome to nginx";
const DISABLED_HOSTNAME: &str = "explicit.example.test";
const WAIT_BUDGET: Duration = Duration::from_secs(60);
const WAIT_DELAY: Duration = Duration::from_millis(250);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabled_urls_allow_manual_gateway_routes() {
    if !e2e_enabled() {
        eprintln!("skipping gateway-route DinD proof; set PLOYZ_DIND_E2E=1 to enable it");
        return;
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    panic!("the pinned gateway-route proof supports only Linux x86_64");

    let docker = connect_docker().expect("connect to Docker for gateway-route proof");
    let cluster = DindCluster::provision(
        &docker,
        DindClusterSpec {
            artifact_dir: artifact_dir(),
            machines: vec![MachineSpec {
                image: machine_image(),
            }],
        },
    )
    .await
    .expect("provision gateway machine");
    let [machine] = cluster.machines() else {
        panic!("gateway proof requires exactly one machine");
    };
    let result = exercise_disabled(&docker, machine).await;
    if let Err(error) = &result {
        match cluster.capture_evidence().await {
            Ok(path) => eprintln!("gateway evidence captured under {}", path.display()),
            Err(capture_error) => {
                eprintln!("gateway evidence capture failed: {capture_error}")
            }
        }
        eprintln!("gateway proof failed: {error}");
    }
    if keep_requested() {
        eprintln!(
            "retaining DinD run {} because PLOYZ_DIND_KEEP=1",
            cluster.run_id()
        );
    } else {
        cluster
            .teardown()
            .await
            .unwrap_or_else(|error| panic!("tear down gateway run: {error}"));
    }
    result.unwrap_or_else(|error| panic!("gateway proof failed: {error}"));
}

async fn exercise_disabled(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let operator = found_and_join(docker, machine, &[]).await?;
    let image = start_mutable_registry(docker, machine, &[]).await?;
    create_namespace_and_deploy(
        &operator,
        NAMESPACE,
        SERVICE,
        &image,
        SECRET_NAME,
        SECRET_VALUE,
    )?;
    require(
        route_rows(docker, machine).await?.is_empty(),
        "disabled service URLs created an automatic route",
    )?;

    let first = attach_route(&operator, DISABLED_HOSTNAME)?;
    assert_gateway_http(docker, machine, DISABLED_HOSTNAME, BODY).await?;
    wait_for_applied_observation(
        docker,
        machine,
        &operator.founder_machine_id,
        &first,
        DISABLED_HOSTNAME,
    )
    .await?;
    let removed = run_cli(
        &operator,
        &[
            "route",
            "rm",
            DISABLED_HOSTNAME,
            "--id",
            first.as_str(),
            "--target",
            operator.founder_target.as_str(),
        ],
    )?;
    require(
        removed.status.success(),
        format!("route rm failed: {removed:?}"),
    )?;
    wait_for_gateway_status(docker, machine, DISABLED_HOSTNAME, 404).await?;

    let second = attach_route(&operator, DISABLED_HOSTNAME)?;
    require(
        first != second,
        format!("reattach reused removed route identity {first}"),
    )?;
    assert_gateway_http(docker, machine, DISABLED_HOSTNAME, BODY).await
}

fn attach_route(operator: &OperatorFixture, hostname: &str) -> Result<RouteBindingRowId, String> {
    let output = run_cli(
        operator,
        &[
            "route",
            "attach",
            hostname,
            "--namespace",
            NAMESPACE,
            "--service",
            SERVICE,
            "--port",
            "80",
            "--target",
            operator.founder_target.as_str(),
        ],
    )?;
    require(
        output.status.success(),
        format!("route attach failed: {output:?}"),
    )?;
    let stdout = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let id = stdout
        .split_once('(')
        .and_then(|(_, suffix)| suffix.split_once(')').map(|(id, _)| id))
        .ok_or_else(|| format!("route attach omitted its row id: {stdout}"))?;
    RouteBindingRowId::try_new(id.to_owned()).map_err(|error| error.to_string())
}

async fn route_rows(
    docker: &Docker,
    machine: &DindMachine,
) -> Result<Vec<(RouteBindingRowId, RouteBindingDocument)>, String> {
    let (address, token) = corrosion_access(docker, machine).await?;
    let rows = corrosion_query(
        docker,
        machine,
        &address,
        &token,
        "SELECT id, document FROM route_bindings ORDER BY id",
    )
    .await?;
    rows.into_iter()
        .map(|row| {
            let [SqliteValue::Text(id), SqliteValue::Text(document)] = row.as_slice() else {
                return Err(format!("route query returned an invalid row: {row:?}"));
            };
            let id = RouteBindingRowId::try_new(id.clone()).map_err(|error| error.to_string())?;
            let document = serde_json::from_str(document)
                .map_err(|error| format!("route row {id} was invalid: {error}"))?;
            Ok((id, document))
        })
        .collect()
}

async fn wait_for_applied_observation(
    docker: &Docker,
    machine: &DindMachine,
    machine_id: &MachineRowId,
    route_id: &RouteBindingRowId,
    hostname: &str,
) -> Result<(), String> {
    let (address, token) = corrosion_access(docker, machine).await?;
    let query = format!(
        "SELECT document FROM gateway_observations WHERE machine_id = '{}'",
        machine_id.as_str()
    );
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("gateway observation was not queried");
    while Instant::now() < deadline {
        match corrosion_query(docker, machine, &address, &token, &query).await {
            Ok(rows) => match rows.as_slice() {
                [row] => {
                    let [SqliteValue::Text(document)] = row.as_slice() else {
                        last = format!("gateway observation query returned invalid row: {row:?}");
                        tokio::time::sleep(WAIT_DELAY).await;
                        continue;
                    };
                    let observation: GatewayObservationDocument = serde_json::from_str(document)
                        .map_err(|error| format!("gateway observation was invalid: {error}"))?;
                    let applied = observation.routes.iter().any(|route| {
                        route.route_binding_id == *route_id
                            && route.hostname.as_str() == hostname
                            && matches!(
                                route.outcome,
                                GatewayRouteProjectionOutcome::Applied {
                                    availability: GatewayRouteAvailability::Serving {
                                        upstream_count: 1..
                                    }
                                }
                            )
                    });
                    if observation.machine_id == *machine_id
                        && observation.serving == GatewayServingStatus::Current
                        && applied
                    {
                        return Ok(());
                    }
                    last =
                        format!("gateway observation had not applied the route: {observation:?}");
                }
                [] => last = "gateway observation row was absent".to_owned(),
                rows => last = format!("gateway observation query returned invalid rows: {rows:?}"),
            },
            Err(error) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!(
        "gateway did not publish a current applied observation for {hostname}: {last}"
    ))
}
