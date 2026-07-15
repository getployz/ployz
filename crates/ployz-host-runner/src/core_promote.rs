use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::artifacts::{ArtifactKind, ArtifactTarget, DataplaneArtifactTargets, artifact_target};
use crate::assigned_substrate::{
    AssignedSubstrateState, SubstrateAssignment, load_assigned_substrate_state,
};
use crate::cli::HostRunnerCorePromote;
use crate::command::SystemHostRunnerCommandRunner;
use crate::core_demote::repoint_non_core_roles;
use crate::join::{
    JOIN_CORE_SEEDS_FILE, JOIN_MATERIAL_DIR, JOIN_MATERIAL_FILE, JOIN_RECOVERY_KEY_FILE,
    JOIN_TRUSTED_CA_FILE, parse_dataplane_endpoint_supernet_from_join_material,
    parse_machine_id_from_join_material,
};
use crate::local::{HostRunnerLocalConfig, HostRunnerLocalEffects};
use crate::plan::{CorePromoteTarget, HostRunnerTextRecorder, core_promote_plan};
use crate::plan::{HostRunnerPlanTerminal, execute_host_runner_plan};
use crate::release_manifest::release_manifest_url;
use ployz_core::install::{MachineJoinClusterName, MachineJoinRuntimeNatsUrl};

use crate::env_config::{
    default_machine_join_template_file, load_versioned_release_manifest, local_release_manifest_url,
};
use crate::runtime::{
    CORE_PROMOTE_RESULT_BEGIN, CORE_PROMOTE_RESULT_END, HOST_RUNNER_STATE_DIR, failure_summary,
};

pub(crate) fn run_core_promote_command(promote: HostRunnerCorePromote) -> ExitCode {
    if promote.check {
        return match check_core_promote_preflight() {
            Ok(()) => {
                println!("core-promote preflight ok");
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("ployz host core-promote check failed: {message}");
                ExitCode::FAILURE
            }
        };
    }

    // The installed release supplies the core nats-server + ployzd binaries;
    // --version is an explicit operator override.
    let manifest_url = match &promote.version {
        Some(version) => release_manifest_url(version),
        None => local_release_manifest_url(Path::new("/etc/ployz/release.env")),
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
    // A promoted core runs its own nats-server; a manifest without one (a dev
    // substrate push) cannot promote.
    let Some(nats_server_spec) = artifacts.nats_server else {
        eprintln!("release manifest carries no nats-server artifact; core-promote requires one");
        return ExitCode::FAILURE;
    };
    let nats_server_artifact = match artifact_target(
        ArtifactKind::NatsServer,
        &ployz_core::install::InstallArtifactSpec {
            version: nats_server_spec.version,
            source: nats_server_spec.source,
            sha256: nats_server_spec.sha256,
            install_path: nats_server_spec.binary,
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
    let ebpf_bytecode_artifact =
        match artifact_target(ArtifactKind::EbpfBytecode, &artifacts.ebpf_bytecode) {
            Ok(target) => target,
            Err(error) => {
                eprintln!("release manifest eBPF bytecode artifact is invalid: {error}");
                return ExitCode::FAILURE;
            }
        };
    let ebpf_ctl_artifact = match artifact_target(ArtifactKind::EbpfCtl, &artifacts.ebpf_ctl) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("release manifest eBPF controller artifact is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };

    let (target, access) = match resolve_core_promote_target(
        nats_server_artifact,
        ployzd_artifact,
        DataplaneArtifactTargets::new(ebpf_bytecode_artifact, ebpf_ctl_artifact),
    ) {
        Ok(resolved) => resolved,
        Err(message) => {
            eprintln!("ployz host core-promote: {message}");
            return ExitCode::FAILURE;
        }
    };

    let stdout = std::io::stdout();
    let mut recorder = HostRunnerTextRecorder::new(stdout.lock());
    let plan = core_promote_plan(target);
    let mut effects = HostRunnerLocalEffects::new(
        HostRunnerLocalConfig {
            supervisor_dirs: crate::supervisor::SupervisorDirectories::host_defaults(),
            state_dir: HOST_RUNNER_STATE_DIR.into(),
            docker_daemon_config: "/etc/docker/daemon.json".into(),
            docker_repository_dir: "/etc/yum.repos.d".into(),
        },
        SystemHostRunnerCommandRunner::default(),
    );
    match execute_host_runner_plan(&plan, &mut effects, &mut recorder).terminal {
        HostRunnerPlanTerminal::Completed => {
            let successor =
                match ployz_nats::connect::NatsClientUrl::try_new("tls://127.0.0.1:4222") {
                    Ok(url) => url,
                    Err(error) => {
                        eprintln!(
                            "ployz host core-promote failed: promoted NATS URL is invalid: {error}"
                        );
                        return ExitCode::FAILURE;
                    }
                };
            if let Err(error) = repoint_non_core_roles(
                Path::new("/etc/ployz"),
                &successor,
                &mut SystemHostRunnerCommandRunner::default(),
            ) {
                eprintln!("ployz host core-promote failed: {error}");
                return ExitCode::FAILURE;
            }
            drop(recorder);
            print_core_promote_result(&access);
            ExitCode::SUCCESS
        }
        HostRunnerPlanTerminal::Failed(failure) => {
            eprintln!(
                "ployz host core-promote failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
    }
}

fn check_core_promote_preflight() -> Result<(), String> {
    promoted_assigned_substrate(Path::new(HOST_RUNNER_STATE_DIR))?;
    let join_dir = PathBuf::from(HOST_RUNNER_STATE_DIR).join(JOIN_MATERIAL_DIR);
    require_promote_file(&join_dir.join(JOIN_CORE_SEEDS_FILE), JOIN_CORE_SEEDS_FILE)?;
    require_promote_file(
        &join_dir.join(JOIN_RECOVERY_KEY_FILE),
        JOIN_RECOVERY_KEY_FILE,
    )?;
    let mirror_path = join_dir.join(ployz_core::install::INTENT_MIRROR_FILE_NAME);
    require_promote_file(&mirror_path, ployz_core::install::INTENT_MIRROR_FILE_NAME)?;
    let _: ployz_core::state::IntentSnapshot =
        serde_json::from_str(&read_promote_file(&mirror_path)?).map_err(|error| {
            format!(
                "cannot parse intent mirror {}: {error}",
                mirror_path.display()
            )
        })?;
    Ok(())
}

fn require_promote_file(path: &Path, name: &str) -> Result<(), String> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        format!(
            "missing required promotion material {name} at {}: {error}",
            path.display()
        )
    })?;
    if metadata.is_file() {
        Ok(())
    } else {
        Err(format!(
            "required promotion material {name} at {} is not a file",
            path.display()
        ))
    }
}

/// Read the promotion inputs from this machine's own state: the CA + wrapped
/// recovery key from join material (decrypted with `PLOYZ_RECOVERY_SECRET`), and
/// this machine's control endpoints from the local mirror.
fn resolve_core_promote_target(
    nats_server_artifact: ArtifactTarget,
    ployzd_artifact: ArtifactTarget,
    dataplane_artifacts: DataplaneArtifactTargets,
) -> Result<(CorePromoteTarget, PromotedCoreAccess), String> {
    let assigned_substrate = promoted_assigned_substrate(Path::new(HOST_RUNNER_STATE_DIR))?;
    let join_dir = PathBuf::from(HOST_RUNNER_STATE_DIR).join(JOIN_MATERIAL_DIR);
    let join_material = read_promote_file(&join_dir.join(JOIN_MATERIAL_FILE))?;
    let machine_id = parse_machine_id_from_join_material(&join_material)
        .ok_or("join material carries no machine id — is this a joined machine?")?;
    let cluster_name = parse_cluster_name_from_join_material(&join_material)
        .and_then(|value| MachineJoinClusterName::try_new(value).ok())
        .unwrap_or_else(|| MachineJoinClusterName::try_new("ployz").expect("valid cluster name"));
    let dataplane_endpoint_supernet =
        parse_dataplane_endpoint_supernet_from_join_material(&join_material)
            .unwrap_or_else(ployz_core::dataplane::MachineEndpointSupernet::default_v1);
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
        crate::recovery_secret::unwrap(&secret, &wrapped).map_err(|error| {
            format!("cannot decrypt the CA key (wrong recovery secret?): {error}")
        })?,
    )
    .map_err(|_| "decrypted CA key is not valid UTF-8".to_owned())?;
    let core_seeds = crate::nats_identity::unwrap_core_seeds(&secret, &wrapped_seeds)
        .map_err(|error| format!("{error}"))?;

    // The machine persists its mirror beside its seed file; on a joined machine
    // that seed lives in the join-material directory (nats.creds), so the mirror is
    // there too — not the first-machine `/var/lib/ployz/nats` layout.
    let mirror_path = join_dir.join(ployz_core::install::INTENT_MIRROR_FILE_NAME);
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
    let machine_join_runtime_nats_url = MachineJoinRuntimeNatsUrl::try_new(format!(
        "tls://{}",
        SocketAddr::new(machine_public_ip, 4222)
    ))
    .map_err(|error| format!("promoted runtime NATS URL is invalid: {error}"))?;

    let hostname = gethostname::gethostname().into_string().ok();
    let sans = crate::nats_identity::ServerCertificateSans::try_new_many(
        control_endpoints.clone(),
        hostname,
    )
    .map_err(|error| format!("{error}"))?;
    // Reuse the old core's principals from the pre-positioned seeds; the grant set is
    // seeded into the store from the mirror by control at startup (ADR 0031).
    let nats_identity =
        crate::nats_identity::resurrect_core_identity(ca, ca_key_pem, core_seeds, &sans)
            .map_err(|error| format!("{error}"))?;

    // Promotion reuses the old core's principals, so the operator's existing ployz
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
        dataplane_artifacts,
        nats_identity,
        ployz_core::install::WrappedCaKey::new(wrapped),
        wrapped_seeds,
        dataplane_endpoint_supernet,
        assigned_substrate,
        Some(machine_public_ip),
        mirror_path,
        default_machine_join_template_file()?,
        cluster_name,
        machine_join_runtime_nats_url,
    );
    Ok((target, access))
}

fn promoted_assigned_substrate(state_dir: &Path) -> Result<AssignedSubstrateState, String> {
    let mut assigned = load_assigned_substrate_state(state_dir).map_err(|error| {
        format!(
            "cannot load assigned substrate state for core promotion: {error}; restore {} before retrying",
            state_dir.join(crate::assigned_substrate::ASSIGNED_SUBSTRATE_FILE).display()
        )
    })?;
    assigned.assignments.insert(SubstrateAssignment::NatsServer);
    Ok(assigned)
}

fn parse_cluster_name_from_join_material(contents: &str) -> Option<String> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("cluster_name="))
        .map(str::to_owned)
}

/// Where the operator points their existing `ployz` context after promotion: the
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ployz_core::install::HostPortAssurance;

    use crate::assigned_substrate::{
        AssignedSubstrateState, SubstrateAssignment, write_assigned_substrate_state,
    };

    use super::promoted_assigned_substrate;

    #[test]
    fn promotion_adds_nats_without_losing_assignments_or_assurance() {
        let state_dir = tempfile::tempdir().expect("state dir");
        write_assigned_substrate_state(
            state_dir.path(),
            &AssignedSubstrateState {
                assignments: BTreeSet::from([
                    SubstrateAssignment::Dataplane,
                    SubstrateAssignment::Gateway,
                ]),
                host_ports: HostPortAssurance::External,
            },
        )
        .expect("assigned state writes");

        let promoted = promoted_assigned_substrate(state_dir.path()).expect("state loads");

        assert_eq!(promoted.host_ports, HostPortAssurance::External);
        assert_eq!(
            promoted.assignments,
            BTreeSet::from([
                SubstrateAssignment::NatsServer,
                SubstrateAssignment::Dataplane,
                SubstrateAssignment::Gateway,
            ])
        );
    }

    #[test]
    fn promotion_refuses_missing_assigned_substrate_state() {
        let state_dir = tempfile::tempdir().expect("state dir");

        let error = promoted_assigned_substrate(state_dir.path()).expect_err("state is required");

        assert!(error.contains("cannot load assigned substrate state for core promotion"));
        assert!(error.contains("assigned-substrate.json"));
    }

    #[test]
    fn promotion_refuses_corrupt_assigned_substrate_state() {
        let state_dir = tempfile::tempdir().expect("state dir");
        std::fs::write(
            state_dir.path().join("assigned-substrate.json"),
            b"not json",
        )
        .expect("corrupt state writes");

        let error = promoted_assigned_substrate(state_dir.path()).expect_err("state is required");

        assert!(error.contains("failed to parse assigned substrate state"));
        assert!(error.contains("restore"));
    }
}
