use chrono::{SecondsFormat, Utc};
use ployz_core::{
    CADDY_VERIFY_PATH, ContainerObservation, ContainerRuntimeObservation, HealthObservation,
    HttpProtocol, IngressHost, IngressHostname, Machine, MachineId, PortPublication,
    ServiceContainer, ServiceName, service_containers,
};
use reqwest::{Client, StatusCode, header};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    future::Future,
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{CertificateChallenge, CertificateMaterial, CertificateRow, ReplicatedStore},
    filesystem::{atomic_write, set_ployz_group},
};

pub const CONFIG_FILE: &str = "Caddyfile";
const CERTS_DIR: &str = "certs";
const CONTAINER_CERTS_DIR: &str = "/config/certs";
const ADMIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Admin(String),
    #[error("{0}")]
    Template(String),
}

trait CaddyAdmin: Send + Sync {
    fn adapt(&self, caddyfile: &str) -> impl Future<Output = Result<String, Error>> + Send;
    fn load(&self, json: &str) -> impl Future<Output = Result<(), Error>> + Send;
}

struct AdminClient {
    client: Client,
}

impl AdminClient {
    async fn connect_if_available(socket: &Path) -> Result<Option<Self>, Error> {
        if !matches!(
            tokio::time::timeout(Duration::from_secs(1), UnixStream::connect(socket)).await,
            Ok(Ok(_))
        ) {
            return Ok(None);
        }
        Ok(Some(Self {
            client: Client::builder()
                .timeout(ADMIN_TIMEOUT)
                .unix_socket(socket)
                .build()?,
        }))
    }

    async fn post(&self, path: &str, content_type: &str, body: String) -> Result<String, Error> {
        let response = self
            .client
            .post(format!("http://localhost{path}"))
            .header(header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if status != StatusCode::OK {
            return Err(Error::Admin(if body.is_empty() {
                format!("Caddy admin returned {status}")
            } else {
                body
            }));
        }
        Ok(body)
    }
}

impl CaddyAdmin for AdminClient {
    async fn adapt(&self, caddyfile: &str) -> Result<String, Error> {
        let body = self
            .post("/adapt", "text/caddyfile", caddyfile.to_owned())
            .await?;
        let response: Value = serde_json::from_str(&body)?;
        response
            .get("result")
            .map(Value::to_string)
            .ok_or_else(|| Error::Admin("Caddy /adapt response omitted result".into()))
    }

    async fn load(&self, json: &str) -> Result<(), Error> {
        self.post("/load", "application/json", json.to_owned())
            .await?;
        Ok(())
    }
}

#[must_use]
pub fn caddyfile_path(data_dir: &Path) -> PathBuf {
    data_dir.join("caddy").join(CONFIG_FILE)
}

pub async fn run(
    machine: Machine,
    replicated: ReplicatedStore,
    config_file: PathBuf,
    admin_socket: PathBuf,
    shutdown: CancellationToken,
) -> io::Result<()> {
    prepare_directory(
        admin_socket
            .parent()
            .ok_or_else(|| io::Error::other("Caddy admin socket has no parent"))?,
    )?;
    let mut container_changes = replicated
        .subscribe_container_changes()
        .await
        .map_err(io::Error::other)?;
    let mut certificate_changes = replicated
        .subscribe_certificate_changes()
        .await
        .map_err(io::Error::other)?;
    let mut last_applied = None;
    loop {
        let snapshot = match (
            replicated.containers().await,
            replicated.certificate_state().await,
        ) {
            (Ok(containers), Ok(certificates)) => Some((containers.observations, certificates)),
            (Err(error), _) | (_, Err(error)) => {
                eprintln!("failed to rebuild Caddy projection: {error}");
                None
            }
        };
        if let Some(snapshot) = snapshot
            && last_applied.as_ref() != Some(&snapshot)
        {
            let admin = AdminClient::connect_if_available(&admin_socket)
                .await
                .map_err(io::Error::other)?;
            match reconcile(
                &machine,
                &snapshot.0,
                &snapshot.1,
                &config_file,
                admin.as_ref(),
            )
            .await
            {
                Ok(()) => last_applied = Some(snapshot),
                Err(error) => {
                    last_applied = None;
                    eprintln!("failed to update Caddy configuration: {error}");
                }
            }
        }
        tokio::select! {
            changed = container_changes.changed() => changed.map_err(io::Error::other)?,
            changed = certificate_changes.changed() => changed.map_err(io::Error::other)?,
            () = shutdown.cancelled() => return Ok(()),
        }
    }
}

async fn reconcile<A: CaddyAdmin>(
    machine: &Machine,
    observations: &[ContainerObservation],
    certificates: &BTreeMap<IngressHost, CertificateRow>,
    config_file: &Path,
    admin: Option<&A>,
) -> Result<(), Error> {
    // TODO(UT-116): keep the Caddy projection membership-blind until the membership model is
    // intentionally changed across replicated projections.
    write_certificate_files(config_file, certificates)?;
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let containers = service_containers(observations.iter().cloned());
    let caddyfile = generate_caddyfile(
        &machine.id,
        machine.name.as_str(),
        &containers,
        &timestamp,
        certificates,
        admin,
    )
    .await;
    if let Some(admin) = admin {
        let json = admin.adapt(&caddyfile).await?;
        admin.load(&json).await?;
    }
    write_caddyfile(config_file, &caddyfile)?;
    remove_stale_certificate_files(config_file, certificates)?;
    Ok(())
}

fn write_certificate_files(
    config_file: &Path,
    certificates: &BTreeMap<IngressHost, CertificateRow>,
) -> Result<(), Error> {
    let directory = config_file
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Caddyfile has no parent"))?
        .join(CERTS_DIR);
    prepare_directory(&directory)?;
    for (hostname, row) in certificates {
        let Some(material) = row.material() else {
            continue;
        };
        let stem = certificate_file_stem(hostname, material);
        let cert_path = directory.join(format!("{stem}.crt"));
        let key_path = directory.join(format!("{stem}.key"));
        atomic_write(&cert_path, material.certificate().as_bytes(), 0o644)?;
        set_ployz_group(&cert_path)?;
        atomic_write(&key_path, material.private_key().as_bytes(), 0o600)?;
        set_ployz_group(&key_path)?;
    }
    Ok(())
}

fn remove_stale_certificate_files(
    config_file: &Path,
    certificates: &BTreeMap<IngressHost, CertificateRow>,
) -> Result<(), Error> {
    let directory = config_file
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Caddyfile has no parent"))?
        .join(CERTS_DIR);
    if !directory.exists() {
        return Ok(());
    }
    let keep: BTreeSet<String> = certificates
        .iter()
        .filter_map(|(hostname, row)| {
            row.material().map(|material| {
                let stem = certificate_file_stem(hostname, material);
                [format!("{stem}.crt"), format!("{stem}.key")]
            })
        })
        .flatten()
        .collect();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !keep.contains(name) {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn certificate_file_stem(hostname: &IngressHost, material: &CertificateMaterial) -> String {
    let mut digest = Sha256::new();
    digest.update((material.certificate().len() as u64).to_le_bytes());
    digest.update(material.certificate().as_bytes());
    digest.update((material.private_key().len() as u64).to_le_bytes());
    digest.update(material.private_key().as_bytes());
    format!("{hostname}-{:x}", digest.finalize())
}

fn write_caddyfile(path: &Path, caddyfile: &str) -> Result<(), Error> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Caddyfile has no parent"))?;
    prepare_directory(directory)?;
    atomic_write(path, caddyfile.as_bytes(), 0o640)?;
    set_ployz_group(path)?;
    Ok(())
}

fn prepare_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o750))?;
    set_ployz_group(path)
}

async fn generate_caddyfile<A: CaddyAdmin>(
    local_machine: &MachineId,
    machine_name: &str,
    containers: &[ServiceContainer],
    timestamp: &str,
    certificates: &BTreeMap<IngressHost, CertificateRow>,
    admin: Option<&A>,
) -> String {
    let mut output = automatic_caddyfile(
        local_machine,
        machine_name,
        containers,
        timestamp,
        None,
        certificates,
    );
    let Some(admin) = admin else {
        output.push_str(
            "\n# User-defined Caddy configs are unavailable because Caddy's admin API is not reachable.\n\
# Run `ployz caddy logs` and `ployz inspect caddy` to troubleshoot.\n",
        );
        return output;
    };

    let healthy = healthy_containers(local_machine, containers);
    let eligible = eligible_containers(local_machine, containers);
    let mut skipped = Vec::new();
    if let Some(container) = healthy
        .iter()
        .copied()
        .filter(|container| {
            let observation = container.as_observation();
            observation.service_name.as_str() == "caddy" && observation.machine_id == *local_machine
        })
        .max_by_key(|container| creation_key(container))
        && let Some(config) = container
            .as_observation()
            .resolved_spec
            .caddy_config
            .as_deref()
    {
        match render_custom_config(config, &container.as_observation().service_name, &eligible) {
            Ok(rendered) => {
                let candidate = automatic_caddyfile(
                    local_machine,
                    machine_name,
                    containers,
                    timestamp,
                    Some(&rendered),
                    certificates,
                );
                // TODO(UT-119): /adapt remains the only validation for custom Caddyfile
                // candidates and may accept a configuration that /load rejects.
                match admin.adapt(&candidate).await {
                    Ok(_) => output = candidate,
                    Err(error) => {
                        skipped.push(format!("Service 'caddy': validation failed: {error}"));
                    }
                }
            }
            Err(error) => skipped.push(format!("Service 'caddy': rendering failed: {error}")),
        }
    }

    let mut newest = BTreeMap::<&str, &ServiceContainer>::new();
    for container in &healthy {
        let service = container.as_observation().service_name.as_str();
        if service == "caddy" {
            continue;
        }
        newest
            .entry(service)
            .and_modify(|current| {
                if creation_key(container) > creation_key(current) {
                    *current = container;
                }
            })
            .or_insert(container);
    }
    for (service, container) in newest {
        let Some(config) = container
            .as_observation()
            .resolved_spec
            .caddy_config
            .as_deref()
        else {
            continue;
        };
        let rendered =
            match render_custom_config(config, &container.as_observation().service_name, &eligible)
            {
                Ok(rendered) => rendered,
                Err(error) => {
                    skipped.push(format!("Service '{service}': rendering failed: {error}"));
                    continue;
                }
            };
        let fragment = format!("\n# User-defined config for Service '{service}'.\n{rendered}\n");
        let candidate = format!("{output}{fragment}");
        match admin.adapt(&candidate).await {
            Ok(_) => output = candidate,
            Err(error) => skipped.push(format!("Service '{service}': validation failed: {error}")),
        }
    }
    if !skipped.is_empty() {
        output.push_str("\n# Skipped invalid user-defined configs:\n");
        for error in skipped {
            for (index, line) in error.lines().enumerate() {
                let _ = writeln!(output, "# {} {line}", if index == 0 { "-" } else { " " });
            }
        }
    }
    output
}

fn eligible_containers<'a>(
    local_machine: &MachineId,
    containers: &'a [ServiceContainer],
) -> Vec<&'a ServiceContainer> {
    healthy_containers(local_machine, containers)
        .into_iter()
        .filter(|container| container.as_observation().address.is_some())
        .collect()
}

fn healthy_containers<'a>(
    local_machine: &MachineId,
    containers: &'a [ServiceContainer],
) -> Vec<&'a ServiceContainer> {
    let mut containers = containers
        .iter()
        .filter(|container| {
            matches!(
                container.as_observation().runtime,
                ContainerRuntimeObservation::Running {
                    health: HealthObservation::Healthy | HealthObservation::NotConfigured
                }
            )
        })
        .collect::<Vec<_>>();
    containers.sort_by_key(|container| {
        let observation = container.as_observation();
        (
            observation.machine_id != *local_machine,
            observation.service_name.as_str(),
            observation.created_at_unix_nanos,
            observation.container_id.as_str(),
        )
    });
    containers
}

fn creation_key(container: &ServiceContainer) -> (i64, &str) {
    let observation = container.as_observation();
    (
        observation.created_at_unix_nanos,
        observation.container_id.as_str(),
    )
}

fn render_custom_config(
    template: &str,
    current_service: &ServiceName,
    containers: &[&ServiceContainer],
) -> Result<String, Error> {
    let mut rendered = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let expression = &remaining[start + 2..];
        let end = expression
            .find("}}")
            .ok_or_else(|| Error::Template("unclosed template expression".into()))?;
        let tokens = shell_words::split(expression[..end].trim())
            .map_err(|error| Error::Template(error.to_string()))?;
        let replacement = match tokens.as_slice() {
            [name] if name == ".Name" => current_service.as_str().to_owned(),
            [helper] if helper == "upstreams" => {
                upstreams(containers, current_service.as_str(), None)
            }
            [helper, argument] if helper == "upstreams" => match argument.parse::<u16>() {
                Ok(port) => upstreams(containers, current_service.as_str(), Some(port)),
                Err(_) => upstreams(
                    containers,
                    if argument == ".Name" {
                        current_service.as_str()
                    } else {
                        argument
                    },
                    None,
                ),
            },
            [helper, service, port] if helper == "upstreams" => {
                let service = if service == ".Name" {
                    current_service.as_str()
                } else {
                    service
                };
                let port = port
                    .parse::<u16>()
                    .map_err(|_| Error::Template(format!("invalid upstream port '{port}'")))?;
                upstreams(containers, service, Some(port))
            }
            _ => {
                return Err(Error::Template(format!(
                    "unsupported template expression '{{{{{}}}}}'",
                    expression[..end].trim()
                )));
            }
        };
        rendered.push_str(&replacement);
        remaining = &expression[end + 2..];
    }
    rendered.push_str(remaining);
    Ok(rendered)
}

fn upstreams(containers: &[&ServiceContainer], service: &str, port: Option<u16>) -> String {
    containers
        .iter()
        .map(|container| container.as_observation())
        .filter(|observation| observation.service_name.as_str() == service)
        .filter_map(|observation| observation.address)
        .map(|address| match port {
            Some(port) => format!("{}:{port}", address.0),
            None => address.0.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn automatic_caddyfile(
    local_machine: &MachineId,
    machine_name: &str,
    containers: &[ServiceContainer],
    timestamp: &str,
    global_config: Option<&str>,
    certificates: &BTreeMap<IngressHost, CertificateRow>,
) -> String {
    let containers = eligible_containers(local_machine, containers);
    let mut sites = BTreeMap::<IngressHost, Site<'_>>::new();
    for container in containers {
        let observation = container.as_observation();
        let address = observation.address.expect("address was filtered above");
        for port in &observation.resolved_spec.ports {
            let PortPublication::Ingress {
                hostname: IngressHostname::Explicit { hostname },
                container_port,
                http_protocol,
                ..
            } = port
            else {
                // TODO(UT-112): Caddy does not route L4 TCP/UDP ingress; host publication remains
                // the supported path. Assignment from the Cluster Domain is resolved before Caddy.
                continue;
            };
            let upstream = format!("{}:{container_port}", address.0);
            let site = sites.entry(hostname.clone()).or_default();
            match http_protocol {
                HttpProtocol::Http => site.http.push(upstream),
                HttpProtocol::Https => site.https.push(upstream),
            }
        }
    }
    for (hostname, row) in certificates {
        let site = sites.entry(hostname.clone()).or_default();
        site.challenge = row.challenge();
        site.material = row.material();
    }

    let mut output = format!(
        "# Caddyfile autogenerated by Ployz on Machine '{machine_name}' (DO NOT EDIT): {timestamp}\n\
# Automatically updated on Service or health status changes.\n\
# Docs: https://github.com/getployz/ployz2\n\
\n"
    );
    write_global_options(&mut output, global_config);
    let _ = write!(
        output,
        "# Health check endpoint to verify Caddy reachability on this Machine.\n\
http:// {{\n\
\thandle {CADDY_VERIFY_PATH} {{\n\
\t\trespond \"{local_machine}\" 200\n\
\t}}\n\
\tlog\n\
}}\n\
\n\
(common_proxy) {{\n\
\t# Retry failed requests up to lb_retries times against other available upstreams.\n\
\tlb_retries 3\n\
\t# Upstreams are marked unhealthy for fail_duration after a failed request (passive health checking).\n\
\tfail_duration 30s\n\
}}\n"
    );
    if sites.values().any(|site| {
        !site.http.is_empty()
            || site.challenge.is_some()
            || (!site.https.is_empty() && site.material.is_some())
    }) {
        output.push_str("\n# Sites generated from Service ports.\n");
    }
    for (hostname, site) in &sites {
        if !site.http.is_empty() || site.challenge.is_some() {
            write_site(
                &mut output,
                "http",
                hostname,
                &site.http,
                "",
                site.challenge,
            );
        }
        let Some(material) = site.material else {
            continue;
        };
        if site.https.is_empty() {
            continue;
        }
        let stem = certificate_file_stem(hostname, material);
        let tls =
            format!("\ttls {CONTAINER_CERTS_DIR}/{stem}.crt {CONTAINER_CERTS_DIR}/{stem}.key\n");
        write_site(&mut output, "https", hostname, &site.https, &tls, None);
    }
    write_certificate_errors(&mut output, certificates);
    output
}

fn write_certificate_errors(
    output: &mut String,
    certificates: &BTreeMap<IngressHost, CertificateRow>,
) {
    let mut header = false;
    for (hostname, row) in certificates {
        if row.material().is_some() {
            continue;
        }
        let Some(error) = row.last_error().filter(|error| !error.is_empty()) else {
            continue;
        };
        if !header {
            output.push_str("\n# Skipped certificate issuance:\n");
            header = true;
        }
        let _ = writeln!(output, "# - {hostname}: {error}");
    }
}

#[derive(Default)]
struct Site<'a> {
    http: Vec<String>,
    https: Vec<String>,
    challenge: Option<&'a CertificateChallenge>,
    material: Option<&'a CertificateMaterial>,
}

fn write_global_options(output: &mut String, global_config: Option<&str>) {
    // Caddy never issues certificates. The daemon pins material when it has any.
    match global_config {
        Some(user) => {
            let _ = writeln!(output, "# User-defined global config from Service 'caddy'.");
            output.push_str(&merge_auto_https(user));
            output.push_str("\n\n");
        }
        None => output.push_str("{\n\tauto_https off\n}\n\n"),
    }
}

fn merge_auto_https(user: &str) -> String {
    let trimmed = user.trim();
    if let Some(inner) = trimmed
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    {
        format!("{{\n\tauto_https off{inner}}}")
    } else {
        format!("{{\n\tauto_https off\n}}\n\n{user}")
    }
}

fn write_site(
    output: &mut String,
    protocol: &str,
    hostname: &IngressHost,
    upstreams: &[String],
    tls: &str,
    challenge: Option<&CertificateChallenge>,
) {
    let handle = challenge
        .map(|challenge| {
            format!(
                "\thandle /.well-known/acme-challenge/{} {{\n\t\trespond \"{}\" 200\n\t}}\n",
                challenge.token(),
                challenge.response()
            )
        })
        .unwrap_or_default();
    let proxy = if upstreams.is_empty() {
        String::new()
    } else {
        format!(
            "\treverse_proxy {} {{\n\t\timport common_proxy\n\t}}\n",
            upstreams.join(" ")
        )
    };
    let _ = write!(
        output,
        "\n{protocol}://{hostname} {{\n{tls}{handle}{proxy}\tlog\n}}\n"
    );
}

#[cfg(test)]
#[path = "caddy_tests.rs"]
mod tests;
