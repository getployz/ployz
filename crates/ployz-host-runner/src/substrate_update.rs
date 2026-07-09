use std::path::Path;
use std::process::ExitCode;

use crate::artifacts::{ArtifactKind, artifact_target};
use crate::cli::{HostRunnerSubstrateUpdate, HostRunnerSubstrateUpdateSource};
use crate::command::{HostRunnerCommandRunner, SystemHostRunnerCommandRunner};
use crate::executor::{HostRunnerPlanTerminal, execute_host_runner_plan};
use crate::fsx::{FileMode, write_durable_file};
use crate::local::{HostRunnerLocalConfig, HostRunnerLocalEffects};
use crate::release_manifest::{ReleaseManifest, release_manifest_url};
use crate::report::HostRunnerTextRecorder;
use crate::steps::{HostPrerequisite, HostRunnerStep, HostRunnerStepPlan};
use ployz_core::ids::OperationId;
use ployz_core::install::InstallArtifactVersion;
use ployz_core::ops::FailureMessage;
use serde::Serialize;

use crate::env_config::load_versioned_release_manifest;
use crate::runtime::{HOST_RUNNER_STATE_DIR, SUBSTRATE_VERSION_FILE, failure_summary};

pub(crate) fn run_substrate_update_command(update: HostRunnerSubstrateUpdate) -> ExitCode {
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
    let update_label = update.source.label();
    let manifest = match load_substrate_update_manifest(&update.source) {
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
        HostRunnerStep::VerifyHost(HostPrerequisite::LinuxRootSystemd),
        HostRunnerStep::PrepareDataplaneHost,
        HostRunnerStep::InstallArtifact(ployzd),
        HostRunnerStep::InstallArtifact(ebpf_bytecode),
        HostRunnerStep::InstallArtifact(ebpf_ctl),
    ];
    if let Some(nats_server) = nats_server
        && units
            .iter()
            .any(|unit| matches!(unit, InstalledUpdateUnit::Nats))
    {
        steps.push(HostRunnerStep::InstallArtifact(nats_server));
    }
    let plan = HostRunnerStepPlan::from_steps(steps);
    let stdout = std::io::stdout();
    let mut recorder = HostRunnerTextRecorder::new(stdout.lock());
    let mut effects = HostRunnerLocalEffects::new(
        HostRunnerLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: HOST_RUNNER_STATE_DIR.into(),
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
    let mut runner = SystemHostRunnerCommandRunner::default();
    if let Err(message) = restart_installed_update_units(&units, &mut runner) {
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
) -> Result<ReleaseManifest, String> {
    match source {
        HostRunnerSubstrateUpdateSource::Version(version) => {
            load_versioned_release_manifest(&release_manifest_url(version))
        }
        HostRunnerSubstrateUpdateSource::ManifestFile(path) => {
            let contents = std::fs::read_to_string(path).map_err(|error| {
                format!(
                    "failed to read release manifest {}: {error}",
                    path.display()
                )
            })?;
            ReleaseManifest::parse(&contents)
        }
    }
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
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    runner.systemctl(&["daemon-reload"])?;
    for unit in units {
        runner.systemctl(&["restart", unit.unit_name()])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{InstalledUpdateUnit, installed_update_units};

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
}
