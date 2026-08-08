#[path = "operation_deploy/support.rs"]
#[allow(dead_code)]
mod support;

use std::time::{Duration, Instant};

use bollard::Docker;
use ployz_core::corrosion::{IngressMode, RouteBindingDocument, SqliteValue};
use ployz_core::ids::RouteBindingRowId;
use ployz_core::ingress::RouteBindingOrigin;
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, DindMachine, MachineSpec, artifact_dir, connect_docker,
    corrosion_access, corrosion_query, e2e_enabled, exec_ok, keep_requested, machine_image,
    require,
};

const NAMESPACE: &str = "production";
const SERVICE: &str = "web";
const SECRET_NAME: &str = "GATEWAY_E2E_SECRET";
const SECRET_VALUE: &str = "gateway-e2e-sentinel";
const BODY: &str = "Welcome to nginx";
const DISABLED_HOSTNAME: &str = "explicit.example.test";
const MANAGED_HOSTNAME: &str = "production.brisk-river-x7f3.up.ployz.app";
const LEASE_STUB_ORIGIN: &str = "http://127.0.0.1:18080";
const WAIT_BUDGET: Duration = Duration::from_secs(60);
const WAIT_DELAY: Duration = Duration::from_millis(250);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabled_and_managed_hostname_modes_drive_gateway_routes() {
    if !e2e_enabled() {
        eprintln!("skipping gateway-route DinD proof; set PLOYZ_DIND_E2E=1 to enable it");
        return;
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    panic!("the pinned gateway-route proof supports only Linux x86_64");

    let docker = connect_docker().expect("connect to Docker for gateway-route proof");
    exercise_one_mode(&docker, GatewayMode::Disabled).await;
    exercise_one_mode(&docker, GatewayMode::Managed).await;
}

#[derive(Clone, Copy)]
enum GatewayMode {
    Disabled,
    Managed,
}

impl GatewayMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Managed => "managed",
        }
    }
}

async fn exercise_one_mode(docker: &Docker, mode: GatewayMode) {
    let mode_name = mode.as_str();
    let cluster = DindCluster::provision(
        docker,
        DindClusterSpec {
            artifact_dir: artifact_dir(),
            machines: vec![MachineSpec {
                image: machine_image(),
            }],
        },
    )
    .await
    .unwrap_or_else(|error| panic!("provision {mode_name} gateway machine: {error}"));
    let [machine] = cluster.machines() else {
        panic!("{mode_name} gateway proof requires exactly one machine");
    };
    let result = match mode {
        GatewayMode::Disabled => exercise_disabled(docker, machine).await,
        GatewayMode::Managed => exercise_managed(docker, machine).await,
    };
    if let Err(error) = &result {
        match cluster.capture_evidence().await {
            Ok(path) => eprintln!(
                "{mode_name} gateway evidence captured under {}",
                path.display()
            ),
            Err(capture_error) => {
                eprintln!("{mode_name} gateway evidence capture failed: {capture_error}")
            }
        }
        eprintln!("{mode_name} gateway proof failed: {error}");
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
            .unwrap_or_else(|error| panic!("tear down {mode_name} gateway run: {error}"));
    }
    result.unwrap_or_else(|error| panic!("{mode_name} gateway proof failed: {error}"));
}

async fn exercise_disabled(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let operator =
        support::found_and_join_with_service_urls(docker, machine, &[], "disabled").await?;
    let image = support::start_mutable_registry(docker, machine, &[]).await?;
    support::create_namespace_and_deploy(
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
    support::assert_gateway_http(docker, machine, DISABLED_HOSTNAME, BODY).await?;
    let removed = support::run_cli(
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
    support::wait_for_gateway_status(docker, machine, DISABLED_HOSTNAME, 404).await?;

    let second = attach_route(&operator, DISABLED_HOSTNAME)?;
    require(
        first != second,
        format!("reattach reused removed route identity {first}"),
    )?;
    support::assert_gateway_http(docker, machine, DISABLED_HOSTNAME, BODY).await
}

async fn exercise_managed(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let operator = support::found_and_join_with_service_urls(docker, machine, &[], "ployz").await?;
    start_lease_stub(docker, machine).await?;
    exec_ok(
        docker,
        machine,
        &[
            "sh",
            "-c",
            &format!(
                "printf '%s\\n' 'PLOYZ_LEASE_WORKER_ORIGIN={LEASE_STUB_ORIGIN}' >> /var/lib/ployz/ployzd.env && systemctl restart ployzd-api.service && systemctl is-active ployzd-api.service"
            ),
        ],
    )
    .await?;

    let image = support::start_mutable_registry(docker, machine, &[]).await?;
    support::create_namespace_and_deploy(
        &operator,
        NAMESPACE,
        SERVICE,
        &image,
        SECRET_NAME,
        SECRET_VALUE,
    )?;
    let routes = wait_for_route_rows(docker, machine, 1).await?;
    let [(_, route)] = routes.as_slice() else {
        return Err(format!(
            "managed deploy produced the wrong routes: {routes:?}"
        ));
    };
    require(
        route.hostname.as_str() == MANAGED_HOSTNAME
            && route.origin == RouteBindingOrigin::Automatic
            && route.ingress_mode == IngressMode::Direct,
        format!("managed deploy attached the wrong hostname: {route:?}"),
    )?;
    support::assert_gateway_http(docker, machine, MANAGED_HOSTNAME, BODY).await?;

    let request = exec_ok(docker, machine, &["cat", "/run/ployz-lease-request.json"])
        .await?
        .stdout;
    let request: serde_json::Value = serde_json::from_str(request.trim())
        .map_err(|error| format!("lease stub recorded invalid JSON: {error}"))?;
    require(
        request
            .get("ipv4")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|addresses| !addresses.is_empty()),
        format!("managed lease request omitted the RFC1918 roster endpoint: {request}"),
    )
}

fn attach_route(
    operator: &support::OperatorFixture,
    hostname: &str,
) -> Result<RouteBindingRowId, String> {
    let output = support::run_cli(
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

async fn wait_for_route_rows(
    docker: &Docker,
    machine: &DindMachine,
    expected: usize,
) -> Result<Vec<(RouteBindingRowId, RouteBindingDocument)>, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        last = route_rows(docker, machine).await?;
        if last.len() == expected {
            return Ok(last);
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!(
        "route rows did not converge to {expected}: {last:?}"
    ))
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

async fn start_lease_stub(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    const PROGRAM: &str = r#"
import ipaddress
import json
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path != "/v1/leases":
            self.send_response(404)
            self.end_headers()
            return
        length = int(self.headers.get("Content-Length", "0"))
        request = json.loads(self.rfile.read(length))
        addresses = [ipaddress.ip_address(value) for value in request.get("ipv4", [])]
        if not addresses or not all(address.is_private for address in addresses):
            self.send_response(422)
            self.end_headers()
            return
        with open("/run/ployz-lease-request.json", "w", encoding="utf-8") as evidence:
            json.dump(request, evidence)
        response = json.dumps({
            "lease": {
                "name": "brisk-river-x7f3",
                "token": "lease_token_123",
                "issued_at": "1700000000",
                "expires_at": "4102444800"
            },
            "bundle": None
        }).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)

    def log_message(self, *_):
        pass

HTTPServer(("127.0.0.1", 18080), Handler).serve_forever()
"#;
    exec_ok(
        docker,
        machine,
        &[
            "systemd-run",
            "--unit=ployz-lease-stub.service",
            "--property=Restart=no",
            "--collect",
            "/usr/bin/python3",
            "-c",
            PROGRAM,
        ],
    )
    .await?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if exec_ok(
            docker,
            machine,
            &["systemctl", "is-active", "ployz-lease-stub.service"],
        )
        .await
        .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err("managed lease stub did not become active".to_owned())
}
