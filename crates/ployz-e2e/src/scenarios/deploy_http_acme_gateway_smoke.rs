use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::wait_until;
use std::time::Duration;

const SERVICE_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const GATEWAY_LISTENER_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
const ACME_STATUS_WAIT_TIMEOUT: Duration = Duration::from_secs(240);
const HTTPS_WAIT_TIMEOUT: Duration = Duration::from_secs(240);
const ACME_SMOKE_HOSTNAME: &str = "acme-smoke.test";
const ACME_SMOKE_BODY: &str = "ployz acme smoke";

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.machine_add("founder", "peer")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;
    run.wait_mesh_ready_name("peer")?;
    run.log_progress("wait founder gateway listeners");
    wait_for_gateway_listener(run, "founder", 80)?;
    wait_for_gateway_listener(run, "founder", 443)?;
    run.log_progress("wait peer gateway listeners");
    wait_for_gateway_listener(run, "peer", 80)?;
    wait_for_gateway_listener(run, "peer", 443)?;
    run.start_pebble_for_http01("founder")?;
    run.log_progress("deploy http smoke manifest");
    deploy_http_smoke_manifest(run, "founder")?;
    run.log_progress("wait founder http smoke container");
    run.wait_service_container_name("founder", "default", "web")?;
    run.log_progress("wait founder direct http");
    wait_for_service_http(run, "founder", "default", "web")?;
    run.log_progress("wait managed certificate active");
    wait_for_managed_certificate_active(run, "founder")?;
    run.log_progress("wait founder gateway https");
    wait_for_gateway_https(run, "founder")?;
    run.log_progress("wait peer gateway https");
    wait_for_gateway_https(run, "peer")
}

fn wait_for_gateway_listener(run: &ScenarioRun, node_name: &str, port: u16) -> Result<()> {
    let command = format!("timeout 2 bash -lc '</dev/tcp/127.0.0.1/{port}'");
    wait_until(GATEWAY_LISTENER_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, &command)?;
        Ok(output.status.success())
    })
    .map_err(|error| {
        Error::Message(format!(
            "gateway listener on {node_name}:127.0.0.1:{port} did not become reachable: {error}"
        ))
    })
}

fn deploy_http_smoke_manifest(run: &ScenarioRun, node_name: &str) -> Result<()> {
    let manifest = format!(
        r#"{{
  "namespace": "default",
  "services": [
    {{
      "name": "web",
      "placement": {{"replicated": {{"count": 1}}}},
      "template": {{
        "image": "ployz-e2e-preload/http-smoke:latest",
        "command": ["sh", "-c", "mkdir -p /www && printf '{ACME_SMOKE_BODY}\\n' >/www/index.html && httpd -f -p 80 -h /www"]
      }},
      "network": "overlay",
      "service_ports": [
        {{"name": "http", "container_port": 80, "protocol": "tcp"}}
      ],
      "routes": [
        {{"http": {{"service_port": "http", "hostnames": ["{ACME_SMOKE_HOSTNAME}"], "path_prefix": "/"}}}}
      ]
    }}
  ]
}}"#
    );
    let command = format!(
        "cat >/tmp/ployz-acme-smoke.json <<'EOF'\n{manifest}\nEOF\nployzd deploy -f /tmp/ployz-acme-smoke.json"
    );
    run.ssh_expect_ok_name(node_name, &command)?;
    Ok(())
}

fn wait_for_service_http(
    run: &ScenarioRun,
    node_name: &str,
    namespace: &str,
    service: &str,
) -> Result<()> {
    let command = format!(
        "sh -lc 'container_id=$(docker ps -q --filter label=dev.ployz.namespace={namespace} --filter label=dev.ployz.service={service} | head -n1); \
         test -n \"$container_id\"; \
         container_ip=$(docker inspect --format \"{{{{range .NetworkSettings.Networks}}}}{{{{.IPAddress}}}}{{{{end}}}}\" \"$container_id\"); \
         test -n \"$container_ip\"; \
         curl -fsS \"http://$container_ip\" | grep -Fq \"{ACME_SMOKE_BODY}\"'"
    );

    wait_until(SERVICE_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, &command)?;
        Ok(output.status.success())
    })
    .map_err(|error| {
        Error::Message(format!(
            "service '{service}' in namespace '{namespace}' on {node_name} did not serve http: {error}"
        ))
    })
}

fn wait_for_managed_certificate_active(run: &ScenarioRun, node_name: &str) -> Result<()> {
    let request = serde_json::json!({
        "AcmeHttp01Status": {
            "hostname": ACME_SMOKE_HOSTNAME
        }
    });
    let command = format!("printf '%s\\n' '{}' | ployzd rpc-stdio", request);
    let mut last_output = String::new();

    wait_until(ACME_STATUS_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, &command)?;
        last_output = output.stdout.trim().to_string();
        if !output.status.success() {
            return Ok(false);
        }
        Ok(certificate_is_active(&last_output))
    })
    .map_err(|error| {
        Error::Message(format!(
            "managed certificate for '{ACME_SMOKE_HOSTNAME}' did not become active on {node_name}: {error}; last_status={last_output}"
        ))
    })
}

fn certificate_is_active(response_json: &str) -> bool {
    let Ok(response) = serde_json::from_str::<serde_json::Value>(response_json) else {
        return false;
    };
    response
        .pointer("/payload/certificate/lifecycle/state")
        .and_then(serde_json::Value::as_str)
        == Some("active")
}

fn wait_for_gateway_https(run: &ScenarioRun, node_name: &str) -> Result<()> {
    let pebble_name = run.pebble_container_name();
    let command = format!(
        "curl -kfsS https://{pebble_name}:15000/roots/0 -o /tmp/ployz-pebble-issuer-root.pem && \
         curl -fsS --cacert /tmp/ployz-pebble-issuer-root.pem \
         --resolve {ACME_SMOKE_HOSTNAME}:443:127.0.0.1 \
         https://{ACME_SMOKE_HOSTNAME}/ | grep -Fq \"{ACME_SMOKE_BODY}\""
    );

    wait_until(HTTPS_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, &command)?;
        Ok(output.status.success())
    })
    .map_err(|error| {
        Error::Message(format!(
            "gateway did not serve HTTPS for '{ACME_SMOKE_HOSTNAME}' on {node_name}: {error}"
        ))
    })
}
