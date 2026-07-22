//! Explicit non-keeper Substrate Update with preflight before activation.

use std::path::Path;
use std::process::ExitCode;

use super::assigned_substrate::load_assigned_substrate_state;
use super::installed_substrate::InstalledSubstrateRelease;
use super::machine_join::prepare_machine_join_template_release_promotion;
use crate::cli::{HostRunnerSubstrateUpdate, HostRunnerSubstrateUpdateSource};
use crate::execution::supervisor::execute_supervisor_commands;
use crate::execution::{
    ArtifactKind, FileMode, HostRunnerCommandRunner, HostRunnerLocalConfig, HostRunnerLocalEffects,
    SupervisorBackend, SupervisorDirectories, SupervisorUnitTarget, SystemHostRunnerCommandRunner,
    artifact_target, detect_host_platform, write_durable_file,
};
use crate::plan::{HostPrerequisite, HostRunnerStep, HostRunnerStepPlan, HostRunnerTextRecorder};
use crate::plan::{HostRunnerPlanTerminal, execute_host_runner_plan};
use crate::release_manifest::{ReleaseManifest, ReleasePlatform, release_manifest_url};
use ployz_core::ids::OperationId;
use ployz_core::install::{InstallArtifactVersion, MachineJoinSubstrateRelease};
use ployz_core::operation::FailureMessage;
use serde::Serialize;

use crate::env_config::{default_machine_join_template_file, load_versioned_release_manifest};
use crate::runtime::{HOST_RUNNER_STATE_DIR, SUBSTRATE_VERSION_FILE, failure_summary};

const MACHINE_ENV_PATH: &str = "/etc/ployz/ployzd-machine.env";
const MACHINE_JOIN_TEMPLATE_FILE_ENV: &str = "PLOYZ_MACHINE_JOIN_TEMPLATE_FILE";

pub(crate) fn run_substrate_update_command(update: HostRunnerSubstrateUpdate) -> ExitCode {
    let assigned_substrate = match load_assigned_substrate_state(Path::new(HOST_RUNNER_STATE_DIR)) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("failed to load assigned substrate state: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut runner = SystemHostRunnerCommandRunner::default();
    let supervisor = match runner.read_os_release().and_then(|os_release| {
        detect_host_platform(&os_release)
            .map(|profile| SupervisorBackend::from(profile.supervisor()))
            .map_err(|error| failure_message(error.to_string()))
    }) {
        Ok(supervisor) => supervisor,
        Err(message) => {
            eprintln!("failed to detect host platform: {}", message.as_str());
            return ExitCode::FAILURE;
        }
    };
    let supervisor_dirs = SupervisorDirectories::host_defaults();
    let units = match installed_update_units(supervisor_dirs.directory(supervisor), supervisor) {
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
    let update_label = update.source.label();
    let template_path = if should_promote_join_template(&update.source) {
        match inherited_machine_join_template_file().and_then(|inherited| {
            resolve_machine_join_template_file(inherited, Path::new(MACHINE_ENV_PATH))
        }) {
            Ok(path) => Some(path),
            Err(message) => {
                eprintln!("ployz host substrate-update join-template path is invalid: {message}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let loaded_manifest = match load_substrate_update_manifest(&update.source) {
        Ok(manifest) => manifest,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let manifest = &loaded_manifest.manifest;
    let local_platform =
        match ReleasePlatform::from_target(std::env::consts::OS, std::env::consts::ARCH) {
            Ok(platform) => platform,
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::FAILURE;
            }
        };
    let artifacts = match manifest.install_artifacts_for(local_platform) {
        Ok(artifacts) => artifacts,
        Err(message) => {
            eprintln!("release manifest is invalid: {message}");
            return ExitCode::FAILURE;
        }
    };
    let ployzd_version = artifacts.ployzd.version.clone();
    let target_version = manifest.version().clone();
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
    let railpack = match artifact_target(ArtifactKind::Railpack, &artifacts.railpack) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("release manifest Railpack artifact is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };
    let nats_server = match artifacts.nats_server {
        Some(spec) => {
            let nats_server_spec = ployz_core::install::InstallArtifactSpec {
                version: spec.version,
                source: spec.source,
                sha256: spec.sha256,
                install_path: spec.binary,
            };
            match artifact_target(ArtifactKind::NatsServer, &nats_server_spec) {
                Ok(target) => Some(target),
                Err(error) => {
                    eprintln!("release manifest NATS artifact is invalid: {error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        None => None,
    };
    let mut steps = vec![
        HostRunnerStep::VerifyHost(HostPrerequisite::LinuxRoot),
        HostRunnerStep::PreflightHostPorts(assigned_substrate.clone()),
        HostRunnerStep::AssureHostPorts(assigned_substrate),
        HostRunnerStep::PrepareDataplaneHost,
    ];
    if units
        .iter()
        .any(|unit| matches!(unit, InstalledUpdateUnit::Machine(_)))
    {
        steps.push(HostRunnerStep::PrepareBuildHost);
    }
    steps.extend([
        HostRunnerStep::InstallArtifact(railpack),
        HostRunnerStep::InstallArtifact(ployzd),
        HostRunnerStep::InstallArtifact(ebpf_bytecode),
        HostRunnerStep::InstallArtifact(ebpf_ctl),
    ]);
    if let Some(nats_server) = nats_server
        && units
            .iter()
            .any(|unit| matches!(unit, InstalledUpdateUnit::Nats))
    {
        steps.push(HostRunnerStep::InstallArtifact(nats_server));
    }
    let installed_release = match loaded_manifest.installed_release() {
        Ok(installed) => installed,
        Err(message) => {
            eprintln!("release manifest provenance is invalid: {message}");
            return ExitCode::FAILURE;
        }
    };
    steps.push(HostRunnerStep::StoreInstalledSubstrateRelease(
        installed_release,
    ));
    let plan = HostRunnerStepPlan::from_steps(steps);
    let template_promotion = match template_path {
        Some(path) => {
            match prepare_machine_join_template_release_promotion(
                Path::new(path.as_str()),
                &target_version,
            ) {
                Ok(promotion) => Some(promotion),
                Err(message) => {
                    eprintln!(
                        "ployz host substrate-update join-template promotion failed: {}",
                        message.as_str()
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
        None => None,
    };
    let stdout = std::io::stdout();
    let mut recorder = HostRunnerTextRecorder::new(stdout.lock());
    let mut effects = HostRunnerLocalEffects::new(
        HostRunnerLocalConfig {
            supervisor_dirs: SupervisorDirectories::host_defaults(),
            state_dir: HOST_RUNNER_STATE_DIR.into(),
            docker_daemon_config: "/etc/docker/daemon.json".into(),
            docker_repository_dir: "/etc/yum.repos.d".into(),
        },
        SystemHostRunnerCommandRunner::default(),
    );
    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);
    match execution.terminal {
        HostRunnerPlanTerminal::Completed => {}
        HostRunnerPlanTerminal::Failed(failure) => {
            eprintln!(
                "ployz host substrate-update failed: {}",
                failure_summary(&failure)
            );
            return ExitCode::FAILURE;
        }
    }
    if let Some(template_promotion) = template_promotion
        && let Err(message) = template_promotion.commit()
    {
        eprintln!(
            "ployz host substrate-update join-template promotion failed: {}",
            message.as_str()
        );
        return ExitCode::FAILURE;
    }
    if let Err(message) = restart_installed_update_units(&units, supervisor, &mut runner) {
        eprintln!(
            "ployz host substrate-update restart failed: {}",
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
        update_label,
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
    let state_dir = Path::new(HOST_RUNNER_STATE_DIR);
    std::fs::create_dir_all(state_dir).map_err(|error| {
        FailureMessage::try_new(format!(
            "failed to create Host Runner state directory {}: {error}",
            state_dir.display()
        ))
        .expect("Host Runner state directory failure message is non-empty")
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

fn load_substrate_update_manifest(
    source: &HostRunnerSubstrateUpdateSource,
) -> Result<LoadedSubstrateUpdateManifest, String> {
    match source {
        HostRunnerSubstrateUpdateSource::Version(version) => {
            let manifest = load_versioned_release_manifest(&release_manifest_url(version))?;
            Ok(LoadedSubstrateUpdateManifest {
                manifest,
                persisted_contents: None,
            })
        }
        HostRunnerSubstrateUpdateSource::ManifestFile(path) => {
            let contents = std::fs::read_to_string(path).map_err(|error| {
                format!(
                    "failed to read release manifest {}: {error}",
                    path.display()
                )
            })?;
            let manifest = ReleaseManifest::parse(&contents).map_err(|error| error.to_string())?;
            Ok(LoadedSubstrateUpdateManifest {
                manifest,
                persisted_contents: Some(contents),
            })
        }
    }
}

struct LoadedSubstrateUpdateManifest {
    manifest: ReleaseManifest,
    persisted_contents: Option<String>,
}

impl LoadedSubstrateUpdateManifest {
    fn installed_release(&self) -> Result<InstalledSubstrateRelease, String> {
        let release = MachineJoinSubstrateRelease {
            version: self.manifest.version().clone(),
        };
        match &self.persisted_contents {
            Some(contents) => {
                InstalledSubstrateRelease::persisted_manifest(release, contents.clone())
            }
            None => Ok(InstalledSubstrateRelease::public_exact(release)),
        }
    }
}

fn should_promote_join_template(source: &HostRunnerSubstrateUpdateSource) -> bool {
    matches!(source, HostRunnerSubstrateUpdateSource::Version(_))
}

fn inherited_machine_join_template_file() -> Result<Option<String>, String> {
    match std::env::var(MACHINE_JOIN_TEMPLATE_FILE_ENV) {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!(
            "{MACHINE_JOIN_TEMPLATE_FILE_ENV} is not valid UTF-8"
        )),
    }
}

fn resolve_machine_join_template_file(
    inherited: Option<String>,
    machine_env_path: &Path,
) -> Result<ployz_core::install::AbsoluteInstallPath, String> {
    if let Some(value) = inherited {
        return ployz_core::install::AbsoluteInstallPath::try_new(value.clone()).map_err(|error| {
            format!("{MACHINE_JOIN_TEMPLATE_FILE_ENV}={value:?} is invalid: {error}")
        });
    }
    let contents = match std::fs::read_to_string(machine_env_path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "failed to read existing machine environment {}: {error}",
                machine_env_path.display()
            ));
        }
    };
    if let Some(value) = contents.as_deref().and_then(|contents| {
        contents.lines().find_map(|line| {
            line.strip_prefix(MACHINE_JOIN_TEMPLATE_FILE_ENV)?
                .strip_prefix('=')
                .map(str::to_owned)
        })
    }) {
        return ployz_core::install::AbsoluteInstallPath::try_new(value.clone()).map_err(|error| {
            format!(
                "{} has invalid {MACHINE_JOIN_TEMPLATE_FILE_ENV}={value:?}: {error}",
                machine_env_path.display()
            )
        });
    }
    default_machine_join_template_file()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InstalledUpdateUnit {
    Nats,
    Machine(String),
    Ployzd(String),
}

impl InstalledUpdateUnit {
    fn unit_name(&self) -> &str {
        match self {
            Self::Nats => "nats-server.service",
            Self::Machine(unit) => unit,
            Self::Ployzd(unit) => unit,
        }
    }
}

fn installed_update_units(
    supervisor_dir: &Path,
    supervisor: SupervisorBackend,
) -> Result<Vec<InstalledUpdateUnit>, std::io::Error> {
    let mut units = Vec::new();
    let nats = supervisor.service_name(&SupervisorUnitTarget::NatsServer);
    if supervisor_dir.join(&nats).is_file() {
        units.push(InstalledUpdateUnit::Nats);
    }
    for entry in std::fs::read_dir(supervisor_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if supervisor.is_machine_ployzd_service_file(file_name) {
            units.push(InstalledUpdateUnit::Machine(file_name.to_owned()));
        } else if supervisor.is_managed_ployzd_service_file(file_name) {
            units.push(InstalledUpdateUnit::Ployzd(file_name.to_owned()));
        }
    }
    units.sort_by(|left, right| left.unit_name().cmp(right.unit_name()));
    units.dedup();
    Ok(units)
}

fn restart_installed_update_units(
    units: &[InstalledUpdateUnit],
    supervisor: SupervisorBackend,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let services = units
        .iter()
        .map(|unit| match unit {
            InstalledUpdateUnit::Nats => supervisor.service_name(&SupervisorUnitTarget::NatsServer),
            InstalledUpdateUnit::Machine(service) => service.clone(),
            InstalledUpdateUnit::Ployzd(service) => service.clone(),
        })
        .collect::<Vec<_>>();
    execute_supervisor_commands(runner, supervisor.restart_installed_commands(&services))
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("substrate update failure message is non-empty")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{
        InstalledUpdateUnit, LoadedSubstrateUpdateManifest, installed_update_units,
        resolve_machine_join_template_file, should_promote_join_template,
    };
    use crate::cli::HostRunnerSubstrateUpdateSource;
    use crate::execution::SupervisorBackend;
    use crate::lifecycle::installed_substrate::InstalledSubstrateManifestSource;
    use crate::release_manifest::ReleaseManifest;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn dev_manifest() -> String {
        format!(
            "PLOYZ_RELEASE_PLATFORM=linux-amd64\n\
             PLOYZ_VERSION=dev-aabbccdd\n\
             PLOYZD_URL=/var/lib/ployz/dev-releases/dev-aabbccdd/ployzd\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=/var/lib/ployz/dev-releases/dev-aabbccdd/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=/var/lib/ployz/dev-releases/dev-aabbccdd/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n\
             PLOYZ_RAILPACK_VERSION=v0.31.0\n\
             PLOYZ_RAILPACK_URL=https://example.test/railpack\n\
             PLOYZ_RAILPACK_SHA256={SHA}\n"
        )
    }

    #[test]
    fn manifest_file_update_persists_source_without_promoting_join_template() {
        let contents = dev_manifest();
        let loaded = LoadedSubstrateUpdateManifest {
            manifest: ReleaseManifest::parse(&contents).expect("manifest parses"),
            persisted_contents: Some(contents.clone()),
        };
        let source = HostRunnerSubstrateUpdateSource::ManifestFile(PathBuf::from(
            "/var/lib/ployz/dev-releases/dev-aabbccdd/release.env",
        ));

        assert!(!should_promote_join_template(&source));
        assert_eq!(
            loaded
                .installed_release()
                .expect("source persists")
                .manifest_source,
            InstalledSubstrateManifestSource::PersistedManifest { contents }
        );
    }

    #[test]
    fn join_template_path_prefers_inherited_configuration() {
        let root = tempfile::tempdir().expect("tempdir");
        let machine_env = root.path().join("ployzd-machine.env");
        fs::write(
            &machine_env,
            "PLOYZ_MACHINE_JOIN_TEMPLATE_FILE=/persisted/template.json\n",
        )
        .expect("machine env writes");

        let path = resolve_machine_join_template_file(
            Some("/inherited/template.json".to_owned()),
            &machine_env,
        )
        .expect("inherited path resolves");

        assert_eq!(path.as_str(), "/inherited/template.json");
    }

    #[test]
    fn join_template_path_uses_exact_persisted_assignment() {
        let root = tempfile::tempdir().expect("tempdir");
        let machine_env = root.path().join("ployzd-machine.env");
        fs::write(
            &machine_env,
            "PLOYZ_MACHINE_ID=machine_1\nPLOYZ_MACHINE_JOIN_TEMPLATE_FILE=/persisted/template.json\n",
        )
        .expect("machine env writes");

        let path = resolve_machine_join_template_file(None, &machine_env)
            .expect("persisted path resolves");

        assert_eq!(path.as_str(), "/persisted/template.json");
    }

    #[test]
    fn join_template_path_defaults_when_file_or_exact_key_is_missing() {
        let root = tempfile::tempdir().expect("tempdir");
        let missing = root.path().join("missing.env");
        assert_eq!(
            resolve_machine_join_template_file(None, &missing)
                .expect("missing file defaults")
                .as_str(),
            "/etc/ployz/machine-join-template.json"
        );

        let machine_env = root.path().join("ployzd-machine.env");
        fs::write(
            &machine_env,
            "export PLOYZ_MACHINE_JOIN_TEMPLATE_FILE=/compatibility/path.json\n",
        )
        .expect("machine env writes");
        assert_eq!(
            resolve_machine_join_template_file(None, &machine_env)
                .expect("noncanonical key is absent")
                .as_str(),
            "/etc/ployz/machine-join-template.json"
        );
    }

    #[test]
    fn join_template_path_rejects_invalid_inherited_or_persisted_values() {
        let root = tempfile::tempdir().expect("tempdir");
        let machine_env = root.path().join("ployzd-machine.env");
        assert!(
            resolve_machine_join_template_file(Some("relative.json".to_owned()), &machine_env)
                .expect_err("relative inherited path fails")
                .contains("PLOYZ_MACHINE_JOIN_TEMPLATE_FILE")
        );

        fs::write(
            &machine_env,
            "PLOYZ_MACHINE_JOIN_TEMPLATE_FILE=relative.json\n",
        )
        .expect("machine env writes");
        assert!(
            resolve_machine_join_template_file(None, &machine_env)
                .expect_err("relative persisted path fails")
                .contains("ployzd-machine.env")
        );
    }

    #[test]
    fn join_template_path_rejects_unreadable_existing_environment() {
        let root = tempfile::tempdir().expect("tempdir");
        let machine_env = root.path().join("ployzd-machine.env");
        fs::create_dir(&machine_env).expect("directory stands in for unreadable file");

        let error = resolve_machine_join_template_file(None, &machine_env)
            .expect_err("existing unreadable environment fails");

        assert!(error.contains("failed to read existing machine environment"));
    }

    #[test]
    fn installed_update_units_discovers_nats_and_ployzd_units() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join("nats-server.service"), "").expect("write nats unit");
        fs::write(root.path().join("ployzd-gateway.service"), "").expect("write gateway unit");
        fs::write(root.path().join("ployzd-machine-machine_1.service"), "")
            .expect("write machine unit");
        fs::write(root.path().join("docker.service"), "").expect("write unrelated unit");

        assert_eq!(
            installed_update_units(root.path(), SupervisorBackend::Systemd).expect("units load"),
            vec![
                InstalledUpdateUnit::Nats,
                InstalledUpdateUnit::Ployzd("ployzd-gateway.service".to_owned()),
                InstalledUpdateUnit::Machine("ployzd-machine-machine_1.service".to_owned()),
            ]
        );
    }

    #[test]
    fn installed_update_units_discovers_openrc_services() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join("nats-server"), "").expect("write nats service");
        fs::write(root.path().join("ployzd-dns"), "").expect("write dns service");
        fs::write(root.path().join("ployzd-dns.conf"), "").expect("write unrelated config");

        assert_eq!(
            installed_update_units(root.path(), SupervisorBackend::OpenRc).expect("services load"),
            vec![
                InstalledUpdateUnit::Nats,
                InstalledUpdateUnit::Ployzd("ployzd-dns".to_owned()),
            ]
        );
    }

    #[test]
    fn installed_update_units_classify_machine_roles() {
        let systemd = tempfile::tempdir().expect("tempdir");
        fs::write(systemd.path().join("ployzd-machine-machine_1.service"), "")
            .expect("write machine unit");
        fs::write(systemd.path().join("ployzd-gateway.service"), "").expect("write gateway unit");
        assert_eq!(
            installed_update_units(systemd.path(), SupervisorBackend::Systemd).expect("units load"),
            vec![
                InstalledUpdateUnit::Ployzd("ployzd-gateway.service".to_owned()),
                InstalledUpdateUnit::Machine("ployzd-machine-machine_1.service".to_owned()),
            ]
        );

        let openrc = tempfile::tempdir().expect("tempdir");
        fs::write(openrc.path().join("ployzd-machine-machine_1"), "")
            .expect("write machine service");
        assert_eq!(
            installed_update_units(openrc.path(), SupervisorBackend::OpenRc)
                .expect("services load"),
            vec![InstalledUpdateUnit::Machine(
                "ployzd-machine-machine_1".to_owned()
            )]
        );
    }
}
