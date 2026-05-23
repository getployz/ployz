use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use mvp_acme::AcmeIssuerConfig;
use mvp_deploy::InstanceId;
use mvp_node::{
    AcmeIssueOptions, InitOptions, ProductDeployOptions, ServingRoleOptions, ServingRoleRequest,
    ServingRoleResponse, ServingRoleSuccess, deploy_product_service_with_runtime, init_node,
    issue_product_certificate_with_config, run_gateway_role,
};
use mvp_runtime::{RuntimeBackend, RuntimeInstance, RuntimeInstanceSpec, RuntimeInstanceState};
use serde::Serialize;

use crate::metrics::{reset_dir, scenario_dir, write_json};

const HOSTNAME: &str = "web.example.test";

#[derive(Debug, Serialize)]
struct PebbleAcmeHttpsReport {
    scenario: &'static str,
    hostname: &'static str,
    order_url: String,
    projected_certificates: usize,
    http_listen_addr: SocketAddr,
    tls_listen_addr: SocketAddr,
    backend_body: String,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for Pebble HTTPS: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let started = Instant::now();
    let root = scenario_dir("pebble-acme-https-contract");
    reset_dir(&root)?;
    let pebble = PebbleContainer::start()?;
    let backend = StaticHttpBackend::start("instance=pebble-acme-https\n")?;
    let state = init_node(
        InitOptions::new(root.join("node"))
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .map_err(|error| error.to_string())?;
    deploy_product_service_with_runtime(
        ProductDeployOptions::new(state.paths().state_dir.clone())
            .with_deploy_id("deploy-pebble")
            .with_target_node("node-a")
            .with_service("web")
            .with_revision("rev-1")
            .with_hostname(HOSTNAME),
        Some(Arc::new(backend.clone())),
    )
    .await
    .map_err(|error| error.to_string())?;

    let control = root.join("gateway.sock");
    let gateway_state_dir = state.paths().state_dir.clone();
    let gateway_control = control.clone();
    let gateway = tokio::spawn(async move {
        run_gateway_role(
            ServingRoleOptions::new(
                gateway_state_dir,
                "127.0.0.1:0".parse().unwrap(),
                gateway_control,
            )
            .with_tls_listen("127.0.0.1:0".parse().unwrap()),
        )
        .await
    });
    let initial_status = wait_for_status(&control)?;
    let http_addr = initial_status.listen_addr;
    let tls_addr = initial_status
        .tls_listen_addr
        .ok_or_else(|| "gateway status did not report TLS listener".to_string())?;

    let issue = issue_product_certificate_with_config(
        AcmeIssueOptions::new(
            state.paths().state_dir.clone(),
            HOSTNAME,
            format!("http://{http_addr}"),
        )
        .with_gateway_control_socket(control.clone()),
        AcmeIssuerConfig {
            directory_url: pebble.directory_url.clone(),
            contact_email: None,
            root_ca_path: Some(pebble.root_ca_path.clone()),
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    reload_gateway(&control)?;

    let body = curl_https(HOSTNAME, tls_addr, &pebble.issued_root_ca_path)?;
    if !body.contains("instance=pebble-acme-https") {
        return Err(format!("unexpected HTTPS body: {body}"));
    }

    shutdown_gateway(&control)?;
    gateway
        .await
        .map_err(|error| format!("gateway task join: {error}"))?
        .map_err(|error| error.to_string())?;

    let report = PebbleAcmeHttpsReport {
        scenario: "pebble-acme-https-contract",
        hostname: HOSTNAME,
        order_url: issue.order_url,
        projected_certificates: issue.projected_certificates,
        http_listen_addr: http_addr,
        tls_listen_addr: tls_addr,
        backend_body: body,
        elapsed_ms: started.elapsed().as_millis(),
    };
    write_json(
        &root.join("pebble-acme-https-contract-metrics.json"),
        &report,
    )?;
    eprintln!("PASS pebble-acme-https-contract");
    Ok(())
}

#[derive(Clone)]
struct StaticHttpBackend {
    instance: RuntimeInstance,
    shutdown: Arc<AtomicBool>,
}

impl StaticHttpBackend {
    fn start(body: &'static str) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        let addr = listener.local_addr().map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        thread::spawn(move || {
            while !thread_shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 1024];
                        let _ = stream.read(&mut request);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            instance: RuntimeInstance {
                instance_id: InstanceId::new("deploy-pebble-web-rev-1"),
                service: mvp_projection::ServiceName::new("web"),
                revision: mvp_deploy::RevisionId::new("rev-1"),
                address: addr.to_string(),
                backend_id: None,
                backend_name: None,
                pid: None,
                state: RuntimeInstanceState::Running,
            },
            shutdown,
        })
    }
}

impl RuntimeBackend for StaticHttpBackend {
    fn prepare(&self, _spec: &RuntimeInstanceSpec) -> mvp_runtime::RuntimeResult<RuntimeInstance> {
        Ok(self.instance.clone())
    }

    fn start(&self, _spec: &RuntimeInstanceSpec) -> mvp_runtime::RuntimeResult<RuntimeInstance> {
        Ok(self.instance.clone())
    }

    fn drain(
        &self,
        _instance_id: &InstanceId,
    ) -> mvp_runtime::RuntimeResult<Option<RuntimeInstance>> {
        Ok(Some(self.instance.clone()))
    }

    fn stop(
        &self,
        _instance_id: &InstanceId,
    ) -> mvp_runtime::RuntimeResult<Option<RuntimeInstance>> {
        Ok(Some(RuntimeInstance {
            state: RuntimeInstanceState::Stopped,
            ..self.instance.clone()
        }))
    }

    fn list(&self) -> mvp_runtime::RuntimeResult<Vec<RuntimeInstance>> {
        Ok(vec![self.instance.clone()])
    }
}

impl Drop for StaticHttpBackend {
    fn drop(&mut self) {
        if Arc::strong_count(&self.shutdown) == 1 {
            self.shutdown.store(true, Ordering::SeqCst);
        }
    }
}

struct PebbleContainer {
    id: String,
    directory_url: String,
    root_ca_path: PathBuf,
    issued_root_ca_path: PathBuf,
}

impl PebbleContainer {
    fn start() -> Result<Self, String> {
        docker_available()?;
        let repo = std::env::current_dir().map_err(|error| error.to_string())?;
        let pebble_dir = repo.join("packaging/e2e/pebble");
        let port = free_loopback_port()?;
        let management_port = free_loopback_port()?;
        let output = Command::new("docker")
            .arg("run")
            .arg("-d")
            .arg("--rm")
            .arg("-p")
            .arg(format!("127.0.0.1:{port}:14000"))
            .arg("-p")
            .arg(format!("127.0.0.1:{management_port}:15000"))
            .arg("-e")
            .arg("PEBBLE_VA_NOSLEEP=1")
            .arg("-e")
            .arg("PEBBLE_VA_ALWAYS_VALID=1")
            .arg("-v")
            .arg(format!("{}:/e2e-pebble:ro", pebble_dir.display()))
            .arg("ghcr.io/letsencrypt/pebble:latest")
            .arg("-config")
            .arg("/e2e-pebble/pebble-config.json")
            .arg("-strict=false")
            .output()
            .map_err(|error| format!("start Pebble container: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "docker run Pebble failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let directory_url = format!("https://127.0.0.1:{port}/dir");
        let root_ca_path = pebble_dir.join("pebble.minica.pem");
        wait_tcp(("127.0.0.1", port))?;
        wait_https_directory(&directory_url, &root_ca_path)?;
        let issued_root_ca_path = repo
            .join("MVP/tmp/pebble-acme-https-contract")
            .join("pebble-issued-root.pem");
        fetch_issued_root_ca(management_port, &root_ca_path, &issued_root_ca_path)?;
        Ok(Self {
            id,
            directory_url,
            root_ca_path,
            issued_root_ca_path,
        })
    }
}

impl Drop for PebbleContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .arg("rm")
            .arg("-f")
            .arg(&self.id)
            .output();
    }
}

fn docker_available() -> Result<(), String> {
    let status = Command::new("docker")
        .arg("version")
        .arg("--format")
        .arg("{{.Server.Version}}")
        .status()
        .map_err(|error| format!("docker is required for Pebble: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("docker is required for Pebble and is not available".to_string())
    }
}

fn free_loopback_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|error| error.to_string())
}

fn wait_tcp(addr: (&str, u16)) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!("timed out waiting for {}:{}", addr.0, addr.1))
}

fn wait_https_directory(url: &str, root_ca: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let status = Command::new("curl")
            .arg("--silent")
            .arg("--fail")
            .arg("--cacert")
            .arg(root_ca)
            .arg(url)
            .status();
        if status.is_ok_and(|status| status.success()) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!("timed out waiting for Pebble directory {url}"))
}

fn fetch_issued_root_ca(
    management_port: u16,
    server_root_ca: &Path,
    output_path: &Path,
) -> Result<(), String> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let status = Command::new("curl")
        .arg("--silent")
        .arg("--fail")
        .arg("--cacert")
        .arg(server_root_ca)
        .arg(format!("https://127.0.0.1:{management_port}/roots/0"))
        .arg("--output")
        .arg(output_path)
        .status()
        .map_err(|error| format!("fetch Pebble issued root CA: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("fetch Pebble issued root CA failed: {status}"))
    }
}

fn wait_for_status(control: &Path) -> Result<mvp_node::ServingRoleProcessStatus, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match control_request(control, &ServingRoleRequest::Readiness)? {
            ServingRoleResponse::Success(ServingRoleSuccess::Status(status))
            | ServingRoleResponse::Success(ServingRoleSuccess::Reloaded(status)) => {
                return Ok(*status);
            }
            ServingRoleResponse::Success(ServingRoleSuccess::Shutdown) => {
                return Err("gateway shut down before readiness".to_string());
            }
            ServingRoleResponse::Failure(failure) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(25));
                let _ = failure;
            }
            ServingRoleResponse::Failure(failure) => return Err(format!("{failure:?}")),
        }
    }
}

fn reload_gateway(control: &Path) -> Result<(), String> {
    match control_request(control, &ServingRoleRequest::Reload)? {
        ServingRoleResponse::Success(ServingRoleSuccess::Reloaded(_)) => Ok(()),
        response => Err(format!("gateway reload failed: {response:?}")),
    }
}

fn shutdown_gateway(control: &Path) -> Result<(), String> {
    match control_request(control, &ServingRoleRequest::Shutdown)? {
        ServingRoleResponse::Success(ServingRoleSuccess::Shutdown) => Ok(()),
        response => Err(format!("gateway shutdown failed: {response:?}")),
    }
}

fn control_request(
    control: &Path,
    request: &ServingRoleRequest,
) -> Result<ServingRoleResponse, String> {
    let bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    let mut stream = connect_control(control)?;
    stream
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&response).map_err(|error| error.to_string())
}

fn connect_control(control: &Path) -> Result<UnixStream, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match UnixStream::connect(control) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(format!("connect control '{}': {error}", control.display())),
        }
    }
}

fn curl_https(hostname: &str, addr: SocketAddr, root_ca: &Path) -> Result<String, String> {
    let output = Command::new("curl")
        .arg("--silent")
        .arg("--show-error")
        .arg("--fail")
        .arg("--cacert")
        .arg(root_ca)
        .arg("--resolve")
        .arg(format!("{hostname}:{}:127.0.0.1", addr.port()))
        .arg("--header")
        .arg(format!("Host: {hostname}"))
        .arg(format!("https://{hostname}:{}/", addr.port()))
        .output()
        .map_err(|error| format!("run curl HTTPS check: {error}"))?;
    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|error| error.to_string())
    } else {
        Err(format!(
            "curl HTTPS check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}
