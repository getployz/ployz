use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use ployz_core::ids::MachineId;
use ployz_core::install::{MachineJoinRuntimeNatsUrl, NatsMachineMaterialPaths};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::FailureMessage;
use ployz_core::roles::plan_joined_machine_process_set;
use ployz_core::security::NatsPrincipal;
use ployz_keeper::artifacts::{ArtifactKind, DataplaneArtifactTargets, artifact_target};
use ployz_keeper::cli::{KeeperBootstrap, KeeperBootstrapMode, KeeperCommand, load_command};
use ployz_keeper::command::SystemKeeperCommandRunner;
use ployz_keeper::executor::{KeeperPlanFailure, KeeperPlanTerminal, execute_keeper_plan};
use ployz_keeper::join::JOIN_MATERIAL_DIR;
use ployz_keeper::join_executor::{
    KeeperJoinRedeemer, KeeperJoinReporter, KeeperJoinTokenConsumer, RedeemedKeeperJoin,
    execute_keeper_join,
};
use ployz_keeper::local::{KeeperLocalConfig, KeeperLocalEffects};
use ployz_keeper::report::KeeperTextRecorder;
use ployz_keeper::steps::{
    FirstMachineInstallTarget, JoinToken, KeeperJoinMaterial, KeeperJoinTarget, NonEmptyRoleSet,
    PloyzdRoleEnvironmentTarget, RoleNatsCredentials, first_machine_install_plan,
};
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsClientUrlError, NatsConnectConfig, NatsTlsTrust,
    connect_authenticated,
};
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    CloudFounderBootstrapResult, MachineJoinRedeemError, MachineJoinRedeemRequest,
    MachineJoinRedeemed, MachineJoinReportOutcome, MachineJoinReportRequest, MachineJoinToken,
    MachineJoinTrustedNats, NatsCaCertificatePem,
};
mod cloud_bootstrap_runner;

const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
const PLOYZ_NATS_CA_FILE_ENV: &str = "PLOYZ_NATS_CA_FILE";
const PLOYZ_JOIN_NKEY_SEED_ENV: &str = "PLOYZ_JOIN_NKEY_SEED";
const PLOYZ_MACHINE_PUBLIC_IP_ENV: &str = "PLOYZ_MACHINE_PUBLIC_IP";
const DEFAULT_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounded redeem retry while the core mints this machine's credential:
/// the join token TTL is 600 seconds, so stop well within it.
const REDEEM_MATERIAL_ATTEMPTS: u32 = 150;
const REDEEM_MATERIAL_RETRY_DELAY: Duration = Duration::from_secs(2);
const KEEPER_STATE_DIR: &str = "/var/lib/ployz";
const FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN: &str = "ployz-first-machine-bootstrap-result begin";
const FIRST_MACHINE_BOOTSTRAP_RESULT_END: &str = "ployz-first-machine-bootstrap-result end";
const CLOUD_BOOTSTRAP_MAX_POLLS: u16 = 900;

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
        Ok(KeeperCommand::Bootstrap(bootstrap)) => run_bootstrap_command(bootstrap),
        Ok(KeeperCommand::FirstMachineInstall(target)) => {
            let machine_id = target.machine_id.clone();
            let nats_material = target.nats_material.clone();
            let runtime_nats_url = target.machine_join_runtime_nats_url().clone();
            let stdout = std::io::stdout();
            let mut recorder = KeeperTextRecorder::new(stdout.lock());
            let execution = run_first_machine_install(*target, &mut recorder);
            match execution.terminal {
                KeeperPlanTerminal::Completed => {
                    drop(recorder);
                    match print_first_machine_bootstrap_result(
                        &machine_id,
                        &runtime_nats_url,
                        &nats_material,
                    ) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(message) => {
                            eprintln!("{message}");
                            ExitCode::FAILURE
                        }
                    }
                }
                KeeperPlanTerminal::Failed(failure) => {
                    eprintln!(
                        "ployz-keeper first-machine-install failed: {}",
                        failure_summary(&failure)
                    );
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) if error.is_help_requested() => {
            print!("{error}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run_bootstrap_command(bootstrap: KeeperBootstrap) -> ExitCode {
    match bootstrap.mode {
        KeeperBootstrapMode::Interactive { cloud_host } => {
            let host = match choose_cloud_host(cloud_host) {
                Ok(Some(host)) => host,
                Ok(None) => {
                    eprintln!("Use local CLI setup from your workstation:");
                    eprintln!("  ployzctl machine init USER@HOST");
                    return ExitCode::FAILURE;
                }
                Err(message) => {
                    eprintln!("{message}");
                    return ExitCode::FAILURE;
                }
            };
            cloud_bootstrap_runner::run_interactive_cloud_bootstrap(host.as_str())
        }
    }
}

fn choose_cloud_host(
    cloud_host: Option<ployz_keeper::cli::CloudHost>,
) -> Result<Option<ployz_keeper::cli::CloudHost>, String> {
    if let Some(host) = cloud_host {
        return Ok(Some(host));
    }

    println!("Ployz bootstrap");
    println!("1. Connect to Ployz Cloud");
    println!("2. Connect to custom Cloud");
    println!("3. Use local CLI setup");
    let mut choice = String::new();
    std::io::stdin()
        .read_line(&mut choice)
        .map_err(|error| format!("failed to read bootstrap choice: {error}"))?;
    match choice.trim() {
        "" | "1" => Ok(Some(Default::default())),
        "2" => {
            println!("Cloud host:");
            let mut host = String::new();
            std::io::stdin()
                .read_line(&mut host)
                .map_err(|error| format!("failed to read cloud host: {error}"))?;
            host.trim()
                .parse()
                .map(Some)
                .map_err(|error| format!("{error}"))
        }
        "3" => Ok(None),
        other => Err(format!("unknown bootstrap choice: {other}")),
    }
}

fn print_first_machine_bootstrap_result(
    machine_id: &MachineId,
    runtime_nats_url: &MachineJoinRuntimeNatsUrl,
    material: &NatsMachineMaterialPaths,
) -> Result<(), String> {
    let cloud_safe = read_cloud_founder_bootstrap_result(machine_id, runtime_nats_url, material)?;
    let operator_seed = read_result_file(&material.operator_seed_file(), "operator seed")?;
    let join_seed = read_result_file(&material.join_seed_file(), "Join seed")?;
    let result = serde_json::json!({
        "machine_id": machine_id.as_str(),
        "nats_url": runtime_nats_url.as_str(),
        "ca_pem": cloud_safe.trusted_nats.ca_pem.as_str(),
        "operator_seed": operator_seed.trim(),
        "join_seed": join_seed.trim(),
    });
    println!("{FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN}");
    println!(
        "{}",
        serde_json::to_string(&result).expect("bootstrap result json serializes")
    );
    println!("{FIRST_MACHINE_BOOTSTRAP_RESULT_END}");
    Ok(())
}

fn read_cloud_founder_bootstrap_result(
    machine_id: &MachineId,
    runtime_nats_url: &MachineJoinRuntimeNatsUrl,
    material: &NatsMachineMaterialPaths,
) -> Result<CloudFounderBootstrapResult, String> {
    let ca_pem = read_result_file(&material.ca_file(), "cluster CA")?;
    let ca_pem = NatsCaCertificatePem::try_new(ca_pem)
        .map_err(|error| format!("first-machine bootstrap cluster CA is invalid: {error}"))?;
    Ok(CloudFounderBootstrapResult {
        machine_id: machine_id.clone(),
        runtime_nats_url: runtime_nats_url.clone(),
        trusted_nats: MachineJoinTrustedNats { ca_pem },
    })
}

fn read_result_file(path: &std::path::Path, label: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| {
        format!(
            "failed to read first-machine bootstrap {label} {}: {error}",
            path.display()
        )
    })
}

fn run_join(
    token: &JoinToken,
    join_token_file: std::path::PathBuf,
    recorder: &mut impl ployz_keeper::executor::KeeperStepRecorder,
) -> ployz_keeper::executor::KeeperPlanExecution {
    let mut redeemer = JoinRedeemer::from_env();
    let mut reporter = JoinReporter::from_env(token.clone());
    let mut token_consumer = StartupJoinTokenConsumer { join_token_file };
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: KEEPER_STATE_DIR.into(),
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

fn run_first_machine_install(
    target: FirstMachineInstallTarget,
    recorder: &mut impl ployz_keeper::executor::KeeperStepRecorder,
) -> ployz_keeper::executor::KeeperPlanExecution {
    let plan = first_machine_install_plan(target);
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: KEEPER_STATE_DIR.into(),
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

struct JoinRedeemer {
    connect: Result<NatsConnectConfig, FailureMessage>,
    machine_public_ip: Result<Option<std::net::IpAddr>, FailureMessage>,
}

impl JoinRedeemer {
    fn new(
        connect: Result<NatsConnectConfig, FailureMessage>,
        machine_public_ip: Result<Option<std::net::IpAddr>, FailureMessage>,
    ) -> Self {
        Self {
            connect,
            machine_public_ip,
        }
    }

    fn from_env() -> Self {
        Self::new(
            load_join_connect_from_env().map_err(|error| failure_message(&format!("{error}"))),
            load_machine_public_ip_from_env().map_err(|error| failure_message(&format!("{error}"))),
        )
    }
}

impl KeeperJoinRedeemer for JoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedKeeperJoin, FailureMessage> {
        let connect = self.connect.clone()?;
        let machine_public_ip = self.machine_public_ip.clone()?;
        let join_token = MachineJoinToken::try_new(token.as_str())
            .map_err(|error| failure_message(&format!("invalid join token: {error:?}")))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure_message(&format!("failed to start async runtime: {error}")))?;

        let redeemed = runtime.block_on(async move {
            let client = connect_authenticated(&connect, DEFAULT_NATS_CONNECT_TIMEOUT)
                .await
                .map_err(|error| failure_message(&error.to_string()))?;
            redeem_until_material_ready(&OperationApiClient::new(client), join_token).await
        })?;

        keeper_join_target_with_public_ip(redeemed, machine_public_ip)
    }
}

/// Redeems the join token, retrying boundedly while the core's mint worker
/// has not reached `material-ready` yet. Any other failure is terminal.
async fn redeem_until_material_ready(
    api: &OperationApiClient,
    join_token: MachineJoinToken,
) -> Result<MachineJoinRedeemed, FailureMessage> {
    let mut last_not_ready = String::new();
    for _ in 0..REDEEM_MATERIAL_ATTEMPTS {
        match api
            .machine_join_redeem(&MachineJoinRedeemRequest {
                join_token: join_token.clone(),
            })
            .await
        {
            Ok(redeemed) => return Ok(redeemed),
            Err(OperationApiClientError::Domain {
                error: MachineJoinRedeemError::MaterialNotReady { operation_id },
                ..
            }) => {
                last_not_ready = format!(
                    "operation {} has not reached material-ready",
                    operation_id.as_str()
                );
                tokio::time::sleep(REDEEM_MATERIAL_RETRY_DELAY).await;
            }
            Err(error) => {
                return Err(failure_message(&format!(
                    "failed to redeem join token: {error}"
                )));
            }
        }
    }

    Err(failure_message(&format!(
        "join material did not become ready within {REDEEM_MATERIAL_ATTEMPTS} attempts: {last_not_ready}"
    )))
}

struct JoinReporter {
    connect: Result<NatsConnectConfig, FailureMessage>,
    join_token: JoinToken,
}

impl JoinReporter {
    fn new(connect: Result<NatsConnectConfig, FailureMessage>, join_token: JoinToken) -> Self {
        Self {
            connect,
            join_token,
        }
    }

    fn from_env(join_token: JoinToken) -> Self {
        Self::new(
            load_join_connect_from_env().map_err(|error| failure_message(&format!("{error}"))),
            join_token,
        )
    }
}

impl KeeperJoinReporter for JoinReporter {
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

impl JoinReporter {
    fn report_join_result(&self, request: MachineJoinReportRequest) -> Result<(), FailureMessage> {
        let connect = self.connect.clone()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure_message(&format!("failed to start async runtime: {error}")))?;

        runtime.block_on(async move {
            let client = connect_authenticated(&connect, DEFAULT_NATS_CONNECT_TIMEOUT)
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
        let token = self.join_token.clone();
        MachineJoinToken::try_new(token.as_str())
            .map_err(|error| failure_message(&format!("invalid join token: {error:?}")))
    }
}

struct CloudJoinTokenConsumer;

impl KeeperJoinTokenConsumer for CloudJoinTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        Ok(())
    }
}

/// Builds the keeper's Join-credential connection: TLS against the cluster
/// CA file plus the deliberately low-privilege Join seed, both delivered by
/// the install command env.
fn load_join_connect_from_env() -> Result<NatsConnectConfig, KeeperNatsConnectError> {
    let url = std::env::var(PLOYZ_NATS_URL_ENV).map_err(|_| KeeperNatsConnectError::MissingUrl)?;
    let url = NatsClientUrl::try_new(url.clone())
        .map_err(|source| KeeperNatsConnectError::InvalidUrl { value: url, source })?;
    let ca_file = std::env::var(PLOYZ_NATS_CA_FILE_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(KeeperNatsConnectError::MissingCaFile)?;
    let seed = std::env::var(PLOYZ_JOIN_NKEY_SEED_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(KeeperNatsConnectError::MissingJoinSeed)?;
    let seed =
        NatsUserSeed::try_new(seed.trim()).map_err(|_| KeeperNatsConnectError::InvalidJoinSeed)?;

    Ok(NatsConnectConfig {
        url,
        auth: NatsClientAuth::NkeySeed(seed),
        trust: NatsTlsTrust::ClusterCa(ca_file),
        principal: NatsPrincipal::Join,
    })
}

fn load_machine_public_ip_from_env() -> Result<Option<std::net::IpAddr>, KeeperMachinePublicIpError>
{
    let Some(value) = std::env::var(PLOYZ_MACHINE_PUBLIC_IP_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|source| KeeperMachinePublicIpError::Invalid { value, source })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeeperNatsConnectError {
    MissingUrl,
    InvalidUrl {
        value: String,
        source: NatsClientUrlError,
    },
    MissingCaFile,
    MissingJoinSeed,
    InvalidJoinSeed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeeperMachinePublicIpError {
    Invalid {
        value: String,
        source: std::net::AddrParseError,
    },
}

impl std::fmt::Display for KeeperNatsConnectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingUrl => write!(formatter, "{PLOYZ_NATS_URL_ENV} is required"),
            Self::InvalidUrl { value, .. } => {
                write!(formatter, "{PLOYZ_NATS_URL_ENV}={value:?} is invalid")
            }
            Self::MissingCaFile => write!(formatter, "{PLOYZ_NATS_CA_FILE_ENV} is required"),
            Self::MissingJoinSeed => {
                write!(formatter, "{PLOYZ_JOIN_NKEY_SEED_ENV} is required")
            }
            Self::InvalidJoinSeed => write!(
                formatter,
                "{PLOYZ_JOIN_NKEY_SEED_ENV} must be an SU-prefixed user seed"
            ),
        }
    }
}

impl std::fmt::Display for KeeperMachinePublicIpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { value, .. } => {
                write!(
                    formatter,
                    "{PLOYZ_MACHINE_PUBLIC_IP_ENV}={value:?} is invalid"
                )
            }
        }
    }
}

fn keeper_join_target_with_public_ip(
    redeemed: MachineJoinRedeemed,
    machine_public_ip: Option<std::net::IpAddr>,
) -> Result<RedeemedKeeperJoin, FailureMessage> {
    let callback_result = redeemed.clone();
    let machine_id = redeemed.machine_id.clone();
    let material = KeeperJoinMaterial::from_join_payload(
        machine_id.clone(),
        &redeemed.join_bundle,
        &redeemed.secret_delivery,
    )
    .map_err(|error| failure_message(&format!("invalid join material: {error:?}")))?;
    let ployzd_artifact =
        artifact_target(ArtifactKind::Ployzd, &redeemed.join_bundle.material.ployzd)
            .map_err(|error| failure_message(&format!("invalid ployzd install target: {error}")))?;
    let ebpf_bytecode_artifact = artifact_target(
        ArtifactKind::EbpfBytecode,
        &redeemed.join_bundle.material.ebpf_bytecode,
    )
    .map_err(|error| failure_message(&format!("invalid eBPF bytecode install target: {error}")))?;
    let ebpf_ctl_artifact = artifact_target(
        ArtifactKind::EbpfCtl,
        &redeemed.join_bundle.material.ebpf_ctl,
    )
    .map_err(|error| failure_message(&format!("invalid eBPF ctl install target: {error}")))?;
    let roles = NonEmptyRoleSet::try_new(
        plan_joined_machine_process_set(&machine_id, redeemed.roles)
            .roles()
            .to_vec(),
    )
    .map_err(|error| failure_message(&format!("invalid joined machine role set: {error:?}")))?;
    let runtime_nats_client_url =
        NatsClientUrl::try_new(redeemed.join_bundle.material.runtime_nats_url.as_str())
            .map_err(|error| failure_message(&format!("invalid runtime nats url: {error:?}")))?;
    let join_material_dir = PathBuf::from(KEEPER_STATE_DIR).join(JOIN_MATERIAL_DIR);
    let mut role_environment = PloyzdRoleEnvironmentTarget::default_path(
        machine_id.clone(),
        runtime_nats_client_url,
        RoleNatsCredentials::joined(&join_material_dir),
    );
    if let Some(public_ip) = machine_public_ip {
        role_environment = role_environment.with_machine_public_ip(public_ip);
    }

    Ok(RedeemedKeeperJoin::new(
        redeemed.operation_id,
        machine_id.clone(),
        KeeperJoinTarget::new(
            material,
            ployzd_artifact,
            DataplaneArtifactTargets::new(ebpf_bytecode_artifact, ebpf_ctl_artifact),
            roles,
            role_environment,
        ),
    )
    .with_callback_result(callback_result))
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
    use super::{keeper_join_target_with_public_ip, read_cloud_founder_bootstrap_result};
    use ployz_core::ids::{MachineId, OperationId};
    use ployz_core::install::{
        AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
        InstallSha256Digest, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
        MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTrustedNats,
        NatsMachineMaterialPaths,
    };
    use ployz_core::machine::{JoinTokenRedeemedAt, MachineName};
    use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
    use ployz_core::roles::InstallRolePolicy;
    use ployz_sdk_types::{MachineJoinRedeemResult, MachineJoinRedeemed};

    #[test]
    fn keeper_join_target_uses_runtime_nats_url_from_redeemed_bundle() {
        let redeemed = MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
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
            target.role_environment.render_for_role(
                &ployz_core::roles::DaemonProcessRole::Machine(
                    MachineId::try_new("machine_2").expect("valid machine id")
                )
            ),
            "PLOYZ_NATS_URL=nats://127.0.0.1:7422\nPLOYZ_NATS_CA_FILE=/var/lib/ployz/join-material.d/ca.pem\nPLOYZ_NATS_NKEY_SEED_FILE=/var/lib/ployz/join-material.d/nats.creds\nPLOYZ_MACHINE_ID=machine_2\nPLOYZ_EBPF_BYTECODE=/usr/local/lib/ployz/ebpf/ployz-ebpf-tc\nPLOYZ_EBPF_CTL=/usr/local/bin/ployz-ebpf-ctl\n"
        );
    }

    #[test]
    fn keeper_join_target_can_carry_machine_public_ip_from_bootstrap_env() {
        let redeemed = MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
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
                .render_for_role(&ployz_core::roles::DaemonProcessRole::Gateway)
                .contains("PLOYZ_MACHINE_PUBLIC_IP=203.0.113.20\n")
        );
    }

    #[test]
    fn cloud_founder_bootstrap_result_omits_local_operator_and_join_seeds() {
        let root =
            std::env::temp_dir().join(format!("ployz-cloud-founder-result-{}", std::process::id()));
        let nats_dir = root.join("nats");
        std::fs::create_dir_all(&nats_dir).expect("nats dir can be created");
        let material = NatsMachineMaterialPaths::new(nats_dir);
        std::fs::write(
            material.ca_file(),
            "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
        )
        .expect("ca can be written");
        std::fs::write(
            material.operator_seed_file(),
            "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM\n",
        )
        .expect("operator seed can be written");
        std::fs::write(
            material.join_seed_file(),
            "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM\n",
        )
        .expect("join seed can be written");

        let result = read_cloud_founder_bootstrap_result(
            &MachineId::try_new("core_1").expect("valid machine id"),
            &MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222").expect("valid nats url"),
            &material,
        )
        .expect("cloud result reads");
        let serialized = serde_json::to_string(&result).expect("cloud result serializes");

        assert!(serialized.contains("core_1"));
        assert!(serialized.contains("tls://203.0.113.10:4222"));
        assert!(!serialized.contains("SUAAAAAAAA"));
        assert!(!serialized.contains("SUBBBBBBBB"));
    }

    fn machine_join_bundle() -> MachineJoinBundle {
        MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                    .expect("valid runtime nats url"),
                trusted_nats: MachineJoinTrustedNats {
                    ca_pem: NatsCaCertificatePem::try_new(
                        "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                    )
                    .expect("valid ca pem"),
                },
                ployzd: join_artifact("/tmp/ployzd", "/usr/local/bin/ployzd"),
                ebpf_bytecode: join_artifact(
                    "/tmp/ployz-ebpf-tc",
                    "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                ),
                ebpf_ctl: join_artifact("/tmp/ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
            },
        }
    }

    fn join_artifact(source: &str, install_path: &str) -> InstallArtifactSpec {
        InstallArtifactSpec {
            version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
            source: InstallArtifactSource::try_new(source).expect("valid source"),
            sha256: InstallSha256Digest::try_new(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .expect("valid digest"),
            install_path: AbsoluteInstallPath::try_new(install_path).expect("valid install path"),
        }
    }

    fn machine_join_secret_delivery() -> MachineJoinSecretDelivery {
        MachineJoinSecretDelivery {
            nats_credentials: NatsUserSeed::try_new(
                "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM",
            )
            .expect("valid nats credentials"),
        }
    }
}
