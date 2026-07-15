//! Founder Bootstrap forms a new control plane and activates its first machine.

use std::process::ExitCode;

use crate::env_config::local_core_target_from_env;
use crate::execution::{
    HostRunnerLocalConfig, HostRunnerLocalEffects, SupervisorDirectories,
    SystemHostRunnerCommandRunner,
};
use crate::plan::{FirstMachineInstallTarget, HostRunnerTextRecorder, first_machine_install_plan};
use crate::plan::{HostRunnerPlanTerminal, execute_host_runner_plan};
use ployz_core::ids::MachineId;
use ployz_core::install::{MachineJoinRuntimeNatsUrl, NatsMachineMaterialPaths};
use ployz_sdk_types::{CloudFounderBootstrapResult, MachineJoinTrustedNats, NatsCaCertificatePem};

use crate::runtime::{
    FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN, FIRST_MACHINE_BOOTSTRAP_RESULT_END,
    HOST_RUNNER_STATE_DIR, failure_summary,
};

pub(crate) fn run_local_founder_bootstrap() -> ExitCode {
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
    let mut recorder = HostRunnerTextRecorder::new(stdout.lock());
    let execution = run_first_machine_install(target, &mut recorder);
    match execution.terminal {
        HostRunnerPlanTerminal::Completed => {
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
        HostRunnerPlanTerminal::Failed(failure) => {
            eprintln!(
                "ployz host bootstrap core failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn run_first_machine_install_command(target: FirstMachineInstallTarget) -> ExitCode {
    let machine_id = target.machine_id.clone();
    let nats_material = target.nats_material.clone();
    let runtime_nats_url = target.machine_join_runtime_nats_url().clone();
    let stdout = std::io::stdout();
    let mut recorder = HostRunnerTextRecorder::new(stdout.lock());
    let execution = run_first_machine_install(target, &mut recorder);
    match execution.terminal {
        HostRunnerPlanTerminal::Completed => {
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
        HostRunnerPlanTerminal::Failed(failure) => {
            eprintln!("ployz host install failed: {}", failure_summary(&failure));
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn print_first_machine_bootstrap_result(
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

pub(crate) fn read_cloud_founder_bootstrap_result(
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

pub(crate) fn run_first_machine_install(
    target: FirstMachineInstallTarget,
    recorder: &mut impl crate::plan::HostRunnerStepRecorder,
) -> crate::plan::HostRunnerPlanExecution {
    let plan = first_machine_install_plan(target);
    let mut effects = HostRunnerLocalEffects::new(
        HostRunnerLocalConfig {
            supervisor_dirs: SupervisorDirectories::host_defaults(),
            state_dir: HOST_RUNNER_STATE_DIR.into(),
            docker_daemon_config: "/etc/docker/daemon.json".into(),
            docker_repository_dir: "/etc/yum.repos.d".into(),
        },
        SystemHostRunnerCommandRunner::default(),
    );
    execute_host_runner_plan(&plan, &mut effects, recorder)
}

#[cfg(test)]
mod tests {
    use super::read_cloud_founder_bootstrap_result;
    use ployz_core::ids::MachineId;
    use ployz_core::install::{MachineJoinRuntimeNatsUrl, NatsMachineMaterialPaths};

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
}
