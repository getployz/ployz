use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use ployz_core::{
    ContainerKind, ContainerObservation, ContainerRuntimeObservation, HealthObservation,
    HttpProtocol, Machine, MachineId, PortPublication, ServiceName,
};
use reqwest::{Client, StatusCode, header};
use serde_json::Value;
use thiserror::Error;
use tokio::{net::UnixStream, sync::watch};

use crate::{
    corrosion::ReplicatedStore,
    filesystem::{atomic_write, set_ployz_group},
};

pub const CONFIG_FILE: &str = "Caddyfile";
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

#[async_trait]
trait CaddyAdmin: Send + Sync {
    async fn adapt(&self, caddyfile: &str) -> Result<String, Error>;
    async fn load(&self, json: &str) -> Result<(), Error>;
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

#[async_trait]
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
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    prepare_directory(
        config_file
            .parent()
            .ok_or_else(|| io::Error::other("Caddyfile has no parent"))?,
    )?;
    prepare_directory(
        admin_socket
            .parent()
            .ok_or_else(|| io::Error::other("Caddy admin socket has no parent"))?,
    )?;
    let mut changes = replicated
        .subscribe_container_changes()
        .await
        .map_err(io::Error::other)?;
    let mut last_applied = None;
    loop {
        match replicated.containers().await {
            Ok(containers) if last_applied.as_ref() != Some(&containers.observations) => {
                let admin = AdminClient::connect_if_available(&admin_socket)
                    .await
                    .map_err(io::Error::other)?;
                match reconcile(
                    &machine,
                    &containers.observations,
                    &config_file,
                    admin.as_ref().map(|admin| admin as &dyn CaddyAdmin),
                )
                .await
                {
                    Ok(()) => last_applied = Some(containers.observations),
                    Err(error) => {
                        last_applied = None;
                        eprintln!("failed to update Caddy configuration: {error}");
                    }
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!("failed to rebuild Caddy projection: {error}"),
        }
        tokio::select! {
            changed = changes.changed() => changed.map_err(io::Error::other)?,
            changed = shutdown.changed() => {
                changed.map_err(io::Error::other)?;
                return Ok(());
            }
        }
    }
}

async fn reconcile(
    machine: &Machine,
    observations: &[ContainerObservation],
    config_file: &Path,
    admin: Option<&dyn CaddyAdmin>,
) -> Result<(), Error> {
    // TODO(UT-116): keep the Caddy projection membership-blind until the membership model is
    // intentionally changed across replicated projections.
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let caddyfile = generate_caddyfile(
        &machine.id,
        machine.name.as_str(),
        observations,
        &timestamp,
        admin,
    )
    .await;
    if let Some(admin) = admin {
        let json = admin.adapt(&caddyfile).await?;
        admin.load(&json).await?;
    }
    write_caddyfile(config_file, &caddyfile)?;
    Ok(())
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

async fn generate_caddyfile(
    local_machine: &MachineId,
    machine_name: &str,
    observations: &[ContainerObservation],
    timestamp: &str,
    admin: Option<&dyn CaddyAdmin>,
) -> String {
    let mut output = automatic_caddyfile(local_machine, machine_name, observations, timestamp);
    let Some(admin) = admin else {
        output.push_str(
            "\n# User-defined Caddy configs are unavailable because Caddy's admin API is not reachable.\n\
# Run `ployz caddy logs` and `ployz inspect caddy` to troubleshoot.\n",
        );
        return output;
    };

    let healthy = healthy_containers(local_machine, observations);
    let eligible = eligible_containers(local_machine, observations);
    let mut skipped = Vec::new();
    if let Some(container) = newest_service(&healthy, "caddy", Some(local_machine))
        && let Some(config) = container.resolved_spec.caddy_config.as_deref()
    {
        match render_custom_config(config, &container.service_name, &eligible) {
            Ok(rendered) => {
                let fragment =
                    format!("# User-defined global config from Service 'caddy'.\n{rendered}\n");
                let candidate =
                    output.replacen("# Health check", &format!("{fragment}\n# Health check"), 1);
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

    let mut newest = BTreeMap::<&str, &ContainerObservation>::new();
    for container in &healthy {
        let service = container.service_name.as_str();
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
        let Some(config) = container.resolved_spec.caddy_config.as_deref() else {
            continue;
        };
        let rendered = match render_custom_config(config, &container.service_name, &eligible) {
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
            let _ = writeln!(output, "# - {error}");
        }
    }
    output
}

fn eligible_containers<'a>(
    local_machine: &MachineId,
    observations: &'a [ContainerObservation],
) -> Vec<&'a ContainerObservation> {
    healthy_containers(local_machine, observations)
        .into_iter()
        .filter(|container| container.address.is_some())
        .collect()
}

fn healthy_containers<'a>(
    local_machine: &MachineId,
    observations: &'a [ContainerObservation],
) -> Vec<&'a ContainerObservation> {
    let mut containers = observations
        .iter()
        .filter(|container| {
            container.kind == ContainerKind::ServiceContainer
                && matches!(
                    container.runtime,
                    ContainerRuntimeObservation::Running {
                        health: HealthObservation::Healthy | HealthObservation::NotConfigured
                    }
                )
        })
        .collect::<Vec<_>>();
    containers.sort_by_key(|container| {
        (
            container.machine_id != *local_machine,
            container.service_name.as_str(),
            container.created_at_unix_nanos,
            container.container_id.as_str(),
        )
    });
    containers
}

fn creation_key(container: &ContainerObservation) -> (i64, &str) {
    (
        container.created_at_unix_nanos,
        container.container_id.as_str(),
    )
}

fn newest_service<'a>(
    observations: &'a [&ContainerObservation],
    service: &str,
    machine: Option<&MachineId>,
) -> Option<&'a ContainerObservation> {
    observations
        .iter()
        .copied()
        .filter(|container| {
            container.service_name.as_str() == service
                && machine.is_none_or(|machine| container.machine_id == *machine)
        })
        .max_by_key(|container| creation_key(container))
}

fn render_custom_config(
    template: &str,
    current_service: &ServiceName,
    observations: &[&ContainerObservation],
) -> Result<String, Error> {
    let mut rendered = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let expression = &remaining[start + 2..];
        let end = expression
            .find("}}")
            .ok_or_else(|| Error::Template("unclosed template expression".into()))?;
        let tokens = template_tokens(expression[..end].trim())?;
        let replacement = match tokens.as_slice() {
            [name] if name == ".Name" => current_service.as_str().to_owned(),
            [helper] if helper == "upstreams" => {
                upstreams(observations, current_service.as_str(), None)
            }
            [helper, argument] if helper == "upstreams" => match argument.parse::<u16>() {
                Ok(port) => upstreams(observations, current_service.as_str(), Some(port)),
                Err(_) => upstreams(
                    observations,
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
                upstreams(observations, service, Some(port))
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

fn template_tokens(expression: &str) -> Result<Vec<String>, Error> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    for character in expression.chars() {
        match character {
            '"' => quoted = !quoted,
            character if character.is_whitespace() && !quoted => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            character => token.push(character),
        }
    }
    if quoted {
        return Err(Error::Template(
            "unclosed quote in template expression".into(),
        ));
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}

fn upstreams(observations: &[&ContainerObservation], service: &str, port: Option<u16>) -> String {
    observations
        .iter()
        .filter(|container| container.service_name.as_str() == service)
        .filter_map(|container| container.address)
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
    observations: &[ContainerObservation],
    timestamp: &str,
) -> String {
    let containers = eligible_containers(local_machine, observations);

    let mut http = BTreeMap::<&str, Vec<String>>::new();
    let mut https = BTreeMap::<&str, Vec<String>>::new();
    for container in containers {
        let address = container.address.expect("address was filtered above");
        for port in &container.resolved_spec.ports {
            let PortPublication::Ingress {
                hostname,
                container_port,
                http_protocol,
                ..
            } = port
            else {
                // TODO(UT-112): Caddy does not route L4 TCP/UDP ingress; host publication remains
                // the supported path.
                continue;
            };
            let upstream = format!("{}:{container_port}", address.0);
            match http_protocol {
                HttpProtocol::Http => http.entry(hostname).or_default().push(upstream),
                HttpProtocol::Https => https.entry(hostname).or_default().push(upstream),
            }
        }
    }

    let mut output = format!(
        "# Caddyfile autogenerated by Ployz on Machine '{machine_name}' (DO NOT EDIT): {timestamp}\n\
# Automatically updated on Service or health status changes.\n\
# Docs: https://github.com/getployz/ployz2\n\
\n\
# Health check endpoint to verify Caddy reachability on this Machine.\n\
http:// {{\n\
\thandle /.ployz-verify {{\n\
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
    if !http.is_empty() || !https.is_empty() {
        output.push_str("\n# Sites generated from Service ports.\n");
    }
    for (protocol, sites) in [("http", http), ("https", https)] {
        for (hostname, upstreams) in sites {
            let _ = write!(
                output,
                "\n{protocol}://{hostname} {{\n\
\treverse_proxy {} {{\n\
\t\timport common_proxy\n\
\t}}\n\
\tlog\n\
}}\n",
                upstreams.join(" ")
            );
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use ployz_core::{
        AdvertisedEndpoint, ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
        ContainerRuntimeObservation, HealthObservation, HostBind, HttpProtocol, Machine, MachineId,
        MachineName, MachineSubnet, ManagementAddress, PortPublication, ResolvedServiceSpec,
        ServiceId, ServiceName, TransportProtocol, WireGuardPublicKey,
    };
    use serde_json::json;

    use super::{
        CONFIG_FILE, CaddyAdmin, Error, automatic_caddyfile, generate_caddyfile, reconcile,
    };

    #[test]
    fn automatic_sites_match_the_frozen_caddyfile_contract() {
        let local = MachineId::parse("a".repeat(32)).unwrap();
        let remote = MachineId::parse("b".repeat(32)).unwrap();
        let observations = vec![
            observation(
                2,
                &remote,
                "api",
                Some([10, 210, 2, 2]),
                vec![ingress("example.com", 80, HttpProtocol::Http)],
            ),
            observation(
                1,
                &local,
                "api",
                Some([10, 210, 1, 2]),
                vec![
                    ingress("example.com", 80, HttpProtocol::Http),
                    ingress("secure.example.com", 8443, HttpProtocol::Https),
                ],
            ),
        ];

        assert_eq!(
            automatic_caddyfile(&local, "node-a", &observations, "TIMESTAMP"),
            format!(
                "# Caddyfile autogenerated by Ployz on Machine 'node-a' (DO NOT EDIT): TIMESTAMP\n\
# Automatically updated on Service or health status changes.\n\
# Docs: https://github.com/getployz/ployz2\n\
\n\
# Health check endpoint to verify Caddy reachability on this Machine.\n\
http:// {{\n\
\thandle /.ployz-verify {{\n\
\t\trespond \"{local}\" 200\n\
\t}}\n\
\tlog\n\
}}\n\
\n\
(common_proxy) {{\n\
\t# Retry failed requests up to lb_retries times against other available upstreams.\n\
\tlb_retries 3\n\
\t# Upstreams are marked unhealthy for fail_duration after a failed request (passive health checking).\n\
\tfail_duration 30s\n\
}}\n\
\n\
# Sites generated from Service ports.\n\
\n\
http://example.com {{\n\
\treverse_proxy 10.210.1.2:80 10.210.2.2:80 {{\n\
\t\timport common_proxy\n\
\t}}\n\
\tlog\n\
}}\n\
\n\
https://secure.example.com {{\n\
\treverse_proxy 10.210.1.2:8443 {{\n\
\t\timport common_proxy\n\
\t}}\n\
\tlog\n\
}}\n"
            )
        );
    }

    #[test]
    fn automatic_sites_omit_unaddressed_host_and_transport_ports() {
        let local = MachineId::parse("a".repeat(32)).unwrap();
        let ports = vec![
            PortPublication::Host {
                bind: HostBind::All,
                published_port: 80.try_into().unwrap(),
                container_port: 80.try_into().unwrap(),
                transport_protocol: TransportProtocol::Tcp,
            },
            PortPublication::IngressTransport {
                load_balancer_port: Some(53.try_into().unwrap()),
                container_port: 53.try_into().unwrap(),
                transport_protocol: TransportProtocol::Udp,
            },
        ];
        let observations = [
            observation(
                7,
                &local,
                "missing",
                None,
                vec![ingress("missing.example", 80, HttpProtocol::Http)],
            ),
            observation(8, &local, "transport", Some([10, 210, 1, 8]), ports),
        ];

        let caddyfile = automatic_caddyfile(&local, "node-a", &observations, "TIMESTAMP");
        assert!(caddyfile.contains("/.ployz-verify"));
        assert!(!caddyfile.contains("missing.example"));
        assert!(!caddyfile.contains("reverse_proxy"));
        assert!(
            serde_json::from_value::<PortPublication>(json!({
                "mode": "ingress",
                "hostname": "invalid.example",
                "load_balancer_port": 0,
                "container_port": 0,
                "http_protocol": "http"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn custom_configs_use_latest_specs_render_upstreams_and_isolate_failures() {
        let local = MachineId::parse("a".repeat(32)).unwrap();
        let remote = MachineId::parse("b".repeat(32)).unwrap();
        let mut external = custom_observation(
            7,
            1,
            &local,
            "external",
            "external.example { respond external }",
            [10, 210, 1, 7],
        );
        external.address = None;
        let observations = vec![
            custom_observation(
                1,
                1,
                &local,
                "caddy",
                "{\n\tadmin unix/{{upstreams \"api\"}}\n}",
                [10, 210, 1, 1],
            ),
            custom_observation(
                2,
                1,
                &local,
                "api",
                "old.example { respond old }",
                [10, 210, 1, 2],
            ),
            custom_observation(
                3,
                2,
                &remote,
                "api",
                "api.example { reverse_proxy {{upstreams}} }",
                [10, 210, 2, 2],
            ),
            custom_observation(
                4,
                1,
                &local,
                "gateway",
                "gateway.example { reverse_proxy {{upstreams \"api\" 9000}} }",
                [10, 210, 1, 4],
            ),
            custom_observation(
                5,
                1,
                &local,
                "invalid",
                "# invalid\ninvalid.example { respond bad }",
                [10, 210, 1, 5],
            ),
            custom_observation(
                6,
                1,
                &local,
                "web",
                "web.example { reverse_proxy {{upstreams 8080}} }",
                [10, 210, 1, 6],
            ),
            external,
        ];
        let admin = FakeAdmin::default();

        let caddyfile =
            generate_caddyfile(&local, "node-a", &observations, "TIMESTAMP", Some(&admin)).await;

        assert!(caddyfile.starts_with(
            "# Caddyfile autogenerated by Ployz on Machine 'node-a' (DO NOT EDIT): TIMESTAMP\n\
# Automatically updated on Service or health status changes.\n\
# Docs: https://github.com/getployz/ployz2\n\
\n\
# User-defined global config from Service 'caddy'.\n\
{\n\tadmin unix/10.210.1.2 10.210.2.2\n}\n\n"
        ));
        assert!(caddyfile.contains(
            "# User-defined config for Service 'api'.\n\
api.example { reverse_proxy 10.210.1.2 10.210.2.2 }"
        ));
        assert!(!caddyfile.contains("old.example"));
        assert!(caddyfile.contains(
            "# User-defined config for Service 'gateway'.\n\
gateway.example { reverse_proxy 10.210.1.2:9000 10.210.2.2:9000 }"
        ));
        assert!(caddyfile.contains(
            "# Skipped invalid user-defined configs:\n\
# - Service 'invalid': validation failed: invalid config detected\n"
        ));
        assert!(caddyfile.contains(
            "# User-defined config for Service 'web'.\n\
web.example { reverse_proxy 10.210.1.6:8080 }"
        ));
        assert!(caddyfile.contains("external.example { respond external }"));
        assert_eq!(admin.adapted.lock().unwrap().len(), 6);
    }

    #[tokio::test]
    async fn unavailable_caddy_omits_every_custom_config() {
        let local = MachineId::parse("a".repeat(32)).unwrap();
        let observations = [custom_observation(
            1,
            1,
            &local,
            "api",
            "custom.example { respond custom }",
            [10, 210, 1, 1],
        )];

        let caddyfile = generate_caddyfile(&local, "node-a", &observations, "TIME", None).await;
        assert!(!caddyfile.contains("custom.example"));
        assert!(caddyfile.contains("admin API is not reachable"));
    }

    #[tokio::test]
    async fn broken_global_template_does_not_hide_valid_service_configs() {
        let local = MachineId::parse("a".repeat(32)).unwrap();
        let observations = [
            custom_observation(1, 1, &local, "caddy", "{{unknown}}", [10, 210, 1, 1]),
            custom_observation(
                2,
                1,
                &local,
                "api",
                "api.example { respond ok }",
                [10, 210, 1, 2],
            ),
        ];

        let caddyfile = generate_caddyfile(
            &local,
            "node-a",
            &observations,
            "TIME",
            Some(&FakeAdmin::default()),
        )
        .await;
        assert!(caddyfile.contains("Service 'caddy': rendering failed"));
        assert!(caddyfile.contains("api.example { respond ok }"));
    }

    #[tokio::test]
    async fn failed_load_preserves_the_last_caddyfile() {
        let directory =
            std::env::temp_dir().join(format!("ployz-caddy-load-test-{}", std::process::id()));
        let path = directory.join(CONFIG_FILE);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, "last loaded").unwrap();
        let machine = Machine {
            id: MachineId::parse("a".repeat(32)).unwrap(),
            name: MachineName::parse("node-a").unwrap(),
            subnet: MachineSubnet("10.210.1.0/24".parse().unwrap()),
            management_address: ManagementAddress("fdcc::1".parse().unwrap()),
            public_key: WireGuardPublicKey([1; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51000".parse().unwrap())],
            runtime: Default::default(),
        };
        let admin = FakeAdmin {
            fail_load: true,
            ..FakeAdmin::default()
        };

        assert!(reconcile(&machine, &[], &path, Some(&admin)).await.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "last loaded");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[derive(Default)]
    struct FakeAdmin {
        adapted: Mutex<Vec<String>>,
        fail_load: bool,
    }

    #[async_trait]
    impl CaddyAdmin for FakeAdmin {
        async fn adapt(&self, caddyfile: &str) -> Result<String, Error> {
            self.adapted.lock().unwrap().push(caddyfile.into());
            if caddyfile.contains("# invalid") {
                Err(Error::Admin("invalid config detected".into()))
            } else {
                Ok("{}".into())
            }
        }

        async fn load(&self, _json: &str) -> Result<(), Error> {
            if self.fail_load {
                Err(Error::Admin("load failed".into()))
            } else {
                Ok(())
            }
        }
    }

    fn ingress(hostname: &str, port: u16, http_protocol: HttpProtocol) -> PortPublication {
        PortPublication::Ingress {
            hostname: hostname.into(),
            load_balancer_port: port.try_into().unwrap(),
            container_port: port.try_into().unwrap(),
            http_protocol,
        }
    }

    fn observation(
        suffix: u8,
        machine_id: &MachineId,
        service_name: &str,
        address: Option<[u8; 4]>,
        ports: Vec<PortPublication>,
    ) -> ContainerObservation {
        let service_id = ServiceId::parse(format!("{suffix:x}").repeat(32)).unwrap();
        let service_name = ServiceName::parse(service_name).unwrap();
        let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "example.test/image", "pull_policy": "missing" },
            "ports": ports,
        }))
        .unwrap();
        ContainerObservation {
            container_id: ContainerId::parse(format!("{suffix:x}").repeat(64)).unwrap(),
            display_name: format!("{service_name}-{suffix}"),
            created_at_unix_nanos: 0,
            machine_id: machine_id.clone(),
            service_id,
            service_name,
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec,
            address: address.map(|address| ContainerAddress(address.into())),
            labels: BTreeMap::new(),
        }
    }

    fn custom_observation(
        suffix: u8,
        created_at_unix_nanos: i64,
        machine_id: &MachineId,
        service_name: &str,
        caddy_config: &str,
        address: [u8; 4],
    ) -> ContainerObservation {
        let mut observation =
            observation(suffix, machine_id, service_name, Some(address), Vec::new());
        observation.created_at_unix_nanos = created_at_unix_nanos;
        observation.resolved_spec.caddy_config = Some(caddy_config.into());
        observation
    }
}
