use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use ployz_core::install::MachineJoinEdgeTunnel;
use ployz_core::ops::FailureMessage;
use ployz_core::roles::joined_node_process_set;
use ployz_keeper::artifacts::{
    ArtifactSource, ArtifactVersion, DataplaneArtifactTargets, EbpfBytecodeArtifactTarget,
    EbpfCtlArtifactTarget, PloyzdArtifactTarget, Sha256Digest,
};
use ployz_keeper::cli::{KeeperCliError, KeeperCommand, load_command};
use ployz_keeper::executor::{KeeperPlanFailure, KeeperPlanTerminal, execute_keeper_plan};
use ployz_keeper::join_executor::{
    KeeperJoinRedeemer, KeeperJoinReporter, KeeperJoinTokenConsumer, RedeemedKeeperJoin,
    execute_keeper_join,
};
use ployz_keeper::local::{KeeperLocalConfig, KeeperLocalEffects, SystemKeeperCommandRunner};
use ployz_keeper::report::KeeperTextRecorder;
use ployz_keeper::steps::{
    FirstNodeInstallTarget, JoinToken, KeeperJoinMaterial, KeeperJoinTarget, NonEmptyRoleSet,
    PloyzdRoleEnvironmentTarget, first_node_install_plan,
};
use ployz_nats::connect::{NatsClientUrl, NatsClientUrlError, connect_with_timeout};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_sdk_types::{
    MachineJoinRedeemRequest, MachineJoinRedeemed, MachineJoinReportOutcome,
    MachineJoinReportRequest, MachineJoinToken,
};

const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
const PLOYZ_NODE_PUBLIC_IP_ENV: &str = "PLOYZ_NODE_PUBLIC_IP";
const DEFAULT_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> ExitCode {
    match load_command(std::env::args_os().skip(1)) {
        Ok(KeeperCommand::Start(startup)) => {
            if let Some(join) = &startup.join {
                let stdout = std::io::stdout();
                let mut recorder = KeeperTextRecorder::new(stdout.lock());
                let execution = run_join(&join.token, join.file.clone(), &mut recorder);
                match execution.terminal {
                    KeeperPlanTerminal::Completed => ExitCode::SUCCESS,
                    KeeperPlanTerminal::Failed(failure) => {
                        eprintln!("ployz-keeper join failed: {}", failure_summary(&failure));
                        ExitCode::FAILURE
                    }
                }
            } else {
                println!("ployz-keeper started");
                ExitCode::SUCCESS
            }
        }
        Ok(KeeperCommand::FirstNodeInstall(target)) => {
            let stdout = std::io::stdout();
            let mut recorder = KeeperTextRecorder::new(stdout.lock());
            let execution = run_first_node_install(*target, &mut recorder);
            match execution.terminal {
                KeeperPlanTerminal::Completed => ExitCode::SUCCESS,
                KeeperPlanTerminal::Failed(failure) => {
                    eprintln!(
                        "ployz-keeper first-node-install failed: {}",
                        failure_summary(&failure)
                    );
                    ExitCode::FAILURE
                }
            }
        }
        Err(KeeperCliError::HelpRequested) => {
            println!("{}", KeeperCliError::HelpRequested);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run_join(
    token: &JoinToken,
    join_token_file: std::path::PathBuf,
    recorder: &mut impl ployz_keeper::executor::KeeperStepRecorder,
) -> ployz_keeper::executor::KeeperPlanExecution {
    let mut redeemer = SystemJoinRedeemer::from_env();
    let mut reporter = SystemJoinReporter::from_env(token.clone());
    let mut token_consumer = StartupJoinTokenConsumer { join_token_file };
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: "/var/lib/ployz".into(),
        },
        SystemKeeperCommandRunner::default(),
    );
    execute_keeper_join(
        token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        recorder,
    )
}

fn run_first_node_install(
    target: FirstNodeInstallTarget,
    recorder: &mut impl ployz_keeper::executor::KeeperStepRecorder,
) -> ployz_keeper::executor::KeeperPlanExecution {
    let plan = first_node_install_plan(target);
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: "/var/lib/ployz".into(),
        },
        SystemKeeperCommandRunner::default(),
    );
    execute_keeper_plan(&plan, &mut effects, recorder)
}

fn failure_summary(failure: &KeeperPlanFailure) -> &str {
    match failure {
        KeeperPlanFailure::Step(step) => step.message.as_str(),
        KeeperPlanFailure::Record(record) => record.message.as_str(),
    }
}

struct SystemJoinRedeemer {
    nats_url: Result<NatsClientUrl, KeeperNatsUrlError>,
    node_public_ip: Result<Option<std::net::IpAddr>, KeeperNodePublicIpError>,
}

impl SystemJoinRedeemer {
    fn from_env() -> Self {
        Self {
            nats_url: load_nats_url_from_env(),
            node_public_ip: load_node_public_ip_from_env(),
        }
    }
}

impl KeeperJoinRedeemer for SystemJoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedKeeperJoin, FailureMessage> {
        let nats_url = self
            .nats_url
            .clone()
            .map_err(|error| failure_message(&format!("{error}")))?;
        let node_public_ip = self
            .node_public_ip
            .clone()
            .map_err(|error| failure_message(&format!("{error}")))?;
        let join_token = MachineJoinToken::try_new(token.as_str())
            .map_err(|error| failure_message(&format!("invalid join token: {error:?}")))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure_message(&format!("failed to start async runtime: {error}")))?;

        let redeem_nats_url = nats_url.clone();
        let redeemed = runtime.block_on(async move {
            let client = connect_with_timeout(&redeem_nats_url, DEFAULT_NATS_CONNECT_TIMEOUT)
                .await
                .map_err(|error| failure_message(&error.to_string()))?;
            OperationApiClient::new(client)
                .machine_join_redeem(&MachineJoinRedeemRequest { join_token })
                .await
                .map_err(|error| failure_message(&format!("failed to redeem join token: {error}")))
        })?;

        keeper_join_target_with_public_ip(redeemed, node_public_ip)
    }
}

struct SystemJoinReporter {
    nats_url: Result<NatsClientUrl, KeeperNatsUrlError>,
    join_token: JoinToken,
}

impl SystemJoinReporter {
    fn from_env(join_token: JoinToken) -> Self {
        Self {
            nats_url: load_nats_url_from_env(),
            join_token,
        }
    }
}

impl KeeperJoinReporter for SystemJoinReporter {
    fn report_join_completed(&mut self) -> Result<(), FailureMessage> {
        self.report_join_result(MachineJoinReportRequest {
            join_token: self.machine_join_token()?,
            outcome: MachineJoinReportOutcome::Completed,
        })
    }

    fn report_join_failed(
        &mut self,
        failure: ployz_sdk_types::MachineJoinReportFailure,
    ) -> Result<(), FailureMessage> {
        self.report_join_result(MachineJoinReportRequest {
            join_token: self.machine_join_token()?,
            outcome: MachineJoinReportOutcome::Failed { failure },
        })
    }
}

impl SystemJoinReporter {
    fn report_join_result(&self, request: MachineJoinReportRequest) -> Result<(), FailureMessage> {
        let nats_url = self
            .nats_url
            .clone()
            .map_err(|error| failure_message(&format!("{error}")))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure_message(&format!("failed to start async runtime: {error}")))?;

        runtime.block_on(async move {
            let client = connect_with_timeout(&nats_url, DEFAULT_NATS_CONNECT_TIMEOUT)
                .await
                .map_err(|error| failure_message(&error.to_string()))?;
            OperationApiClient::new(client)
                .machine_join_report(&request)
                .await
                .map(|_| ())
                .map_err(|error| failure_message(&format!("failed to report join result: {error}")))
        })
    }

    fn machine_join_token(&self) -> Result<MachineJoinToken, FailureMessage> {
        MachineJoinToken::try_new(self.join_token.as_str())
            .map_err(|error| failure_message(&format!("invalid join token: {error:?}")))
    }
}

fn load_nats_url_from_env() -> Result<NatsClientUrl, KeeperNatsUrlError> {
    let value = std::env::var(PLOYZ_NATS_URL_ENV).map_err(|_| KeeperNatsUrlError::Missing)?;
    NatsClientUrl::try_new(value.clone())
        .map_err(|source| KeeperNatsUrlError::Invalid { value, source })
}

fn load_node_public_ip_from_env() -> Result<Option<std::net::IpAddr>, KeeperNodePublicIpError> {
    let Some(value) = std::env::var(PLOYZ_NODE_PUBLIC_IP_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|source| KeeperNodePublicIpError::Invalid { value, source })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeeperNatsUrlError {
    Missing,
    Invalid {
        value: String,
        source: NatsClientUrlError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeeperNodePublicIpError {
    Invalid {
        value: String,
        source: std::net::AddrParseError,
    },
}

impl std::fmt::Display for KeeperNatsUrlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(formatter, "{PLOYZ_NATS_URL_ENV} is required"),
            Self::Invalid { value, .. } => {
                write!(formatter, "{PLOYZ_NATS_URL_ENV}={value:?} is invalid")
            }
        }
    }
}

impl std::fmt::Display for KeeperNodePublicIpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { value, .. } => {
                write!(formatter, "{PLOYZ_NODE_PUBLIC_IP_ENV}={value:?} is invalid")
            }
        }
    }
}

fn keeper_join_target_with_public_ip(
    redeemed: MachineJoinRedeemed,
    node_public_ip: Option<std::net::IpAddr>,
) -> Result<RedeemedKeeperJoin, FailureMessage> {
    let node_id = redeemed.node_id.clone();
    let material = KeeperJoinMaterial::from_join_payload(
        node_id.clone(),
        &redeemed.join_bundle,
        &redeemed.secret_delivery,
    )
    .map_err(|error| failure_message(&format!("invalid join material: {error:?}")))?;
    let ployzd_artifact = PloyzdArtifactTarget::new(
        ArtifactVersion::try_new(redeemed.join_bundle.material.ployzd.version.as_str())
            .map_err(|error| failure_message(&format!("invalid ployzd version: {error}")))?,
        ArtifactSource::try_new(redeemed.join_bundle.material.ployzd.source.as_str())
            .map_err(|error| failure_message(&format!("invalid ployzd source: {error}")))?,
        Sha256Digest::try_new(redeemed.join_bundle.material.ployzd.sha256.as_str())
            .map_err(|error| failure_message(&format!("invalid ployzd digest: {error}")))?,
        PathBuf::from(redeemed.join_bundle.material.ployzd.install_path.as_str()),
    )
    .map_err(|error| failure_message(&format!("invalid ployzd install target: {error}")))?;
    let ebpf_bytecode_artifact = EbpfBytecodeArtifactTarget::new(
        ArtifactVersion::try_new(redeemed.join_bundle.material.ebpf_bytecode.version.as_str())
            .map_err(|error| failure_message(&format!("invalid eBPF bytecode version: {error}")))?,
        ArtifactSource::try_new(redeemed.join_bundle.material.ebpf_bytecode.source.as_str())
            .map_err(|error| failure_message(&format!("invalid eBPF bytecode source: {error}")))?,
        Sha256Digest::try_new(redeemed.join_bundle.material.ebpf_bytecode.sha256.as_str())
            .map_err(|error| failure_message(&format!("invalid eBPF bytecode digest: {error}")))?,
        PathBuf::from(
            redeemed
                .join_bundle
                .material
                .ebpf_bytecode
                .install_path
                .as_str(),
        ),
    )
    .map_err(|error| failure_message(&format!("invalid eBPF bytecode install target: {error}")))?;
    let ebpf_ctl_artifact = EbpfCtlArtifactTarget::new(
        ArtifactVersion::try_new(redeemed.join_bundle.material.ebpf_ctl.version.as_str())
            .map_err(|error| failure_message(&format!("invalid eBPF ctl version: {error}")))?,
        ArtifactSource::try_new(redeemed.join_bundle.material.ebpf_ctl.source.as_str())
            .map_err(|error| failure_message(&format!("invalid eBPF ctl source: {error}")))?,
        Sha256Digest::try_new(redeemed.join_bundle.material.ebpf_ctl.sha256.as_str())
            .map_err(|error| failure_message(&format!("invalid eBPF ctl digest: {error}")))?,
        PathBuf::from(redeemed.join_bundle.material.ebpf_ctl.install_path.as_str()),
    )
    .map_err(|error| failure_message(&format!("invalid eBPF ctl install target: {error}")))?;
    let roles = NonEmptyRoleSet::try_new(
        joined_node_process_set(&node_id, redeemed.gateway)
            .roles()
            .to_vec(),
    )
    .map_err(|error| failure_message(&format!("invalid joined node role set: {error:?}")))?;
    let edge_tunnel = MachineJoinEdgeTunnel::from_join_bundle(&redeemed.join_bundle);
    let runtime_nats_client_url = NatsClientUrl::try_new(edge_tunnel.runtime_nats_url.as_str())
        .map_err(|error| failure_message(&format!("invalid runtime nats url: {error:?}")))?;
    let mut role_environment =
        PloyzdRoleEnvironmentTarget::default_path(node_id.clone(), runtime_nats_client_url)
            .with_edge_tunnel(
                edge_tunnel.listen_addr,
                edge_tunnel.core_node,
                edge_tunnel.core_public_key,
                edge_tunnel.core_direct_addresses,
                edge_tunnel.core_relay_url,
            );
    if let Some(public_ip) = node_public_ip {
        role_environment = role_environment.with_node_public_ip(public_ip);
    }

    Ok(RedeemedKeeperJoin::new(
        redeemed.operation_id,
        node_id.clone(),
        KeeperJoinTarget::new(
            material,
            ployzd_artifact,
            DataplaneArtifactTargets::new(ebpf_bytecode_artifact, ebpf_ctl_artifact),
            roles,
            role_environment,
        ),
    ))
}

fn failure_message(message: &str) -> FailureMessage {
    FailureMessage::try_new(message).expect("generated keeper failure message is non-empty")
}

struct StartupJoinTokenConsumer {
    join_token_file: std::path::PathBuf,
}

impl KeeperJoinTokenConsumer for StartupJoinTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        ployz_keeper::join::remove_join_token_file(&self.join_token_file).map_err(|error| {
            FailureMessage::try_new(error.to_string()).expect("join token file error is non-empty")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::keeper_join_target_with_public_ip;
    use ployz_core::ids::{NodeId, OperationId};
    use ployz_core::install::{
        AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
        MachineJoinArtifact, MachineJoinBundle, MachineJoinClusterName,
        MachineJoinCoreIrohEndpoint, MachineJoinIrohPublicKey, MachineJoinIrohTicket,
        MachineJoinMaterial, MachineJoinNatsCredentials, MachineJoinPloyzdArtifact,
        MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTrustedNats,
        MachineJoinTrustedNatsServerId,
    };
    use ployz_core::machine::{JoinTokenRedeemedAt, MachineName};
    use ployz_core::roles::FirstNodeGateway;
    use ployz_sdk_types::{MachineJoinRedeemResult, MachineJoinRedeemed};

    #[test]
    fn keeper_join_target_uses_runtime_nats_url_from_redeemed_bundle() {
        let redeemed = MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            node_id: NodeId::try_new("node_2").expect("valid node id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: FirstNodeGateway::Skip,
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::ops::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        };

        let target = keeper_join_target_with_public_ip(redeemed, None)
            .expect("redeemed bundle converts")
            .target;

        assert_eq!(
            target.role_environment.render(),
            "PLOYZ_NATS_URL=nats://127.0.0.1:7422\nPLOYZ_NODE_ID=node_2\nPLOYZ_EBPF_BYTECODE=/usr/local/lib/ployz/ebpf/ployz-ebpf-tc\nPLOYZ_EBPF_CTL=/usr/local/bin/ployz-ebpf-ctl\nPLOYZ_TUNNEL_SECRET_KEY_FILE=/var/lib/ployz/iroh/endpoint.key\nPLOYZ_TUNNEL_PUBLIC_KEY_FILE=/var/lib/ployz/iroh/endpoint.public\nPLOYZ_TUNNEL_IROH_BIND_ADDR=0.0.0.0:0\nPLOYZ_TUNNEL_LISTEN_ADDR=127.0.0.1:7422\nPLOYZ_TUNNEL_CORE_NODE=core_1\nPLOYZ_TUNNEL_CORE_PUBLIC_KEY=core-public-key\n"
        );
    }

    #[test]
    fn keeper_join_target_can_carry_node_public_ip_from_bootstrap_env() {
        let redeemed = MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            node_id: NodeId::try_new("node_2").expect("valid node id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: FirstNodeGateway::Skip,
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::ops::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        };

        let target = keeper_join_target_with_public_ip(
            redeemed,
            Some("203.0.113.20".parse().expect("valid IP")),
        )
        .expect("redeemed bundle converts")
        .target;

        assert!(
            target
                .role_environment
                .render()
                .contains("PLOYZ_NODE_PUBLIC_IP=203.0.113.20\n")
        );
    }

    fn machine_join_bundle() -> MachineJoinBundle {
        MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                    .expect("valid runtime nats url"),
                trusted_nats: MachineJoinTrustedNats {
                    server_id: MachineJoinTrustedNatsServerId::try_new("server_1")
                        .expect("valid nats server id"),
                    config_sha256: InstallSha256Digest::try_new(
                        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    )
                    .expect("valid nats config digest"),
                },
                core_iroh: MachineJoinCoreIrohEndpoint {
                    node_id: NodeId::try_new("core_1").expect("valid core node id"),
                    public_key: MachineJoinIrohPublicKey::try_new("core-public-key")
                        .expect("valid core iroh public key"),
                    direct_addresses: Vec::new(),
                    relay_url: None,
                },
                ployzd: MachineJoinPloyzdArtifact {
                    version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                    source: InstallArtifactSource::try_new("/tmp/ployzd").expect("valid source"),
                    sha256: InstallSha256Digest::try_new(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid digest"),
                    install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployzd")
                        .expect("valid install path"),
                },
                ebpf_bytecode: MachineJoinArtifact {
                    version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                    source: InstallArtifactSource::try_new("/tmp/ployz-ebpf-tc")
                        .expect("valid source"),
                    sha256: InstallSha256Digest::try_new(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid digest"),
                    install_path: AbsoluteInstallPath::try_new(
                        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                    )
                    .expect("valid install path"),
                },
                ebpf_ctl: MachineJoinArtifact {
                    version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                    source: InstallArtifactSource::try_new("/tmp/ployz-ebpf-ctl")
                        .expect("valid source"),
                    sha256: InstallSha256Digest::try_new(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid digest"),
                    install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployz-ebpf-ctl")
                        .expect("valid install path"),
                },
            },
        }
    }

    fn machine_join_secret_delivery() -> MachineJoinSecretDelivery {
        MachineJoinSecretDelivery {
            nats_credentials: MachineJoinNatsCredentials::try_new("user-jwt-and-seed")
                .expect("valid nats credentials"),
            core_iroh_ticket: MachineJoinIrohTicket::try_new("core-ticket")
                .expect("valid core iroh ticket"),
        }
    }
}
