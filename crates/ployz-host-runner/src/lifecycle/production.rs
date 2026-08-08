//! Shared Linux substrate mechanics composed by founding and join workflows.

use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
#[cfg(test)]
use hickory_proto::rr::Record;
use hickory_proto::rr::rdata::A;
use hickory_proto::rr::{DNSClass, Name, RData, RecordType};
use ployz_core::corrosion::MachineTransport;
use ployz_core::install::InstallArtifactSpec;
use ployz_core::network::internal_dns::{
    INTERNAL_DNS_READINESS_ADDRESS, INTERNAL_DNS_READINESS_NAME,
};
use ployz_core::operation::FailureMessage;

use crate::{
    ArtifactKind, ArtifactSourceView, DockerInstall, FileMode, HostPlatformProfile,
    HostRunnerCommandRunner, PloyzdArtifactStore, PloyzdKeeperRevertUnit, PloyzdRole,
    PloyzdRoleEnvironmentFile, PloyzdUpgradeRollback, SupervisorBackend, SupervisorChange,
    SupervisorDirectories, SupervisorUnitSpec, SupervisorUnitTarget,
    acquire_remote_artifact_content_addressed, artifact_target, detect_host_platform,
    install_verified_artifact, verify_artifact_file, write_durable_file,
};

const CORROSION_API_PORT: u16 = 8_080;
const CORROSION_GOSSIP_PORT: u16 = 8_787;
const DNS_PORT: u16 = 53;
const DNS_READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const DNS_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(3);
const DNS_READINESS_ID: [u8; 2] = [0x70, 0x6c];

pub(super) fn machine_endpoint_gateway(transport: &MachineTransport) -> Ipv4Addr {
    let subnet = match transport {
        MachineTransport::Wireguard { subnet_v4, .. }
        | MachineTransport::Tailscale { subnet_v4, .. } => subnet_v4,
    };
    subnet.bridge_gateway_ipv4()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CorrosionServiceChange {
    Enable,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CorrosionBootstrap<'a> {
    Founder,
    Seed(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeneratedSecretPersistence {
    InMemory,
    Durable,
}

pub(super) struct CorrosionConfig<'a> {
    pub state: &'a Path,
    pub schema_path: &'a str,
    pub gossip_addr: Ipv6Addr,
    pub bootstrap: CorrosionBootstrap<'a>,
    pub bearer_token: &'a str,
}

pub(super) struct LinuxSubstrate<'a, R> {
    state: &'a Path,
    runner: &'a mut R,
    profile: &'a mut Option<HostPlatformProfile>,
    supervisor_directories: &'a SupervisorDirectories,
}

impl<'a, R: HostRunnerCommandRunner> LinuxSubstrate<'a, R> {
    pub(super) fn new(
        state: &'a Path,
        runner: &'a mut R,
        profile: &'a mut Option<HostPlatformProfile>,
        supervisor_directories: &'a SupervisorDirectories,
    ) -> Self {
        Self {
            state,
            runner,
            profile,
            supervisor_directories,
        }
    }

    pub(super) fn profile(&mut self) -> Result<&HostPlatformProfile, FailureMessage> {
        if self.profile.is_none() {
            let release = self.runner.read_os_release()?;
            *self.profile = Some(detect_host_platform(&release).map_err(failure)?);
        }
        let Some(profile) = self.profile.as_ref() else {
            unreachable!("profile is populated immediately above")
        };
        Ok(profile)
    }

    pub(super) fn supervisor(&mut self) -> Result<SupervisorBackend, FailureMessage> {
        Ok(self.profile()?.supervisor().into())
    }

    pub(super) fn require(&mut self, program: &str, args: &[&str]) -> Result<(), FailureMessage> {
        let output = self.runner.command(program, args)?;
        if output.success {
            Ok(())
        } else {
            Err(failure(output.failure))
        }
    }

    pub(super) fn run_supervisor(
        &mut self,
        change: SupervisorChange,
        target: &SupervisorUnitTarget,
    ) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        for (program, args) in backend.commands(change, target) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }

    pub(super) fn install_artifact(
        &mut self,
        kind: ArtifactKind,
        spec: &InstallArtifactSpec,
    ) -> Result<(), FailureMessage> {
        let target = artifact_target(kind, spec).map_err(failure)?;
        let verified = match target.source_view() {
            ArtifactSourceView::LocalPath(path) => {
                verify_artifact_file(path, &target.digest).map_err(failure)?
            }
            ArtifactSourceView::RemoteUrl(url) => {
                let downloads = self.state.join("downloads");
                acquire_remote_artifact_content_addressed(
                    url,
                    &target.digest,
                    &downloads,
                    |staged| self.runner.download(url, staged),
                )
                .map_err(failure)?
            }
        };
        match kind {
            ArtifactKind::Ployzd => {
                PloyzdArtifactStore::new(self.state.to_path_buf())
                    .map_err(failure)?
                    .seed_current(&verified)
                    .map_err(failure)?;
            }
            ArtifactKind::EbpfBytecode
            | ArtifactKind::EbpfCtl
            | ArtifactKind::Corrosion
            | ArtifactKind::CorrosionSchema
            | ArtifactKind::Railpack => {
                install_verified_artifact(&verified, &target).map_err(failure)?;
            }
        }
        Ok(())
    }

    pub(super) fn ensure_docker(&mut self) -> Result<(), FailureMessage> {
        if !self.runner.docker_is_installed() {
            let install = self.profile()?.docker_install();
            match install {
                DockerInstall::GetDocker => {
                    let script = self.state.join("get-docker.sh");
                    self.runner.download("https://get.docker.com", &script)?;
                    self.require("sh", &[script.to_string_lossy().as_ref()])?;
                }
                DockerInstall::AlpinePackages => self.require("apk", &["add", "docker"])?,
                DockerInstall::ArchPackages => {
                    self.require("pacman", &["--noconfirm", "-S", "docker"])?;
                }
                DockerInstall::SusePackages => {
                    self.require("zypper", &["--non-interactive", "install", "docker"])?;
                }
                DockerInstall::AmazonPackages => {
                    self.require("dnf", &["install", "-y", "docker"])?;
                }
                DockerInstall::RhelRepositoryFile | DockerInstall::CentosRepositoryFile => {
                    self.require("dnf", &["install", "-y", "docker-ce"])?;
                }
            }
        }
        let backend = self.supervisor()?;
        for (program, args) in backend.docker_commands(SupervisorChange::InstallAndStart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        self.runner.docker_info()
    }

    pub(super) fn restart_docker_and_verify(&mut self) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        for (program, args) in backend.docker_commands(SupervisorChange::Restart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        self.runner.docker_info()
    }

    pub(super) fn install_ployzd_units(
        &mut self,
        environment: &PloyzdRoleEnvironmentFile,
    ) -> Result<(), FailureMessage> {
        self.install_role_privilege_substrate()?;
        let backend = self.supervisor()?;
        let artifact_store = PloyzdArtifactStore::new(self.state.to_path_buf()).map_err(failure)?;
        match backend.ployzd_upgrade_rollback() {
            PloyzdUpgradeRollback::SystemdOnFailure => {
                let rendered = PloyzdKeeperRevertUnit::new(artifact_store.clone())
                    .render()
                    .map_err(failure)?;
                write_durable_file(
                    self.supervisor_directories.directory(backend),
                    PloyzdKeeperRevertUnit::unit_name(),
                    FileMode::Executable0755,
                    rendered.as_bytes(),
                )?;
            }
            PloyzdUpgradeRollback::Unsupported => {}
        }
        for role in [
            PloyzdRole::Keeper,
            PloyzdRole::Api,
            PloyzdRole::Gateway,
            PloyzdRole::Dns,
        ] {
            let role_environment = match role {
                PloyzdRole::Gateway => {
                    PloyzdRoleEnvironmentFile::new(self.state.join("ployz-gateway.env"))
                        .map_err(failure)?
                }
                PloyzdRole::Keeper | PloyzdRole::Api | PloyzdRole::Dns => environment.clone(),
            };
            let spec = SupervisorUnitSpec::PloyzdRole {
                role,
                artifact_store: artifact_store.clone(),
                environment_file: role_environment,
            };
            let rendered = backend.render(&spec).map_err(failure)?;
            write_durable_file(
                self.supervisor_directories.directory(backend),
                rendered.file_name(),
                FileMode::Executable0755,
                rendered.contents().as_bytes(),
            )?;
            let target = spec.target();
            match role {
                PloyzdRole::Keeper | PloyzdRole::Api => {
                    self.run_supervisor(SupervisorChange::Enable, &target)?;
                }
                PloyzdRole::Gateway | PloyzdRole::Dns => {
                    self.run_supervisor(SupervisorChange::Disable, &target)?;
                    self.run_supervisor(SupervisorChange::Stop, &target)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn install_role_privilege_substrate(&mut self) -> Result<(), FailureMessage> {
        let package_family = self.profile()?.package_family();
        let commands = crate::installed_role_privilege_commands(self.state, package_family)?;
        for command in commands {
            let args = command
                .args()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            self.require(command.program(), &args)?;
        }
        Ok(())
    }

    pub(super) fn enable_and_start_dns(&mut self) -> Result<(), FailureMessage> {
        if self.supervisor()? != SupervisorBackend::Systemd {
            return Err(failure(
                "the DNS role requires systemd dynamic-user and capability isolation",
            ));
        }
        let target = SupervisorUnitTarget::PloyzdRole(PloyzdRole::Dns);
        self.run_supervisor(SupervisorChange::Enable, &target)?;
        self.run_supervisor(SupervisorChange::Restart, &target)
    }

    pub(super) fn enable_start_and_verify_gateway(&mut self) -> Result<(), FailureMessage> {
        let target = SupervisorUnitTarget::PloyzdRole(PloyzdRole::Gateway);
        self.run_supervisor(SupervisorChange::Enable, &target)?;
        self.run_supervisor(SupervisorChange::Restart, &target)?;
        self.wait_for_role(PloyzdRole::Gateway, "Gateway")
    }

    pub(super) fn await_endpoint_network_gateway(
        &mut self,
        expected: Ipv4Addr,
    ) -> Result<(), FailureMessage> {
        let deadline = Instant::now() + DNS_READINESS_TIMEOUT;
        let last = loop {
            let observed = match self.runner.command(
                "docker",
                &[
                    "network",
                    "inspect",
                    "ployz",
                    "--format",
                    "{{(index .IPAM.Config 0).Gateway}}",
                ],
            ) {
                Ok(output)
                    if output.success
                        && !output.stdout_truncated
                        && output.stdout.trim() == expected.to_string() =>
                {
                    return Ok(());
                }
                Ok(output) if output.stdout_truncated => {
                    "Docker network inspection output was truncated".to_owned()
                }
                Ok(output) if output.success => {
                    format!(
                        "ployz network gateway was {:?}, expected {expected}",
                        output.stdout.trim()
                    )
                }
                Ok(output) => output.failure,
                Err(error) => error.to_string(),
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break observed;
            }
            thread::sleep(Duration::from_secs(1).min(remaining));
        };
        Err(failure(format!(
            "endpoint network did not expose gateway {expected} within 30 seconds: {last}"
        )))
    }

    pub(super) fn await_dns_readiness(&mut self, gateway: Ipv4Addr) -> Result<(), FailureMessage> {
        await_dns_readiness_at(SocketAddr::from((gateway, DNS_PORT)), DNS_READINESS_TIMEOUT)
    }

    pub(super) fn install_corrosion_unit(&mut self, config: &Path) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        let (name, contents) = corrosion_unit(backend, config);
        write_durable_file(
            self.supervisor_directories.directory(backend),
            name,
            FileMode::Executable0755,
            contents.as_bytes(),
        )
    }

    pub(super) fn change_corrosion_service(
        &mut self,
        change: CorrosionServiceChange,
    ) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        for (program, args) in corrosion_commands(backend, change) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }

    pub(super) fn wait_for_role(
        &mut self,
        role: PloyzdRole,
        description: &str,
    ) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        let target = SupervisorUnitTarget::PloyzdRole(role);
        for _ in 0..30 {
            let mut healthy = true;
            for (program, args) in backend.commands(SupervisorChange::IsActive, &target) {
                let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
                healthy &= self.runner.command(program, &refs)?.success;
            }
            if healthy {
                return Ok(());
            }
            thread::sleep(Duration::from_secs(1));
        }
        Err(failure(format!(
            "{description} did not become ready within 30 seconds"
        )))
    }
}

fn await_dns_readiness_at(resolver: SocketAddr, timeout: Duration) -> Result<(), FailureMessage> {
    let deadline = Instant::now() + timeout;
    let last = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break "the DNS readiness deadline elapsed before a valid response".to_owned();
        }
        let observed = match probe_dns_once(resolver, DNS_ATTEMPT_TIMEOUT.min(remaining)) {
            Ok(()) => return Ok(()),
            Err(error) => error.to_string(),
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break observed;
        }
        thread::sleep(Duration::from_secs(1).min(remaining));
    };
    Err(failure(format!(
        "DNS resolver at {resolver} did not become ready within {} seconds: {last}",
        timeout.as_secs()
    )))
}

fn probe_dns_once(resolver: SocketAddr, timeout: Duration) -> std::io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_read_timeout(Some(timeout))?;
    socket.set_write_timeout(Some(timeout))?;
    socket.connect(resolver)?;
    socket.send(&dns_readiness_query())?;
    let mut response = [0_u8; 4_096];
    let length = socket.recv(&mut response)?;
    let Some(response) = response.get(..length) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DNS readiness response exceeded its receive buffer",
        ));
    };
    validate_dns_readiness_response(response)
}

fn dns_readiness_query() -> Vec<u8> {
    let name = readiness_dns_name();
    let mut query = Message::new(
        u16::from_be_bytes(DNS_READINESS_ID),
        MessageType::Query,
        OpCode::Query,
    );
    query.metadata.recursion_desired = true;
    query.add_query(Query::query(name, RecordType::A));
    query
        .to_vec()
        .expect("the static DNS readiness query is encodable")
}

fn validate_dns_readiness_response(response: &[u8]) -> std::io::Result<()> {
    let message = Message::from_vec(response)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))?;
    if message.metadata.id != u16::from_be_bytes(DNS_READINESS_ID) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DNS readiness transaction id did not match",
        ));
    }
    if message.metadata.message_type != MessageType::Response {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DNS readiness packet was not a response",
        ));
    }
    if message.metadata.response_code != ResponseCode::NoError {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DNS readiness response was not successful",
        ));
    }
    let expected_name = readiness_dns_name();
    let [query] = message.queries.as_slice() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DNS readiness response did not retain exactly one query",
        ));
    };
    if query.name != expected_name
        || query.query_type != RecordType::A
        || query.query_class != DNSClass::IN
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DNS readiness response retained a different query",
        ));
    }
    let contains_readiness_answer = message.answers.iter().any(|record| {
        record.name == expected_name
            && record.dns_class == DNSClass::IN
            && matches!(
                record.data,
                RData::A(address) if address == A::from(INTERNAL_DNS_READINESS_ADDRESS)
            )
    });
    if !contains_readiness_answer {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DNS readiness response did not contain the reserved A answer",
        ));
    }
    Ok(())
}

fn readiness_dns_name() -> Name {
    Name::from_ascii(format!("{INTERNAL_DNS_READINESS_NAME}."))
        .expect("the static DNS readiness name is valid")
}

pub(super) fn render_corrosion_config(config: CorrosionConfig<'_>) -> String {
    let CorrosionConfig {
        state,
        schema_path,
        gossip_addr,
        bootstrap,
        bearer_token,
    } = config;
    let bootstrap = match bootstrap {
        CorrosionBootstrap::Founder => "[]".to_owned(),
        CorrosionBootstrap::Seed(address) => format!("[{address:?}]"),
    };
    format!(
        "[db]\npath = {db:?}\nschema_paths = [{schema:?}]\nsubscriptions_path = {subscriptions:?}\n\n[gossip]\naddr = {gossip:?}\nbootstrap = {bootstrap}\nplaintext = true\nmax_mtu = 1232\n\n[api]\naddr = {api:?}\nauthz.bearer-token = {token:?}\n\n[admin]\npath = {admin:?}\n",
        db = state.join("corrosion.db").display().to_string(),
        schema = schema_path,
        subscriptions = state.join("subscriptions").display().to_string(),
        gossip = format!("[{gossip_addr}]:{CORROSION_GOSSIP_PORT}"),
        api = format!("127.0.0.1:{CORROSION_API_PORT}"),
        token = bearer_token,
        admin = state.join("corrosion-admin.sock").display().to_string(),
    )
}

pub(super) fn read_or_generate_secret(
    path: &Path,
    persistence: GeneratedSecretPersistence,
) -> Result<String, FailureMessage> {
    match fs::read_to_string(path) {
        Ok(secret) if !secret.trim().is_empty() => Ok(secret.trim().to_owned()),
        Ok(_) => Err(failure(format!("secret file {} is empty", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let secret = defguard_wireguard_rs::key::Key::generate().to_string();
            if persistence == GeneratedSecretPersistence::Durable {
                let parent = path
                    .parent()
                    .ok_or_else(|| failure("secret path has no parent"))?;
                let file_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| failure("secret path has no file name"))?;
                write_durable_file(
                    parent,
                    file_name,
                    FileMode::Secret0600,
                    format!("{secret}\n").as_bytes(),
                )?;
            }
            Ok(secret)
        }
        Err(error) => Err(failure(error)),
    }
}

pub(super) fn corrosion_unit(backend: SupervisorBackend, config: &Path) -> (&'static str, String) {
    match backend {
        SupervisorBackend::Systemd => (
            "ployz-corrosion.service",
            format!(
                "[Unit]\nDescription=Ployz Corrosion\nAfter=network-online.target ployzd-keeper.service\nWants=network-online.target\n\n[Service]\nType=exec\nExecStart=/usr/local/bin/corrosion --config {} agent\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
                config.display()
            ),
        ),
        SupervisorBackend::OpenRc => (
            "ployz-corrosion",
            format!(
                "#!/sbin/openrc-run\nname=ployz-corrosion\nsupervisor=supervise-daemon\ncommand=/usr/local/bin/corrosion\ncommand_args=\"--config {} agent\"\nrespawn_delay=5\n\ndepend() {{ need net; after ployzd-keeper; }}\n",
                config.display()
            ),
        ),
    }
}

fn corrosion_commands(
    backend: SupervisorBackend,
    change: CorrosionServiceChange,
) -> Vec<(&'static str, Vec<String>)> {
    match (backend, change) {
        (SupervisorBackend::Systemd, CorrosionServiceChange::Enable) => vec![
            ("systemctl", vec!["daemon-reload".to_owned()]),
            (
                "systemctl",
                vec!["enable".to_owned(), "ployz-corrosion.service".to_owned()],
            ),
        ],
        (SupervisorBackend::Systemd, CorrosionServiceChange::Restart) => vec![(
            "systemctl",
            vec!["restart".to_owned(), "ployz-corrosion.service".to_owned()],
        )],
        (SupervisorBackend::OpenRc, CorrosionServiceChange::Enable) => vec![(
            "rc-update",
            vec![
                "add".to_owned(),
                "ployz-corrosion".to_owned(),
                "default".to_owned(),
            ],
        )],
        (SupervisorBackend::OpenRc, CorrosionServiceChange::Restart) => vec![(
            "rc-service",
            vec!["ployz-corrosion".to_owned(), "restart".to_owned()],
        )],
    }
}

fn failure(error: impl std::fmt::Display) -> FailureMessage {
    FailureMessage::try_new(error.to_string()).unwrap_or_else(|_| {
        FailureMessage::try_new("Linux substrate effect failed").expect("constant is non-empty")
    })
}

#[cfg(test)]
mod dns_readiness_tests {
    use super::*;

    fn readiness_response(response_code: ResponseCode, address: Ipv4Addr) -> Vec<u8> {
        let query = Message::from_vec(&dns_readiness_query()).expect("readiness query parses");
        let [question] = query.queries.as_slice() else {
            panic!("readiness query contains exactly one question");
        };
        let mut response = Message::response(u16::from_be_bytes(DNS_READINESS_ID), OpCode::Query);
        response.metadata.response_code = response_code;
        response.add_query(question.clone());
        response.add_answer(Record::from_rdata(
            question.name.clone(),
            5,
            RData::A(A::from(address)),
        ));
        response.to_vec().expect("readiness response encodes")
    }

    #[test]
    fn readiness_uses_a_real_udp_query_and_validates_the_response() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("UDP server binds");
        let address = server.local_addr().expect("UDP server address");
        let responder = thread::spawn(move || {
            let mut packet = [0_u8; 512];
            let (length, peer) = server.recv_from(&mut packet).expect("DNS query arrives");
            let query = Message::from_vec(packet.get(..length).expect("query length"))
                .expect("readiness query parses");
            let [question] = query.queries.as_slice() else {
                panic!("readiness query contains exactly one question");
            };
            assert_eq!(query.metadata.message_type, MessageType::Query);
            assert_eq!(question.name.to_ascii(), "readiness.ployz.internal.");
            let response = readiness_response(
                ResponseCode::NoError,
                ployz_core::network::internal_dns::INTERNAL_DNS_READINESS_ADDRESS,
            );
            assert!(
                response.windows(2).any(|window| window == [0xc0, 0x0c]),
                "Hickory compresses the repeated answer name"
            );
            server.send_to(&response, peer).expect("DNS response sends");
        });

        await_dns_readiness_at(address, Duration::from_secs(2))
            .expect("valid UDP DNS response proves readiness");
        responder.join().expect("UDP responder exits");

        let mut wrong_id = dns_readiness_query();
        let [id_high, _, flags_high, ..] = wrong_id.as_mut_slice() else {
            panic!("readiness query carries its header");
        };
        *id_high ^= 0xff;
        *flags_high |= 0x80;
        assert!(validate_dns_readiness_response(&wrong_id).is_err());

        let mut wrong_question = dns_readiness_query();
        let [_, _, flags_high, ..] = wrong_question.as_mut_slice() else {
            panic!("readiness query carries its header");
        };
        *flags_high |= 0x80;
        let Some(first_label_byte) = wrong_question.get_mut(13) else {
            panic!("readiness query carries its first label");
        };
        *first_label_byte = b'x';
        assert!(validate_dns_readiness_response(&wrong_question).is_err());

        let negative = readiness_response(
            ResponseCode::NXDomain,
            ployz_core::network::internal_dns::INTERNAL_DNS_READINESS_ADDRESS,
        );
        assert!(validate_dns_readiness_response(&negative).is_err());

        let wrong_address = readiness_response(ResponseCode::NoError, Ipv4Addr::new(192, 0, 2, 54));
        assert!(validate_dns_readiness_response(&wrong_address).is_err());
    }

    #[test]
    fn tailscale_machine_uses_its_declared_endpoint_subnet_gateway() {
        let transport = MachineTransport::Tailscale {
            ip: "100.64.0.7".parse().expect("Tailscale address"),
            subnet_v4: ployz_core::network::MachineEndpointSubnet::try_new("10.210.7.0/24")
                .expect("endpoint subnet"),
        };

        assert_eq!(
            machine_endpoint_gateway(&transport),
            "10.210.7.1".parse::<Ipv4Addr>().expect("gateway")
        );
    }
}
