//! Docker-in-Docker support for the ignored Layer 3 parity suite.

pub mod fake_acme;
mod machine_admin;

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    io,
    net::{Ipv4Addr, TcpListener},
    process::{Command, Output},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use ployz_core::{
    AdvertisedEndpoint, ContainerId, DescribeContractRequest, DockerVolumeName, InitializeRequest,
    InspectRequest, JoinRequest, ListImagesRequest, LocalMachinePhase, Machine, MachineName,
    MachineRpcClient, MembershipObservation, OpaquePayload, Registered, ResetRequest, RpcResponse,
    RpcResponseBody, op,
};
use thiserror::Error;

pub const IMAGE: &str = "ghcr.io/getployz/ployz2-testkit:main";
pub const CORROSION_IMAGE: &str = "ghcr.io/unlabs-dev/corrosion:2026.6.15";
pub const SERVICE_CONTAINER_IMAGE: &str = "alpine:3.23.3";
pub const CADDY_IMAGE: &str = "caddy:2.10.2";
pub const UNREGISTRY_IMAGE: &str = "ghcr.io/psviderski/unregistry:0.4.1";
pub const OWNER_LABEL: &str = "dev.ployz.testkit";
pub const CLUSTER_LABEL: &str = "dev.ployz.testkit.cluster";
pub const MACHINE_API_PORT: u16 = 51000;
const HOST_ENTRY_API_PORT: u16 = 51003;
static RESERVED_PORTS: LazyLock<Mutex<BTreeSet<u16>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageTarget {
    pub platform: &'static str,
    pub requires: [&'static str; 7],
}

#[must_use]
pub fn image_targets() -> [ImageTarget; 2] {
    let requires = [
        "ployzd",
        "dockerd",
        "wg",
        CORROSION_IMAGE,
        SERVICE_CONTAINER_IMAGE,
        CADDY_IMAGE,
        UNREGISTRY_IMAGE,
    ];
    [
        ImageTarget {
            platform: "linux/amd64",
            requires,
        },
        ImageTarget {
            platform: "linux/arm64",
            requires,
        },
    ]
}

/// Per-Machine knobs that are not derived from the Cluster name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachinePlan {
    pub api_port: u16,
    pub environment: BTreeMap<String, String>,
    pub daemon_args: Vec<String>,
}

/// Planned Docker-in-Docker Cluster.
///
/// Network name, labels, Machine container names, and privileged are derived
/// from the Cluster name. [`Cluster`] is the running Cluster and tears it down
/// on [`Drop`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClusterPlan {
    name: String,
    pub machines: Vec<MachinePlan>,
}

impl ClusterPlan {
    /// Plan a Cluster whose Docker identity is derived from `name`.
    ///
    /// # Errors
    ///
    /// Returns [`TestkitError::InvalidClusterName`] when `name` is empty or
    /// contains a byte that is not ASCII alphanumeric or `-`.
    /// Returns [`TestkitError::PortAllocation`] when a Machine API port cannot
    /// be reserved.
    pub fn new(name: &str, machine_count: usize) -> Result<Self, TestkitError> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(TestkitError::InvalidClusterName(name.to_owned()));
        }
        let machines = (0..machine_count)
            .map(|_| {
                Ok(MachinePlan {
                    api_port: reserve_loopback_port()?,
                    environment: BTreeMap::from([(
                        "PLOYZ_ACME_DIRECTORY".to_owned(),
                        String::new(),
                    )]),
                    daemon_args: Vec::new(),
                })
            })
            .collect::<Result<_, io::Error>>()
            .map_err(TestkitError::PortAllocation)?;
        Ok(Self {
            name: name.to_owned(),
            machines,
        })
    }

    /// Cluster name the rest of the Docker identity is derived from.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Docker network name: `ployz-testkit-{name}`.
    #[must_use]
    pub fn network(&self) -> String {
        format!("ployz-testkit-{}", self.name)
    }

    /// Owner and Cluster labels derived from the Cluster name.
    #[must_use]
    pub fn labels(&self) -> BTreeMap<String, String> {
        labels(&self.name)
    }

    /// Docker container name for the Machine at `index`: `ployz-testkit-{name}-{index}`.
    #[must_use]
    pub fn machine_name(&self, index: usize) -> String {
        format!("ployz-testkit-{}-{index}", self.name)
    }

    #[must_use]
    pub fn teardown_filters(&self) -> [String; 2] {
        [
            format!("label={OWNER_LABEL}=true"),
            format!("label={CLUSTER_LABEL}={}", self.name),
        ]
    }
}

#[must_use]
pub fn join_request(first: &Machine, registration: &Registered) -> JoinRequest {
    let mut visible_peers = registration.visible_peers.clone();
    if !visible_peers.iter().any(|machine| machine.id == first.id) {
        visible_peers.push(first.clone());
    }
    JoinRequest {
        registration: Registered {
            visible_peers,
            ..registration.clone()
        },
        wireguard_mtu: None,
        cloud_pairing: None,
    }
}

fn labels(cluster: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (OWNER_LABEL.to_owned(), "true".to_owned()),
        (CLUSTER_LABEL.to_owned(), cluster.to_owned()),
    ])
}

fn reserve_loopback_port() -> io::Result<u16> {
    loop {
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        if RESERVED_PORTS
            .lock()
            .map_err(|_| io::Error::other("testkit port reservation lock poisoned"))?
            .insert(port)
        {
            return Ok(port);
        }
    }
}

pub struct Cluster {
    plan: ClusterPlan,
}

impl Cluster {
    pub fn create(plan: ClusterPlan) -> Result<Self, TestkitError> {
        let labels = plan.labels();
        let network = plan.network();
        let mut network_args = vec!["network".to_owned(), "create".to_owned()];
        for (key, value) in &labels {
            network_args.extend(["--label".to_owned(), format!("{key}={value}")]);
        }
        network_args.push(network.clone());
        docker(network_args)?;
        let cluster = Self { plan };
        let image = std::env::var("PLOYZ_TESTKIT_IMAGE").unwrap_or_else(|_| IMAGE.into());
        for (index, machine) in cluster.plan.machines.iter().enumerate() {
            let mut args = vec![
                "run".to_owned(),
                "--detach".to_owned(),
                "--privileged".to_owned(),
                "--name".to_owned(),
                cluster.plan.machine_name(index),
                "--network".to_owned(),
                network.clone(),
                "--add-host".to_owned(),
                "host.docker.internal:host-gateway".to_owned(),
            ];
            for (key, value) in &labels {
                args.extend(["--label".to_owned(), format!("{key}={value}")]);
            }
            for (key, value) in &machine.environment {
                args.extend(["--env".to_owned(), format!("{key}={value}")]);
            }
            args.extend([
                "--env".to_owned(),
                format!("PLOYZ_TESTKIT_HOST_API_PORT={HOST_ENTRY_API_PORT}"),
                "--publish".to_owned(),
                format!("127.0.0.1:{}:{HOST_ENTRY_API_PORT}", machine.api_port),
                image.clone(),
            ]);
            args.extend(machine.daemon_args.clone());
            if let Err(error) = docker(args) {
                let _ = cluster.teardown();
                return Err(error);
            }
        }
        Ok(cluster)
    }

    pub async fn wait_ready(&self, timeout: Duration) -> Result<(), TestkitError> {
        let deadline = Instant::now() + timeout;
        for index in 0..self.plan.machines.len() {
            loop {
                let ready = match self.client(index).await {
                    Ok(mut client) => client
                        .describe_contract(
                            op::DescribeContract::into_request(DescribeContractRequest {})
                                .encode()?,
                        )
                        .await
                        .is_ok(),
                    Err(_) => false,
                };
                if ready {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(TestkitError::ReadinessTimeout(self.container_name(index)?));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        Ok(())
    }

    pub async fn initialize_two(&self) -> Result<[Machine; 2], TestkitError> {
        if self.plan.machines.len() != 2 {
            return Err(TestkitError::MachineCount {
                expected: 2,
                actual: self.plan.machines.len(),
            });
        }
        Ok(self
            .initialize_all()
            .await?
            .try_into()
            .expect("checked length"))
    }

    pub async fn initialize_three(&self) -> Result<[Machine; 3], TestkitError> {
        if self.plan.machines.len() != 3 {
            return Err(TestkitError::MachineCount {
                expected: 3,
                actual: self.plan.machines.len(),
            });
        }
        Ok(self
            .initialize_all()
            .await?
            .try_into()
            .expect("checked length"))
    }

    async fn prepare_entry_machine(&self) -> Result<Machine, TestkitError> {
        self.wait_ready(Duration::from_secs(30)).await?;
        let initialized = self.initialize_first().await?;
        let first = self
            .wait_phase(0, LocalMachinePhase::Participating, Duration::from_secs(60))
            .await?
            .machine
            .ok_or_else(|| TestkitError::Rpc("initialized Machine record is missing".into()))?;
        if first != initialized {
            return Err(TestkitError::Rpc(
                "initialized Machine changed across restart".into(),
            ));
        }
        self.wait_machine_count(0, 1, Duration::from_secs(60))
            .await?;
        Ok(first)
    }

    async fn initialize_all(&self) -> Result<Vec<Machine>, TestkitError> {
        let first = self.prepare_entry_machine().await?;
        let mut machines = vec![first];
        for index in 1..self.plan.machines.len() {
            machines.push(
                self.add_machine(0, index, &format!("machine-{}", index + 1))
                    .await?,
            );
            for entry in 0..=index {
                self.wait_machine_count(entry, index + 1, Duration::from_secs(60))
                    .await?;
            }
        }
        Ok(machines)
    }

    /// Initialize two Machines and prove a joining Machine does not serve
    /// store-dependent data below its frontier.
    ///
    /// # Errors
    ///
    /// Returns if the Cluster is not two Machines, a join RPC fails, or the
    /// frontier barrier is violated.
    pub async fn initialize_two_with_join_barrier(&self) -> Result<[Machine; 2], TestkitError> {
        if self.plan.machines.len() != 2 {
            return Err(TestkitError::MachineCount {
                expected: 2,
                actual: self.plan.machines.len(),
            });
        }
        let first = self.prepare_entry_machine().await?;
        self.seed_observation(0)?;
        let token = self.machine_token(1, vec![self.endpoint(1)?]).await?;
        let registered = self.register_machine(0, "machine-2", token).await?;
        self.gossip_rule(1, "--insert")?;
        self.join(1, join_request(&first, &registered)).await?;
        self.wait_for_join_barrier(1, &registered).await?;
        self.wait_machine_count(0, 2, Duration::from_secs(60))
            .await?;
        self.wait_machine_count(1, 2, Duration::from_secs(60))
            .await?;
        Ok([first, registered.assigned_machine])
    }

    async fn wait_for_join_barrier(
        &self,
        index: usize,
        registered: &Registered,
    ) -> Result<(), TestkitError> {
        self.wait_phase(index, LocalMachinePhase::Joining, Duration::from_secs(60))
            .await?;
        if self.inspect(index).await.is_ok_and(|details| {
            version_reached(&details.store_version, &registered.target_versions)
        }) {
            return Err(TestkitError::Rpc(
                "joining Machine reached its frontier while gossip was blocked".into(),
            ));
        }
        if self.machines(index).await.is_ok() {
            return Err(TestkitError::Rpc(
                "joining Machine served store-dependent data below its frontier".into(),
            ));
        }
        self.insert_known_gap(
            index,
            registered.target_versions.keys().next().ok_or_else(|| {
                TestkitError::Rpc("registration returned an empty join frontier".into())
            })?,
        )?;
        self.gossip_rule(index, "--delete")?;
        self.wait_store_version(index, &registered.target_versions, Duration::from_secs(120))
            .await?;
        if self.machines(index).await.is_ok() {
            return Err(TestkitError::Rpc(
                "joining Machine served store-dependent data while a known gap remained".into(),
            ));
        }
        self.clear_known_gaps(index)?;
        self.wait_phase(
            index,
            LocalMachinePhase::Participating,
            Duration::from_secs(120),
        )
        .await?;
        self.read_seeded_observation(index)
    }

    pub async fn initialize_first(&self) -> Result<Machine, TestkitError> {
        let mut client = self.client(0).await?;
        Ok(response(
            client
                .initialize(
                    op::Initialize::into_request(InitializeRequest {
                        name: MachineName::parse("machine-1")?,
                        cluster_network: "10.210.0.0/16".parse().expect("static network is valid"),
                        public_ip: None,
                        advertised_endpoints: vec![self.endpoint(0)?],
                        wireguard_mtu: None,
                        cloud_pairing: None,
                    })
                    .encode()?,
                )
                .await?
                .into_inner(),
        )?
        .decode::<op::Initialize>()?
        .machine)
    }

    pub async fn join(&self, index: usize, request: JoinRequest) -> Result<(), TestkitError> {
        let mut client = self.client(index).await?;
        response(
            client
                .join(op::Join::into_request(request).encode()?)
                .await?
                .into_inner(),
        )?
        .decode::<op::Join>()
        .map(|_| ())
        .map_err(Into::into)
    }

    fn seed_observation(&self, index: usize) -> Result<(), TestkitError> {
        self.corrosion_transaction(
            index,
            r#"[{"query":"INSERT INTO cluster (key, value, updated_at) VALUES ('l3_seed', ?, datetime('now')) ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at","params":["seeded"]}]"#,
        )
    }

    fn read_seeded_observation(&self, index: usize) -> Result<(), TestkitError> {
        let output = self.corrosion_request(
            index,
            "v1/queries",
            r#"{"query":"SELECT value FROM cluster WHERE key = 'l3_seed'","params":[]}"#,
        )?;
        if String::from_utf8_lossy(&output.stdout).contains("seeded") {
            Ok(())
        } else {
            Err(TestkitError::Rpc(
                "joining Machine did not replicate the seeded observation".into(),
            ))
        }
    }

    fn insert_known_gap(&self, index: usize, actor: &str) -> Result<(), TestkitError> {
        if !actor.len().is_multiple_of(2) {
            return Err(TestkitError::Rpc(
                "invalid actor ID in join frontier".into(),
            ));
        }
        let bytes = actor
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .map(|byte| byte.to_string())
                    .ok_or_else(|| TestkitError::Rpc("invalid actor ID in join frontier".into()))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(",");
        self.corrosion_transaction(
            index,
            &format!(
                r#"[{{"query":"INSERT INTO __corro_bookkeeping_gaps (actor_id, start, end) VALUES (?, 1, 1)","params":[[{bytes}]]}}]"#,
            ),
        )
    }

    fn clear_known_gaps(&self, index: usize) -> Result<(), TestkitError> {
        self.corrosion_transaction(
            index,
            r#"[{"query":"DELETE FROM __corro_bookkeeping_gaps","params":[]}]"#,
        )
    }

    fn corrosion_transaction(&self, index: usize, payload: &str) -> Result<(), TestkitError> {
        let output = self.corrosion_request(index, "v1/transactions", payload)?;
        if String::from_utf8_lossy(&output.stdout).contains("\"error\"") {
            Err(TestkitError::Rpc(format!(
                "Corrosion transaction failed: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            )))
        } else {
            Ok(())
        }
    }

    fn corrosion_request(
        &self,
        index: usize,
        path: &str,
        payload: &str,
    ) -> Result<Output, TestkitError> {
        let script = format!(
            "token=$(cat /var/lib/ployz/corrosion/.api-token); curl --fail --silent --show-error --http2-prior-knowledge -H \"Authorization: Bearer $token\" -H 'Content-Type: application/json' --data-binary {} http://127.0.0.1:51002/{path}",
            shell_quote(payload)
        );
        let output = docker_output(["exec", &self.container_name(index)?, "sh", "-c", &script])?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(command_error(output))
        }
    }

    fn gossip_rule(&self, index: usize, action: &str) -> Result<(), TestkitError> {
        docker([
            "exec",
            &self.container_name(index)?,
            "ip6tables",
            action,
            "OUTPUT",
            "--protocol",
            "udp",
            "--destination-port",
            "51001",
            "--jump",
            "DROP",
        ])
    }

    pub fn block_gossip(&self, index: usize) -> Result<(), TestkitError> {
        self.gossip_rule(index, "--insert")
    }

    pub fn unblock_gossip(&self, index: usize) -> Result<(), TestkitError> {
        self.gossip_rule(index, "--delete")
    }

    pub fn write_volume_data(
        &self,
        index: usize,
        name: &DockerVolumeName,
        value: &str,
    ) -> Result<(), TestkitError> {
        docker([
            "exec",
            &self.container_name(index)?,
            "sh",
            "-c",
            &format!(
                "printf %s {} > /var/lib/docker/volumes/{}/_data/value",
                shell_quote(value),
                name
            ),
        ])
    }

    pub fn stop(&self, index: usize) -> Result<(), TestkitError> {
        docker(["stop", &self.container_name(index)?])
    }

    pub fn api_socket_address(&self, index: usize) -> Result<std::net::SocketAddr, TestkitError> {
        Ok((Ipv4Addr::LOCALHOST, self.machine(index)?.api_port).into())
    }

    pub fn prepare_bind(&self, index: usize, path: &str, value: &str) -> Result<(), TestkitError> {
        let path = shell_quote(path);
        docker([
            "exec",
            &self.container_name(index)?,
            "sh",
            "-c",
            &format!(
                "mkdir -p {path} && printf %s {} > {path}/value",
                shell_quote(value)
            ),
        ])
    }

    pub fn start_container(
        &self,
        index: usize,
        container_id: &ContainerId,
    ) -> Result<(), TestkitError> {
        docker([
            "exec",
            &self.container_name(index)?,
            "docker",
            "start",
            container_id.as_str(),
        ])
    }

    pub fn read_container_file(
        &self,
        index: usize,
        container_id: &ContainerId,
        path: &str,
    ) -> Result<String, TestkitError> {
        let output = docker_output([
            "exec",
            &self.container_name(index)?,
            "docker",
            "exec",
            container_id.as_str(),
            "cat",
            path,
        ])?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(command_error(output))
        }
    }

    pub async fn images(
        &self,
        index: usize,
        reference: Option<String>,
    ) -> Result<ployz_core::MachineImages, TestkitError> {
        let mut client = self.client(index).await?;
        Ok(response(
            client
                .list_images(
                    op::ListImages::into_request(ListImagesRequest { reference }).encode()?,
                )
                .await?
                .into_inner(),
        )?
        .decode::<op::ListImages>()?)
    }

    pub async fn reset(&self, index: usize) -> Result<(), TestkitError> {
        let mut client = self.client(index).await?;
        response(
            client
                .reset(op::Reset::into_request(ResetRequest {}).encode()?)
                .await?
                .into_inner(),
        )?
        .decode::<op::Reset>()?;
        Ok(())
    }

    pub fn docker(&self, index: usize, args: &[&str]) -> Result<(), TestkitError> {
        let name = self.container_name(index)?;
        let mut command = vec!["exec", name.as_str(), "docker"];
        command.extend_from_slice(args);
        docker(command)
    }

    pub fn shell(&self, index: usize, script: &str) -> Result<Output, TestkitError> {
        let name = self.container_name(index)?;
        let output = docker_output(["exec", name.as_str(), "sh", "-c", script])?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(command_error(output))
        }
    }

    pub fn container_mounts(
        &self,
        index: usize,
        container_id: &ContainerId,
    ) -> Result<String, TestkitError> {
        let output = docker_output([
            "exec",
            &self.container_name(index)?,
            "docker",
            "inspect",
            "--format",
            "{{json .Mounts}}",
            container_id.as_str(),
        ])?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(command_error(output))
        }
    }

    pub fn remove_container(
        &self,
        index: usize,
        container_id: &ContainerId,
    ) -> Result<(), TestkitError> {
        docker([
            "exec",
            &self.container_name(index)?,
            "docker",
            "rm",
            "--force",
            container_id.as_str(),
        ])
    }

    pub fn logs(&self, index: usize) -> Result<String, TestkitError> {
        let name = self.container_name(index)?;
        let output = docker_output(["logs", name.as_str()])?;
        Ok(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }

    pub fn restart(&self, index: usize) -> Result<(), TestkitError> {
        docker(["restart", &self.container_name(index)?])
    }

    pub fn remote_machine_api_rule(
        &self,
        entry_index: usize,
        action: &str,
    ) -> Result<(), TestkitError> {
        docker([
            "exec",
            &self.container_name(entry_index)?,
            "ip6tables",
            action,
            "OUTPUT",
            "--protocol",
            "tcp",
            "--destination-port",
            &MACHINE_API_PORT.to_string(),
            "--jump",
            "REJECT",
        ])
    }

    pub fn api_address(&self, index: usize) -> Result<String, TestkitError> {
        Ok(format!("tcp://127.0.0.1:{}", self.machine(index)?.api_port))
    }

    pub fn machine_shell(&self, index: usize, script: &str) -> Result<String, TestkitError> {
        let output = docker_output(["exec", &self.container_name(index)?, "sh", "-c", script])?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(command_error(output))
        }
    }

    pub fn container_network_shell(
        &self,
        machine_index: usize,
        container_id: &ContainerId,
        script: &str,
    ) -> Result<String, TestkitError> {
        self.machine_shell(
            machine_index,
            &format!(
                "pid=$(docker inspect --format '{{{{.State.Pid}}}}' {container_id}); nsenter --target \"$pid\" --net sh -c {}",
                shell_quote(script)
            ),
        )
    }

    async fn wait_phase(
        &self,
        index: usize,
        phase: LocalMachinePhase,
        timeout: Duration,
    ) -> Result<ployz_core::MachineDetails, TestkitError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(details) = self.inspect(index).await
                && details.phase == phase
            {
                return Ok(details);
            }
            if Instant::now() >= deadline {
                return Err(TestkitError::ReadinessTimeout(self.container_name(index)?));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn wait_machine_count(
        &self,
        index: usize,
        count: usize,
        timeout: Duration,
    ) -> Result<(), TestkitError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(machines) = self.machines(index).await
                && machines.len() == count
                && machines
                    .iter()
                    .all(|machine| machine.membership == MembershipObservation::Up)
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestkitError::ReadinessTimeout(self.container_name(index)?));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn wait_store_version(
        &self,
        index: usize,
        target: &BTreeMap<String, i64>,
        timeout: Duration,
    ) -> Result<(), TestkitError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self
                .inspect(index)
                .await
                .is_ok_and(|details| version_reached(&details.store_version, target))
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestkitError::ReadinessTimeout(self.container_name(index)?));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn inspect(&self, index: usize) -> Result<ployz_core::MachineDetails, TestkitError> {
        let mut client = self.client(index).await?;
        Ok(response(
            client
                .inspect(op::Inspect::into_request(InspectRequest::default()).encode()?)
                .await?
                .into_inner(),
        )?
        .decode::<op::Inspect>()?)
    }

    async fn client(
        &self,
        index: usize,
    ) -> Result<MachineRpcClient<tonic::transport::Channel>, TestkitError> {
        let port = self.machine(index)?.api_port;
        MachineRpcClient::connect(format!("http://127.0.0.1:{port}"))
            .await
            .map_err(|error| TestkitError::Rpc(error.to_string()))
    }

    pub fn endpoint(&self, index: usize) -> Result<AdvertisedEndpoint, TestkitError> {
        let name = self.container_name(index)?;
        let output = docker_output([
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            &name,
        ])?;
        if !output.status.success() {
            return Err(command_error(output));
        }
        let address = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .map_err(|_| TestkitError::Rpc("Docker returned an invalid Machine IP".into()))?;
        Ok(AdvertisedEndpoint(std::net::SocketAddr::new(
            address, 51820,
        )))
    }

    fn machine(&self, index: usize) -> Result<&MachinePlan, TestkitError> {
        self.plan
            .machines
            .get(index)
            .ok_or(TestkitError::MachineIndex(index))
    }

    fn container_name(&self, index: usize) -> Result<String, TestkitError> {
        self.machine(index)?;
        Ok(self.plan.machine_name(index))
    }

    pub fn teardown(&self) -> Result<(), TestkitError> {
        let filters = self.plan.teardown_filters();
        let listed = docker_output([
            "ps",
            "--all",
            "--quiet",
            "--filter",
            &filters[0],
            "--filter",
            &filters[1],
        ])?;
        for id in String::from_utf8_lossy(&listed.stdout).split_whitespace() {
            docker(["rm", "--force", "--volumes", id])?;
        }
        let network = self.plan.network();
        match docker_output(["network", "rm", &network]) {
            Ok(output)
                if output.status.success()
                    || String::from_utf8_lossy(&output.stderr).contains("not found") =>
            {
                if let Ok(mut ports) = RESERVED_PORTS.lock() {
                    for machine in &self.plan.machines {
                        ports.remove(&machine.api_port);
                    }
                }
                Ok(())
            }
            Ok(output) => Err(command_error(output)),
            Err(error) => Err(error),
        }
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

fn docker<I, S>(args: I) -> Result<(), TestkitError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = docker_output(args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error(output))
    }
}

fn docker_output<I, S>(args: I) -> Result<Output, TestkitError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("docker")
        .args(args)
        .output()
        .map_err(TestkitError::DockerIo)
}

fn command_error(output: Output) -> TestkitError {
    TestkitError::DockerCommand(String::from_utf8_lossy(&output.stderr).trim().to_owned())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn version_reached(current: &BTreeMap<String, i64>, target: &BTreeMap<String, i64>) -> bool {
    target
        .iter()
        .all(|(actor, version)| current.get(actor).is_some_and(|current| current >= version))
}

#[derive(Debug, Error)]
pub enum TestkitError {
    #[error("invalid Cluster name {0:?}")]
    InvalidClusterName(String),
    #[error("could not allocate a loopback API port: {0}")]
    PortAllocation(io::Error),
    #[error("Docker command could not run: {0}")]
    DockerIo(io::Error),
    #[error("Docker command failed: {0}")]
    DockerCommand(String),
    #[error("Machine {0} did not become ready")]
    ReadinessTimeout(String),
    #[error("expected {expected} Machines, found {actual}")]
    MachineCount { expected: usize, actual: usize },
    #[error("Machine index {0} is out of range")]
    MachineIndex(usize),
    #[error("Machine API failed: {0}")]
    Rpc(String),
}

impl From<ployz_core::CodecError> for TestkitError {
    fn from(error: ployz_core::CodecError) -> Self {
        Self::Rpc(error.to_string())
    }
}

impl From<ployz_core::ValueError> for TestkitError {
    fn from(error: ployz_core::ValueError) -> Self {
        Self::Rpc(error.to_string())
    }
}

impl From<tonic::Status> for TestkitError {
    fn from(error: tonic::Status) -> Self {
        Self::Rpc(error.to_string())
    }
}

fn response(payload: OpaquePayload) -> Result<RpcResponse, TestkitError> {
    let response = payload.decode_response()?;
    if let RpcResponseBody::Error(error) = &response.body {
        Err(TestkitError::Rpc(error.message.clone()))
    } else {
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_two_isolated_privileged_machines() {
        let plan = ClusterPlan::new("l3-001", 2).unwrap();
        assert_eq!(plan.name(), "l3-001");
        assert_eq!(plan.network(), "ployz-testkit-l3-001");
        assert_eq!(plan.machine_name(0), "ployz-testkit-l3-001-0");
        assert_eq!(plan.machine_name(1), "ployz-testkit-l3-001-1");
        assert_eq!(plan.machines.len(), 2);
        let [first, second] = plan.machines.as_slice() else {
            panic!("expected two Machines")
        };
        assert_ne!(first.api_port, second.api_port);
        assert_eq!(
            plan.labels(),
            BTreeMap::from([
                (OWNER_LABEL.to_owned(), "true".to_owned()),
                (CLUSTER_LABEL.to_owned(), "l3-001".to_owned()),
            ])
        );
        assert!(plan.machines.iter().all(|machine| {
            machine.environment.get("PLOYZ_ACME_DIRECTORY") == Some(&String::new())
        }));
        assert_eq!(
            plan.teardown_filters(),
            [
                "label=dev.ployz.testkit=true",
                "label=dev.ployz.testkit.cluster=l3-001",
            ]
        );

        let targets = image_targets();
        assert_eq!(
            targets.map(|target| target.platform),
            ["linux/amd64", "linux/arm64"]
        );
        assert!(targets.iter().all(|target| {
            target.requires
                == [
                    "ployzd",
                    "dockerd",
                    "wg",
                    CORROSION_IMAGE,
                    SERVICE_CONTAINER_IMAGE,
                    CADDY_IMAGE,
                    UNREGISTRY_IMAGE,
                ]
        }));
    }

    #[tokio::test]
    async fn join_barrier_init_rejects_the_wrong_machine_count() {
        let cluster = Cluster {
            plan: ClusterPlan::new("l3-barrier", 1).unwrap(),
        };
        assert!(matches!(
            cluster.initialize_two_with_join_barrier().await,
            Err(TestkitError::MachineCount {
                expected: 2,
                actual: 1
            })
        ));
    }

    #[test]
    fn models_unfenced_registration_before_join_without_compensation() {
        use ployz_core::{MachineId, ManagementAddress, WireGuardPublicKey};

        fn machine(name: &str, key: u8) -> Machine {
            Machine {
                id: MachineId::random(),
                name: MachineName::parse(name).unwrap(),
                subnet: format!("10.210.{key}.0/24").parse().unwrap(),
                management_address: ManagementAddress(format!("fdcc::{key}").parse().unwrap()),
                public_key: WireGuardPublicKey([key; 32]),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint(
                    format!("127.0.0.1:{}", 51000 + u16::from(key))
                        .parse()
                        .unwrap(),
                )],
                runtime: Default::default(),
            }
        }

        let first = machine("machine-1", 1);
        let second = machine("machine-2", 2);
        let registration = Registered {
            assigned_machine: second.clone(),
            visible_peers: Vec::new(),
            target_versions: BTreeMap::from([("actor".to_owned(), 7)]),
        };
        let join = join_request(&first, &registration);
        assert_eq!(join.registration.assigned_machine, second);
        assert_eq!(join.registration.visible_peers, vec![first]);
        assert_eq!(join.registration.target_versions.get("actor"), Some(&7));
    }

    #[test]
    fn rejects_an_invalid_cluster_name() {
        assert!(matches!(
            ClusterPlan::new("", 1),
            Err(TestkitError::InvalidClusterName(name)) if name.is_empty()
        ));
        assert!(matches!(
            ClusterPlan::new("has space", 1),
            Err(TestkitError::InvalidClusterName(name)) if name == "has space"
        ));
    }
}
