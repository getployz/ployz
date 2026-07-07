use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::{
    DEFAULT_MACHINE_BOOTSTRAP_URL, FirstMachineInstallSpec, InstallArtifactVersion,
    MachineBootstrapUrl, MachineJoinClusterName, MachineJoinRuntimeNatsUrl,
    NatsMachineMaterialPaths,
};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DnsRole, GatewayRole, plan_joined_machine_process_set};
use ployz_core::security::NatsPrincipal;
use ployz_keeper::artifacts::{
    ArtifactKind, ArtifactTarget, DataplaneArtifactTargets, artifact_target,
};
use ployz_keeper::cli::{
    KeeperBootstrap, KeeperBootstrapMode, KeeperCommand, KeeperCoreDemote, KeeperCorePromote,
    KeeperSubstrateUpdate, load_command,
};
use ployz_keeper::cloud_client::get_text_url;
use ployz_keeper::command::{KeeperCommandRunner, SystemKeeperCommandRunner};
use ployz_keeper::core_demote::{CoreDemoteTarget, demote_local_core};
use ployz_keeper::executor::{KeeperPlanFailure, KeeperPlanTerminal, execute_keeper_plan};
use ployz_keeper::fsx::{FileMode, write_durable_file};
use ployz_keeper::join::{
    JOIN_CORE_SEEDS_FILE, JOIN_MATERIAL_DIR, JOIN_MATERIAL_FILE, JOIN_RECOVERY_KEY_FILE,
    JOIN_TRUSTED_CA_FILE, parse_machine_id_from_join_material,
};
use ployz_keeper::join_executor::{
    KeeperJoinRedeemer, KeeperJoinReporter, KeeperJoinTokenConsumer, RedeemedKeeperJoin,
    execute_keeper_join,
};
use ployz_keeper::local::{KeeperLocalConfig, KeeperLocalEffects};
use ployz_keeper::release_manifest::{
    ReleaseManifest, default_release_manifest_url, persisted_release_manifest_url,
    release_manifest_url,
};
use ployz_keeper::report::KeeperTextRecorder;
use ployz_keeper::steps::{
    CorePromoteTarget, FirstMachineInstallTarget, HostPrerequisite, JoinToken, KeeperJoinMaterial,
    KeeperJoinTarget, KeeperStep, KeeperStepPlan, NonEmptyRoleSet, PloyzdRoleEnvironmentTarget,
    RoleNatsCredentials, core_promote_plan, first_machine_install_plan,
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
use serde::Serialize;
mod cloud_bootstrap_runner;

const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
const PLOYZ_NATS_CA_FILE_ENV: &str = "PLOYZ_NATS_CA_FILE";
const PLOYZ_JOIN_NKEY_SEED_ENV: &str = "PLOYZ_JOIN_NKEY_SEED";
const PLOYZ_MACHINE_PUBLIC_IP_ENV: &str = "PLOYZ_MACHINE_PUBLIC_IP";
const PLOYZ_MACHINE_ID_ENV: &str = "PLOYZ_MACHINE_ID";
const PLOYZ_GATEWAY_ENV: &str = "PLOYZ_GATEWAY";
const PLOYZ_DNS_ENV: &str = "PLOYZ_DNS";
const PLOYZ_MACHINE_BOOTSTRAP_URL_ENV: &str = "PLOYZ_MACHINE_BOOTSTRAP_URL";
const PLOYZ_MACHINE_JOIN_CLUSTER_NAME_ENV: &str = "PLOYZ_MACHINE_JOIN_CLUSTER_NAME";
const PLOYZ_MACHINE_JOIN_NATS_URL_ENV: &str = "PLOYZ_MACHINE_JOIN_NATS_URL";
const PLOYZ_RELEASE_MANIFEST_URL_ENV: &str = "PLOYZ_RELEASE_MANIFEST_URL";
const DEFAULT_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounded redeem retry while the core mints this machine's credential:
/// the join token TTL is 600 seconds, so stop well within it.
const REDEEM_MATERIAL_ATTEMPTS: u32 = 150;
const REDEEM_MATERIAL_RETRY_DELAY: Duration = Duration::from_secs(2);
const KEEPER_STATE_DIR: &str = "/var/lib/ployz";
const SUBSTRATE_VERSION_FILE: &str = "substrate-version.json";
const FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN: &str = "ployz-first-machine-bootstrap-result begin";
const FIRST_MACHINE_BOOTSTRAP_RESULT_END: &str = "ployz-first-machine-bootstrap-result end";
const CORE_PROMOTE_RESULT_BEGIN: &str = "ployz-core-promote-result begin";
const CORE_PROMOTE_RESULT_END: &str = "ployz-core-promote-result end";
const CLOUD_BOOTSTRAP_MAX_POLLS: u16 = 900;

fn main() -> ExitCode {
    match load_command(std::env::args_os().skip(1)) {
        Ok(KeeperCommand::Start(startup)) => {
            if let Some(join) = &startup.join {
                let stdout = std::io::stdout();
                let mut recorder = KeeperTextRecorder::new(stdout.lock());
                let execution = run_startup_join(&join.token, join.file.clone(), &mut recorder);
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
        Ok(KeeperCommand::SubstrateUpdate(update)) => run_substrate_update_command(update),
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
        Ok(KeeperCommand::CorePromote(promote)) => run_core_promote_command(promote),
        Ok(KeeperCommand::CoreDemote(demote)) => run_core_demote_command(demote),
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

fn run_core_demote_command(demote: KeeperCoreDemote) -> ExitCode {
    let mut runner = SystemKeeperCommandRunner::default();
    let target = CoreDemoteTarget::new(demote.successor_nats_url);
    match demote_local_core(&target, &mut runner) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ployz-keeper core-demote failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_substrate_update_command(update: KeeperSubstrateUpdate) -> ExitCode {
    let units = match installed_update_units(Path::new("/etc/systemd/system")) {
        Ok(units) if !units.is_empty() => units,
        Ok(_) => {
            eprintln!("no installed Ployz substrate units found");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("failed to inspect installed Ployz substrate units: {error}");
            return ExitCode::FAILURE;
        }
    };
    let manifest_url = release_manifest_url(&update.version);
    let manifest = match load_versioned_release_manifest(&manifest_url) {
        Ok(manifest) => manifest,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let artifacts = match manifest.install_artifacts() {
        Ok(artifacts) => artifacts,
        Err(message) => {
            eprintln!("release manifest is invalid: {message}");
            return ExitCode::FAILURE;
        }
    };
    let ployzd_version = artifacts.ployzd.version.clone();
    let ployzd = match artifact_target(ArtifactKind::Ployzd, &artifacts.ployzd) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("release manifest ployzd artifact is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };
    let ebpf_bytecode = match artifact_target(ArtifactKind::EbpfBytecode, &artifacts.ebpf_bytecode)
    {
        Ok(target) => target,
        Err(error) => {
            eprintln!("release manifest eBPF bytecode artifact is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };
    let ebpf_ctl = match artifact_target(ArtifactKind::EbpfCtl, &artifacts.ebpf_ctl) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("release manifest eBPF controller artifact is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };
    let nats_server_spec = ployz_core::install::InstallArtifactSpec {
        version: artifacts.nats_server.version,
        source: artifacts.nats_server.source,
        sha256: artifacts.nats_server.sha256,
        install_path: artifacts.nats_server.binary,
    };
    let nats_server = match artifact_target(ArtifactKind::NatsServer, &nats_server_spec) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("release manifest NATS artifact is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut steps = vec![
        KeeperStep::VerifyHost(HostPrerequisite::LinuxRootSystemd),
        KeeperStep::PrepareDataplaneHost,
        KeeperStep::InstallArtifact(ployzd),
        KeeperStep::InstallArtifact(ebpf_bytecode),
        KeeperStep::InstallArtifact(ebpf_ctl),
    ];
    if units
        .iter()
        .any(|unit| matches!(unit, InstalledUpdateUnit::Nats))
    {
        steps.push(KeeperStep::InstallArtifact(nats_server));
    }
    let plan = KeeperStepPlan::from_steps(steps);
    let stdout = std::io::stdout();
    let mut recorder = KeeperTextRecorder::new(stdout.lock());
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: KEEPER_STATE_DIR.into(),
        },
        SystemKeeperCommandRunner::default(),
    );
    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);
    match execution.terminal {
        KeeperPlanTerminal::Completed => {}
        KeeperPlanTerminal::Failed(failure) => {
            eprintln!(
                "ployz-keeper substrate-update failed: {}",
                failure_summary(&failure)
            );
            return ExitCode::FAILURE;
        }
    }
    let mut runner = SystemKeeperCommandRunner::default();
    if let Err(message) = restart_installed_update_units(&units, &mut runner) {
        eprintln!(
            "ployz-keeper substrate-update restart failed: {}",
            message.as_str()
        );
        return ExitCode::FAILURE;
    }
    if let Some(operation_id) = &update.operation_id
        && let Err(message) = write_substrate_update_evidence(operation_id, ployzd_version)
    {
        eprintln!(
            "failed to write substrate update evidence: {}",
            message.as_str()
        );
        return ExitCode::FAILURE;
    }
    println!(
        "substrate updated to {} and restarted {} unit(s)",
        update.version.as_str(),
        units.len()
    );
    ExitCode::SUCCESS
}

#[derive(Serialize)]
struct SubstrateUpdateEvidence {
    operation_id: OperationId,
    ployzd: InstallArtifactVersion,
}

fn write_substrate_update_evidence(
    operation_id: &OperationId,
    ployzd: InstallArtifactVersion,
) -> Result<(), FailureMessage> {
    let state_dir = Path::new(KEEPER_STATE_DIR);
    std::fs::create_dir_all(state_dir).map_err(|error| {
        FailureMessage::try_new(format!(
            "failed to create keeper state directory {}: {error}",
            state_dir.display()
        ))
        .expect("keeper state directory failure message is non-empty")
    })?;
    let contents = serde_json::to_vec(&SubstrateUpdateEvidence {
        operation_id: operation_id.clone(),
        ployzd,
    })
    .map_err(|error| {
        FailureMessage::try_new(format!(
            "failed to encode substrate update evidence: {error}"
        ))
        .expect("substrate update evidence encode message is non-empty")
    })?;
    write_durable_file(
        state_dir,
        SUBSTRATE_VERSION_FILE,
        FileMode::Plain,
        &contents,
    )
}

fn load_versioned_release_manifest(url: &str) -> Result<ReleaseManifest, String> {
    let contents = get_text_url(url)
        .map_err(|error| format!("failed to download release manifest {url}: {error}"))?;
    ReleaseManifest::parse(&contents)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InstalledUpdateUnit {
    Nats,
    Ployzd(String),
}

impl InstalledUpdateUnit {
    fn unit_name(&self) -> &str {
        match self {
            Self::Nats => "nats-server.service",
            Self::Ployzd(unit) => unit,
        }
    }
}

fn installed_update_units(systemd_dir: &Path) -> Result<Vec<InstalledUpdateUnit>, std::io::Error> {
    let mut units = Vec::new();
    if systemd_dir.join("nats-server.service").is_file() {
        units.push(InstalledUpdateUnit::Nats);
    }
    for entry in std::fs::read_dir(systemd_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if file_name.starts_with("ployzd-") && file_name.ends_with(".service") {
            units.push(InstalledUpdateUnit::Ployzd(file_name.to_owned()));
        }
    }
    units.sort_by(|left, right| left.unit_name().cmp(right.unit_name()));
    units.dedup();
    Ok(units)
}

fn restart_installed_update_units(
    units: &[InstalledUpdateUnit],
    runner: &mut impl KeeperCommandRunner,
) -> Result<(), FailureMessage> {
    runner.systemctl(&["daemon-reload"])?;
    for unit in units {
        runner.systemctl(&["restart", unit.unit_name()])?;
    }
    Ok(())
}

fn run_bootstrap_command(bootstrap: KeeperBootstrap) -> ExitCode {
    match bootstrap.mode {
        KeeperBootstrapMode::LocalGuidance => {
            eprintln!("Use local CLI setup from your workstation:");
            eprintln!("  ployzctl machine init USER@HOST");
            eprintln!("Or opt in to Cloud:");
            eprintln!("  sudo ployz-keeper bootstrap cloud");
            ExitCode::FAILURE
        }
        KeeperBootstrapMode::Cloud { cloud_host } => {
            let host = choose_cloud_host(cloud_host);
            cloud_bootstrap_runner::run_interactive_cloud_bootstrap(host.as_str())
        }
        KeeperBootstrapMode::Core => run_local_core_bootstrap(),
        KeeperBootstrapMode::Join { join_token } => run_bootstrap_join(&join_token),
    }
}

fn choose_cloud_host(
    cloud_host: Option<ployz_keeper::cli::CloudHost>,
) -> ployz_keeper::cli::CloudHost {
    cloud_host.unwrap_or_default()
}

fn run_local_core_bootstrap() -> ExitCode {
    let target = match local_core_target_from_env() {
        Ok(target) => target,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let machine_id = target.machine_id.clone();
    let nats_material = target.nats_material.clone();
    let runtime_nats_url = target.machine_join_runtime_nats_url().clone();
    let stdout = std::io::stdout();
    let mut recorder = KeeperTextRecorder::new(stdout.lock());
    let execution = run_first_machine_install(target, &mut recorder);
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
                "ployz-keeper bootstrap core failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
    }
}

fn local_core_target_from_env() -> Result<FirstMachineInstallTarget, String> {
    let manifest = load_local_release_manifest()?;
    let machine_id = env_machine_id(PLOYZ_MACHINE_ID_ENV)?;
    let runtime_nats_url = env_runtime_nats_url(PLOYZ_MACHINE_JOIN_NATS_URL_ENV)?;
    let cluster_name = env_cluster_name(PLOYZ_MACHINE_JOIN_CLUSTER_NAME_ENV)?;
    let bootstrap_url = optional_env(PLOYZ_MACHINE_BOOTSTRAP_URL_ENV)
        .map(MachineBootstrapUrl::try_new)
        .transpose()
        .map_err(|error| format!("{PLOYZ_MACHINE_BOOTSTRAP_URL_ENV} is invalid: {error}"))?
        .or_else(|| MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL).ok());

    let install = FirstMachineInstallSpec {
        machine_id,
        gateway: env_gateway_role(PLOYZ_GATEWAY_ENV)?,
        dns: env_dns_role(PLOYZ_DNS_ENV)?,
        machine_public_ip: local_core_machine_public_ip_from_env()?,
        machine_bootstrap_url: bootstrap_url,
        machine_join_template_file: None,
        machine_join_cluster_name: cluster_name,
        machine_join_runtime_nats_url: runtime_nats_url,
        artifacts: manifest.install_artifacts()?,
    };

    ployz_keeper::cli::first_machine_install_target_from_spec(install)
        .map_err(|error| error.to_string())
}

fn load_local_release_manifest() -> Result<ReleaseManifest, String> {
    let url = optional_env(PLOYZ_RELEASE_MANIFEST_URL_ENV)
        .or_else(|| persisted_release_manifest_url(Path::new("/etc/ployz/release.env")).ok())
        .unwrap_or_else(default_release_manifest_url);
    let contents = get_text_url(&url)
        .map_err(|error| format!("failed to download release manifest {url}: {error}"))?;
    ReleaseManifest::parse(&contents)
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_machine_id(name: &str) -> Result<MachineId, String> {
    let value = optional_env(name).ok_or_else(|| format!("{name} is required"))?;
    MachineId::try_new(value.clone())
        .map_err(|error| format!("{name}={value:?} is invalid: {error}"))
}

fn env_runtime_nats_url(name: &str) -> Result<MachineJoinRuntimeNatsUrl, String> {
    let value = optional_env(name).ok_or_else(|| format!("{name} is required"))?;
    MachineJoinRuntimeNatsUrl::try_new(value.clone())
        .map_err(|error| format!("{name}={value:?} is invalid: {error}"))
}

fn env_cluster_name(name: &str) -> Result<MachineJoinClusterName, String> {
    let value = optional_env(name).ok_or_else(|| format!("{name} is required"))?;
    MachineJoinClusterName::try_new(value.clone())
        .map_err(|error| format!("{name}={value:?} is invalid: {error}"))
}

fn env_gateway_role(name: &str) -> Result<GatewayRole, String> {
    match optional_env(name).as_deref() {
        None | Some("install") => Ok(GatewayRole::Install),
        Some("skip") => Ok(GatewayRole::Skip),
        Some(value) => Err(format!("{name}={value:?} must be install or skip")),
    }
}

fn env_dns_role(name: &str) -> Result<DnsRole, String> {
    match optional_env(name).as_deref() {
        None | Some("install") => Ok(DnsRole::Install),
        Some("skip") => Ok(DnsRole::Skip),
        Some(value) => Err(format!("{name}={value:?} must be install or skip")),
    }
}

fn local_core_machine_public_ip_from_env() -> Result<Option<std::net::IpAddr>, String> {
    let Some(value) = optional_env(PLOYZ_MACHINE_PUBLIC_IP_ENV) else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|error| format!("{PLOYZ_MACHINE_PUBLIC_IP_ENV}={value:?} is invalid: {error}"))
}

fn run_bootstrap_join(join_token: &JoinToken) -> ExitCode {
    let stdout = std::io::stdout();
    let mut recorder = KeeperTextRecorder::new(stdout.lock());
    let execution = run_join_with_consumer(join_token, CloudJoinTokenConsumer, &mut recorder);
    match execution.terminal {
        KeeperPlanTerminal::Completed => ExitCode::SUCCESS,
        KeeperPlanTerminal::Failed(failure) => {
            eprintln!(
                "ployz-keeper bootstrap join failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
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

fn run_startup_join(
    token: &JoinToken,
    join_token_file: std::path::PathBuf,
    recorder: &mut impl ployz_keeper::executor::KeeperStepRecorder,
) -> ployz_keeper::executor::KeeperPlanExecution {
    run_join_with_consumer(
        token,
        StartupJoinTokenConsumer { join_token_file },
        recorder,
    )
}

fn run_join_with_consumer(
    token: &JoinToken,
    mut token_consumer: impl KeeperJoinTokenConsumer,
    recorder: &mut impl ployz_keeper::executor::KeeperStepRecorder,
) -> ployz_keeper::executor::KeeperPlanExecution {
    let mut redeemer = JoinRedeemer::from_env();
    let mut reporter = JoinReporter::from_env(token.clone());
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

fn run_core_promote_command(promote: KeeperCorePromote) -> ExitCode {
    // The release supplies the core nats-server + ployzd binaries; it defaults to
    // this keeper binary's own version (the cluster release on a joined machine),
    // with --version as an override. Everything else is read from machine state.
    let manifest_url = match &promote.version {
        Some(version) => release_manifest_url(version),
        None => default_release_manifest_url(),
    };
    let manifest = match load_versioned_release_manifest(&manifest_url) {
        Ok(manifest) => manifest,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let artifacts = match manifest.install_artifacts() {
        Ok(artifacts) => artifacts,
        Err(message) => {
            eprintln!("release manifest is invalid: {message}");
            return ExitCode::FAILURE;
        }
    };
    let nats_server_artifact = match artifact_target(
        ArtifactKind::NatsServer,
        &ployz_core::install::InstallArtifactSpec {
            version: artifacts.nats_server.version,
            source: artifacts.nats_server.source,
            sha256: artifacts.nats_server.sha256,
            install_path: artifacts.nats_server.binary,
        },
    ) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("release manifest NATS artifact is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };
    let ployzd_artifact = match artifact_target(ArtifactKind::Ployzd, &artifacts.ployzd) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("release manifest ployzd artifact is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };

    let (target, access) = match resolve_core_promote_target(nats_server_artifact, ployzd_artifact)
    {
        Ok(resolved) => resolved,
        Err(message) => {
            eprintln!("ployz-keeper core-promote: {message}");
            return ExitCode::FAILURE;
        }
    };

    let stdout = std::io::stdout();
    let mut recorder = KeeperTextRecorder::new(stdout.lock());
    let plan = core_promote_plan(target);
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: KEEPER_STATE_DIR.into(),
        },
        SystemKeeperCommandRunner::default(),
    );
    match execute_keeper_plan(&plan, &mut effects, &mut recorder).terminal {
        KeeperPlanTerminal::Completed => {
            drop(recorder);
            print_core_promote_result(&access);
            ExitCode::SUCCESS
        }
        KeeperPlanTerminal::Failed(failure) => {
            eprintln!(
                "ployz-keeper core-promote failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
    }
}

/// Read the promotion inputs from this machine's own state: the CA + wrapped
/// recovery key from join material (decrypted with `PLOYZ_RECOVERY_SECRET`), and
/// this machine's control endpoints from the local mirror.
fn resolve_core_promote_target(
    nats_server_artifact: ArtifactTarget,
    ployzd_artifact: ArtifactTarget,
) -> Result<(CorePromoteTarget, PromotedCoreAccess), String> {
    let join_dir = PathBuf::from(KEEPER_STATE_DIR).join(JOIN_MATERIAL_DIR);
    let machine_id = parse_machine_id_from_join_material(&read_promote_file(
        &join_dir.join(JOIN_MATERIAL_FILE),
    )?)
    .ok_or("join material carries no machine id — is this a joined machine?")?;
    let ca = ployz_core::nats_config::NatsCaCertificatePem::try_new(read_promote_file(
        &join_dir.join(JOIN_TRUSTED_CA_FILE),
    )?)
    .map_err(|error| format!("invalid trusted CA in join material: {error}"))?;
    let wrapped = std::fs::read(join_dir.join(JOIN_RECOVERY_KEY_FILE)).map_err(|error| {
        format!("cannot read the wrapped CA key (was this machine joined with recovery material?): {error}")
    })?;
    let wrapped_seeds = ployz_core::install::WrappedCoreSeeds::new(
        std::fs::read(join_dir.join(JOIN_CORE_SEEDS_FILE))
            .map_err(|error| format!("cannot read the wrapped core seeds: {error}"))?,
    );
    let secret = read_recovery_secret()?;
    let ca_key_pem = String::from_utf8(
        ployz_keeper::recovery_secret::unwrap(&secret, &wrapped).map_err(|error| {
            format!("cannot decrypt the CA key (wrong recovery secret?): {error}")
        })?,
    )
    .map_err(|_| "decrypted CA key is not valid UTF-8".to_owned())?;
    let core_seeds = ployz_keeper::nats_identity::unwrap_core_seeds(&secret, &wrapped_seeds)
        .map_err(|error| format!("{error}"))?;

    // The machine persists its mirror beside its seed file; on a joined machine
    // that seed lives in the join-material directory (nats.creds), so the mirror is
    // there too — not the first-machine `/var/lib/ployz/nats` layout.
    let mirror_path = join_dir.join("intent-mirror.json");
    let snapshot: ployz_core::state::IntentSnapshot =
        serde_json::from_str(&read_promote_file(&mirror_path)?).map_err(|error| {
            format!(
                "cannot parse intent mirror {}: {error}",
                mirror_path.display()
            )
        })?;
    // A promoted core must serve on a fleet-dialable address: without advertised
    // control endpoints the listener + cert would be loopback-only and every other
    // machine's failover pool could never reach it. Require at least one.
    let control_endpoints = snapshot
        .control_endpoints_of(&machine_id)
        .ok_or_else(|| {
            "this machine has no advertised control endpoint in the mirror — the core \
         never recorded its reachability, so it cannot be promoted to a core the \
         fleet can dial"
                .to_owned()
        })?
        .to_vec();
    let machine_public_ip = control_endpoints
        .first()
        .copied()
        .expect("control endpoint list is non-empty");

    let hostname = gethostname::gethostname().into_string().ok();
    let sans = ployz_keeper::nats_identity::ServerCertificateSans::try_new_many(
        control_endpoints.clone(),
        hostname,
    )
    .map_err(|error| format!("{error}"))?;
    // Reuse the old core's principals from the pre-positioned seeds; the grant set is
    // seeded into the store from the mirror by control at startup (ADR 0031).
    let nats_identity =
        ployz_keeper::nats_identity::resurrect_core_identity(ca, ca_key_pem, core_seeds, &sans)
            .map_err(|error| format!("{error}"))?;

    // Promotion reuses the old core's principals, so the operator's existing ployzctl
    // credential still authenticates — it only needs the promoted core's new address.
    let access = PromotedCoreAccess {
        ca_pem: nats_identity.ca.as_str().to_owned(),
        nats_urls: control_endpoints
            .iter()
            .map(|endpoint| format!("tls://{}", SocketAddr::new(*endpoint, 4222)))
            .collect(),
    };

    let target = CorePromoteTarget::assemble(
        machine_id,
        nats_server_artifact,
        ployzd_artifact,
        nats_identity,
        ployz_core::install::WrappedCaKey::new(wrapped),
        wrapped_seeds,
        Some(machine_public_ip),
        mirror_path,
    );
    Ok((target, access))
}

/// Where the operator points their existing `ployzctl` context after promotion: the
/// promoted core's new TLS address. Its principals are reused, so the operator's
/// credential still works; the CA is printed so a fresh context can be built from
/// this block alone.
struct PromotedCoreAccess {
    ca_pem: String,
    nats_urls: Vec<String>,
}

fn print_core_promote_result(access: &PromotedCoreAccess) {
    let result = serde_json::json!({
        "nats_urls": access.nats_urls,
        "ca_pem": access.ca_pem,
    });
    println!("{CORE_PROMOTE_RESULT_BEGIN}");
    println!(
        "{}",
        serde_json::to_string(&result).expect("core-promote result json serializes")
    );
    println!("{CORE_PROMOTE_RESULT_END}");
}

fn read_promote_file(path: &std::path::Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// The cluster recovery secret: `PLOYZ_RECOVERY_SECRET` when set (automation and
/// Cloud SSH forced commands), otherwise prompted (hidden) from an interactive
/// terminal. Never on argv or in shell history either way.
fn read_recovery_secret() -> Result<String, String> {
    if let Some(secret) = std::env::var("PLOYZ_RECOVERY_SECRET")
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Ok(secret);
    }
    if std::io::stdin().is_terminal() {
        rpassword::prompt_password("Cluster recovery secret: ")
            .map_err(|error| format!("failed to read recovery secret: {error}"))
    } else {
        Err("set PLOYZ_RECOVERY_SECRET, or run interactively to be prompted for it".to_owned())
    }
}

fn failure_summary(failure: &KeeperPlanFailure) -> &str {
    match failure {
        KeeperPlanFailure::Step(step) => step.message.as_str(),
        KeeperPlanFailure::Record(record) => record.message.as_str(),
    }
}

struct JoinRedeemer {
    connect: Result<NatsConnectConfig, FailureMessage>,
}

impl JoinRedeemer {
    fn new(connect: Result<NatsConnectConfig, FailureMessage>) -> Self {
        Self { connect }
    }

    fn from_env() -> Self {
        Self::new(
            load_join_connect_from_env().map_err(|error| failure_message(&format!("{error}"))),
        )
    }
}

impl KeeperJoinRedeemer for JoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedKeeperJoin, FailureMessage> {
        let connect = self.connect.clone()?;
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

        keeper_join_target(redeemed)
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum KeeperNatsConnectError {
    #[error("{PLOYZ_NATS_URL_ENV} is required")]
    MissingUrl,
    #[error("{PLOYZ_NATS_URL_ENV}={value:?} is invalid")]
    InvalidUrl {
        value: String,
        #[source]
        source: NatsClientUrlError,
    },
    #[error("{PLOYZ_NATS_CA_FILE_ENV} is required")]
    MissingCaFile,
    #[error("{PLOYZ_JOIN_NKEY_SEED_ENV} is required")]
    MissingJoinSeed,
    #[error("{PLOYZ_JOIN_NKEY_SEED_ENV} must be an SU-prefixed user seed")]
    InvalidJoinSeed,
}

fn keeper_join_target(redeemed: MachineJoinRedeemed) -> Result<RedeemedKeeperJoin, FailureMessage> {
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
    let role_environment = PloyzdRoleEnvironmentTarget::default_path(
        machine_id.clone(),
        runtime_nats_client_url,
        RoleNatsCredentials::joined(&join_material_dir),
    );

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
    use std::fs;

    use super::{
        InstalledUpdateUnit, installed_update_units, keeper_join_target,
        read_cloud_founder_bootstrap_result,
    };
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
    fn installed_update_units_discovers_nats_and_ployzd_units() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join("nats-server.service"), "").expect("write nats unit");
        fs::write(root.path().join("ployzd-gateway.service"), "").expect("write gateway unit");
        fs::write(root.path().join("ployzd-machine-machine_1.service"), "")
            .expect("write machine unit");
        fs::write(root.path().join("docker.service"), "").expect("write unrelated unit");

        assert_eq!(
            installed_update_units(root.path()).expect("units load"),
            vec![
                InstalledUpdateUnit::Nats,
                InstalledUpdateUnit::Ployzd("ployzd-gateway.service".to_owned()),
                InstalledUpdateUnit::Ployzd("ployzd-machine-machine_1.service".to_owned()),
            ]
        );
    }

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

        let target = keeper_join_target(redeemed)
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
    fn keeper_join_target_does_not_render_machine_public_ip_env() {
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

        let target = keeper_join_target(redeemed)
            .expect("redeemed bundle converts")
            .target;

        let rendered = target
            .role_environment
            .render_for_role(&ployz_core::roles::DaemonProcessRole::Gateway);
        assert!(!rendered.contains("PLOYZ_MACHINE_PUBLIC_IP="));
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
                recovery_key_wrapped: ployz_core::install::WrappedCaKey::new(vec![1, 2, 3]),
                core_seeds_wrapped: ployz_core::install::WrappedCoreSeeds::new(vec![4, 5, 6]),
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
