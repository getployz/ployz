use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use crate::support::wait_until;
use serde::{Deserialize, Serialize};

use super::cluster::MvpBootstrapEvidence;
use super::{DATA_PLANE_WAIT_TIMEOUT, STATE_DIR, shell_quote};

const ACME_HTTPS_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(240);
const GATEWAY_HTTP_PORT: u16 = 80;
const GATEWAY_HTTPS_PORT: u16 = 443;
const GATEWAY_CONTROL: &str = "/var/lib/ployz-mvp/node/control/gateway.sock";
const GATEWAY_LOG: &str = "/tmp/mvp-node-gateway.log";
const GATEWAY_SNAPSHOT: &str = "/var/lib/ployz-mvp/node/gateway.snapshot";
const DNS_CONTROL: &str = "/var/lib/ployz-mvp/node/control/dns.sock";
const DNS_LOG: &str = "/tmp/mvp-node-dns.log";
const CLIENT_IMAGE: &str = "ployz-e2e-preload/http-smoke:latest";
const NODE_A_NETWORK: &str = "ployz-mvp-node-a";
const WEB_HOSTNAME: &str = "web.example.test";
const WEB_BODY: &str = "ok-web-rev-1";
pub(super) const API_HOSTNAME: &str = "api.example.test";
const ECHO_SERVICE_DNS: &str = "echo.service.example.test";
const ECHO_SERVICE_URL: &str = "http://echo.service.example.test:8080/";
const ACME_ACCOUNT_PATH: &str = "/var/lib/ployz-mvp/node/acme-accounts.json";
const PEBBLE_ROOT_PATH: &str = "/tmp/ployz-mvp-pebble-root.pem";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpGatewayHttpEvidence {
    pub(crate) service: &'static str,
    pub(crate) gateway_node: &'static str,
    pub(crate) hostname: &'static str,
    pub(crate) expected_body: &'static str,
    pub(crate) body: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpContainerDnsEvidence {
    pub(crate) client_node: &'static str,
    pub(crate) dns_node: &'static str,
    pub(crate) dns_server: String,
    pub(crate) network: &'static str,
    pub(crate) service_name: &'static str,
    pub(crate) dns_answer: String,
    pub(crate) body: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpAcmeHttpsEvidence {
    pub(crate) hostname: &'static str,
    pub(crate) order_url: String,
    pub(crate) projected_certificates: usize,
    pub(crate) https_probes: Vec<MvpGatewayHttpsEvidence>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpGatewayHttpsEvidence {
    pub(crate) gateway_node: &'static str,
    pub(crate) hostname: &'static str,
    pub(crate) expected_body: &'static str,
    pub(crate) body: String,
}

#[derive(Debug, Deserialize)]
struct AcmeIssueReport {
    order_url: String,
    projected_certificates: usize,
}

pub(crate) fn verify_gateway_http(run: &ScenarioRun) -> Result<Vec<MvpGatewayHttpEvidence>> {
    for node in ["founder", "peer", "edge"] {
        wait_gateway_snapshot_hosts(
            run,
            node,
            &["web.example.test", "api.example.test", "echo.example.test"],
        )?;
        start_gateway(run, node)?;
    }
    [
        ("web", "peer", WEB_HOSTNAME, WEB_BODY),
        ("web", "edge", WEB_HOSTNAME, WEB_BODY),
        ("api", "founder", API_HOSTNAME, "ok-api-rev-1"),
        ("api", "edge", API_HOSTNAME, "ok-api-rev-1"),
    ]
    .into_iter()
    .map(|(service, gateway_node, hostname, expected_body)| {
        let body = wait_gateway_body(run, gateway_node, hostname, expected_body)?;
        Ok(MvpGatewayHttpEvidence {
            service,
            gateway_node,
            hostname,
            expected_body,
            body,
        })
    })
    .collect()
}

pub(crate) fn verify_container_dns(
    run: &ScenarioRun,
    bootstrap: &[MvpBootstrapEvidence],
) -> Result<MvpContainerDnsEvidence> {
    for node in [
        DnsNode {
            harness_node: "founder",
            mvp_node_id: "node-a",
        },
        DnsNode {
            harness_node: "peer",
            mvp_node_id: "node-b",
        },
        DnsNode {
            harness_node: "edge",
            mvp_node_id: "node-c",
        },
    ] {
        wait_gateway_snapshot_hosts(run, node.harness_node, &["echo.example.test"])?;
        start_dns(run, bootstrap, node)?;
    }
    probe_container_dns(run, bootstrap)
}

pub(super) fn probe_container_dns(
    run: &ScenarioRun,
    bootstrap: &[MvpBootstrapEvidence],
) -> Result<MvpContainerDnsEvidence> {
    let dns_server = gateway_ip_for_node(bootstrap, "node-a")?;
    let nslookup_stdout =
        run_client_container_probe(run, &dns_server, &["nslookup", ECHO_SERVICE_DNS])?;
    let dns_answer = container_dns_answer(&nslookup_stdout)?;
    let body = run_client_container(run, &dns_server, &["wget", "-qO-", ECHO_SERVICE_URL])?;
    if body.trim() != "ok-echo-rev-1" {
        return Err(Error::Message(format!(
            "container DNS client reached echo but body was unexpected: {body}"
        )));
    }
    Ok(MvpContainerDnsEvidence {
        client_node: "node-a",
        dns_node: "node-a",
        dns_server,
        network: NODE_A_NETWORK,
        service_name: ECHO_SERVICE_DNS,
        dns_answer,
        body: body.trim().to_string(),
    })
}

pub(super) fn probe_web_https(
    run: &ScenarioRun,
    gateway_node: &'static str,
) -> Result<MvpGatewayHttpsEvidence> {
    wait_gateway_snapshot_certificate(run, gateway_node, WEB_HOSTNAME)?;
    let body = wait_gateway_https_body(run, gateway_node, WEB_HOSTNAME, WEB_BODY)?;
    Ok(MvpGatewayHttpsEvidence {
        gateway_node,
        hostname: WEB_HOSTNAME,
        expected_body: WEB_BODY,
        body,
    })
}

pub(crate) fn verify_acme_https(run: &ScenarioRun) -> Result<MvpAcmeHttpsEvidence> {
    run.start_pebble_for_http01("founder")?;
    let issue = issue_certificate(run)?;
    let https_probes = ["peer", "edge"]
        .into_iter()
        .map(|gateway_node| probe_web_https(run, gateway_node))
        .collect::<Result<Vec<_>>>()?;
    Ok(MvpAcmeHttpsEvidence {
        hostname: WEB_HOSTNAME,
        order_url: issue.order_url,
        projected_certificates: issue.projected_certificates,
        https_probes,
    })
}

#[derive(Clone, Copy)]
struct DnsNode {
    harness_node: &'static str,
    mvp_node_id: &'static str,
}

fn wait_gateway_snapshot_hosts(
    run: &ScenarioRun,
    node: &'static str,
    expected_hosts: &[&str],
) -> Result<()> {
    let mut last_output = String::new();
    wait_until(DATA_PLANE_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node, &format!("cat {GATEWAY_SNAPSHOT}"))?;
        last_output = output.combined();
        if !output.status.success() {
            return Ok(false);
        }
        Ok(snapshot_contains_hosts(&output.stdout, expected_hosts))
    })
    .map_err(|error| {
        Error::Message(format!(
            "gateway snapshot on {node} did not contain hosts {:?}: {error}\nlast output:\n{last_output}",
            expected_hosts
        ))
    })
}

fn wait_gateway_snapshot_certificate(
    run: &ScenarioRun,
    node: &'static str,
    hostname: &str,
) -> Result<()> {
    let mut last_output = String::new();
    wait_until(ACME_HTTPS_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node, &format!("cat {GATEWAY_SNAPSHOT}"))?;
        last_output = output.combined();
        if !output.status.success() {
            return Ok(false);
        }
        Ok(snapshot_contains_certificate(&output.stdout, hostname))
    })
    .map_err(|error| {
        Error::Message(format!(
            "gateway snapshot on {node} did not contain certificate for {hostname}: {error}\nlast output:\n{last_output}"
        ))
    })
}

pub(super) fn reload_gateway(run: &ScenarioRun, node: &'static str) -> Result<()> {
    run.ssh_expect_ok_name(
        node,
        &format!("mvp-node serving-reload --control {GATEWAY_CONTROL}"),
    )?;
    Ok(())
}

fn start_gateway(run: &ScenarioRun, node: &'static str) -> Result<()> {
    run.ssh_expect_ok_name(
        node,
        &format!(
            "rm -f {control} {log}; \
             nohup mvp-node gateway --state {state} --listen 0.0.0.0:{http_port} \
             --tls-listen 0.0.0.0:{https_port} --control {control} \
             > {log} 2>&1 < /dev/null & echo $! > /tmp/mvp-node-gateway.pid",
            control = GATEWAY_CONTROL,
            log = GATEWAY_LOG,
            state = STATE_DIR,
            http_port = GATEWAY_HTTP_PORT,
            https_port = GATEWAY_HTTPS_PORT
        ),
    )?;
    Ok(())
}

pub(super) fn wait_gateway_body(
    run: &ScenarioRun,
    gateway_node: &'static str,
    hostname: &'static str,
    expected_body: &'static str,
) -> Result<String> {
    let command = format!(
        "curl -fsS -H {} http://127.0.0.1:{GATEWAY_HTTP_PORT}/",
        shell_quote(&format!("Host: {hostname}"))
    );
    let mut last_output = String::new();
    let mut body = String::new();
    wait_until(DATA_PLANE_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(gateway_node, &command)?;
        last_output = output.combined();
        body = output.stdout.trim().to_string();
        Ok(output.status.success() && body == expected_body)
    })
    .map_err(|error| {
        Error::Message(format!(
            "gateway {gateway_node} did not serve {hostname} body {expected_body}: {error}\nlast output:\n{last_output}"
        ))
    })?;
    Ok(body)
}

fn issue_certificate(run: &ScenarioRun) -> Result<AcmeIssueReport> {
    let command = format!(
        "PLOYZ_ACME_DIRECTORY_URL=https://pebble:14000/dir \
         PLOYZ_ACME_ROOT_CA_PATH=/e2e-pebble/pebble.minica.pem \
         mvp-node acme-issue --state {state} --hostname {hostname} \
         --gateway http://127.0.0.1:{http_port} --gateway-control {control} \
         --account-path {account_path}",
        state = STATE_DIR,
        hostname = shell_quote(WEB_HOSTNAME),
        http_port = GATEWAY_HTTP_PORT,
        control = GATEWAY_CONTROL,
        account_path = ACME_ACCOUNT_PATH
    );
    let output = run.ssh_expect_ok_name("founder", &command)?;
    serde_json::from_str::<AcmeIssueReport>(output.stdout.trim()).map_err(|error| {
        Error::Message(format!(
            "failed to decode mvp-node acme-issue report: {error}; output={}",
            output.combined()
        ))
    })
}

fn wait_gateway_https_body(
    run: &ScenarioRun,
    gateway_node: &'static str,
    hostname: &'static str,
    expected_body: &'static str,
) -> Result<String> {
    let pebble_name = run.pebble_container_name();
    let command = format!(
        "curl -kfsS https://{pebble_name}:15000/roots/0 -o {root_path} && \
         mvp-node serving-reload --control {control} >/tmp/mvp-serving-reload.json && \
         curl -fsS --cacert {root_path} --resolve {hostname}:{https_port}:127.0.0.1 \
         https://{hostname}/",
        root_path = PEBBLE_ROOT_PATH,
        control = GATEWAY_CONTROL,
        https_port = GATEWAY_HTTPS_PORT
    );
    let mut last_output = String::new();
    let mut body = String::new();
    wait_until(ACME_HTTPS_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(gateway_node, &command)?;
        last_output = output.combined();
        body = output.stdout.trim().to_string();
        Ok(output.status.success() && body == expected_body)
    })
    .map_err(|error| {
        Error::Message(format!(
            "gateway {gateway_node} did not serve HTTPS for {hostname} body {expected_body}: {error}\nlast output:\n{last_output}"
        ))
    })?;
    Ok(body)
}

fn start_dns(run: &ScenarioRun, bootstrap: &[MvpBootstrapEvidence], node: DnsNode) -> Result<()> {
    let listen = gateway_ip_for_node(bootstrap, node.mvp_node_id)?;
    run.ssh_expect_ok_name(
        node.harness_node,
        &format!(
            "rm -f {control} {log}; \
             nohup mvp-node dns --state {state} --listen {listen}:53 \
             --control {control} > {log} 2>&1 < /dev/null & echo $! > /tmp/mvp-node-dns.pid",
            control = DNS_CONTROL,
            log = DNS_LOG,
            state = STATE_DIR
        ),
    )?;
    Ok(())
}

fn run_client_container(run: &ScenarioRun, dns_server: &str, argv: &[&str]) -> Result<String> {
    let output = run.ssh_expect_ok_name("founder", &client_container_command(dns_server, argv))?;
    Ok(output.stdout.trim().to_string())
}

fn run_client_container_probe(
    run: &ScenarioRun,
    dns_server: &str,
    argv: &[&str],
) -> Result<String> {
    let output = run.ssh_run_name("founder", &client_container_command(dns_server, argv))?;
    Ok(output.stdout.trim().to_string())
}

fn client_container_command(dns_server: &str, argv: &[&str]) -> String {
    format!(
        "docker run --rm --network {network} --dns {dns_server} {image} {argv}",
        network = NODE_A_NETWORK,
        image = CLIENT_IMAGE,
        argv = argv
            .iter()
            .map(|value| shell_quote(value))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn gateway_ip_for_node(bootstrap: &[MvpBootstrapEvidence], node_id: &str) -> Result<String> {
    let evidence = bootstrap
        .iter()
        .find(|evidence| evidence.mvp_node_id == node_id)
        .ok_or_else(|| Error::Message(format!("missing bootstrap evidence for {node_id}")))?;
    gateway_ip_from_subnet(&evidence.container_subnet)
}

fn gateway_ip_from_subnet(subnet: &str) -> Result<String> {
    let Some((network, prefix)) = subnet.split_once('/') else {
        return Err(Error::Message(format!(
            "container subnet missing prefix: {subnet}"
        )));
    };
    if prefix != "24" {
        return Err(Error::Message(format!(
            "container subnet is not a /24: {subnet}"
        )));
    }
    let mut octets = network.split('.');
    let Some(first) = octets.next() else {
        return Err(Error::Message(format!(
            "container subnet is empty: {subnet}"
        )));
    };
    let Some(second) = octets.next() else {
        return Err(Error::Message(format!(
            "container subnet missing second octet: {subnet}"
        )));
    };
    let Some(third) = octets.next() else {
        return Err(Error::Message(format!(
            "container subnet missing third octet: {subnet}"
        )));
    };
    let Some(_fourth) = octets.next() else {
        return Err(Error::Message(format!(
            "container subnet missing fourth octet: {subnet}"
        )));
    };
    if octets.next().is_some() {
        return Err(Error::Message(format!(
            "container subnet has too many octets: {subnet}"
        )));
    }
    Ok(format!("{first}.{second}.{third}.1"))
}

fn container_dns_answer(nslookup_stdout: &str) -> Result<String> {
    nslookup_stdout
        .lines()
        .filter_map(|line| line.split_once("Address:"))
        .map(|(_prefix, answer)| answer.trim())
        .filter(|answer| answer.starts_with("10.210."))
        .last()
        .map(ToString::to_string)
        .ok_or_else(|| {
            Error::Message(format!(
                "nslookup did not include a container subnet answer: {nslookup_stdout}"
            ))
        })
}

fn snapshot_contains_hosts(snapshot: &str, expected_hosts: &[&str]) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(snapshot) else {
        return false;
    };
    expected_hosts.iter().all(|expected| {
        value
            .get("routes")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|route| {
                route
                    .get("hostnames")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .any(|host| host.as_str() == Some(*expected))
    })
}

fn snapshot_contains_certificate(snapshot: &str, hostname: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(snapshot) else {
        return false;
    };
    value
        .get("certificates")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .any(|certificate| {
            certificate
                .get("hostname")
                .and_then(serde_json::Value::as_str)
                == Some(hostname)
        })
}

#[cfg(test)]
mod tests {
    use super::{
        container_dns_answer, gateway_ip_from_subnet, snapshot_contains_certificate,
        snapshot_contains_hosts,
    };

    #[test]
    fn snapshot_host_probe_requires_all_hosts() {
        let snapshot = r#"{
            "routes": [
                { "hostnames": ["web.example.test"] },
                { "hostnames": ["api.example.test"] }
            ]
        }"#;

        assert!(snapshot_contains_hosts(
            snapshot,
            &["web.example.test", "api.example.test"]
        ));
        assert!(!snapshot_contains_hosts(
            snapshot,
            &["web.example.test", "echo.example.test"]
        ));
    }

    #[test]
    fn snapshot_certificate_probe_requires_hostname() {
        let snapshot = r#"{
            "certificates": [
                { "hostname": "web.example.test" }
            ]
        }"#;

        assert!(snapshot_contains_certificate(snapshot, "web.example.test"));
        assert!(!snapshot_contains_certificate(snapshot, "api.example.test"));
    }

    #[test]
    fn gateway_ip_uses_container_subnet_gateway() {
        assert_eq!(
            gateway_ip_from_subnet("10.210.121.0/24").expect("gateway ip"),
            "10.210.121.1"
        );
        assert!(gateway_ip_from_subnet("10.210.121.0/16").is_err());
    }

    #[test]
    fn container_dns_answer_extracts_container_subnet_answer() {
        let output = r#"Server:		10.210.121.1
Address:	10.210.121.1:53

Name:	echo.service.example.test
Address: 10.210.137.42
"#;

        assert_eq!(
            container_dns_answer(output).expect("dns answer"),
            "10.210.137.42"
        );
    }
}
