use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::execution::HostRunnerCommandOutput;
use crate::execution::supervisor::execute_supervisor_commands;
use crate::execution::{
    HostRunnerCommandRunner, SupervisorBackend, SupervisorChange, SupervisorUnitTarget,
    detect_host_platform,
};
use ployz_core::ids::MachineId;
use ployz_core::ops::FailureMessage;
use ployz_core::roles::DaemonProcessRole;
use ployz_nats::connect::NatsClientUrl;

const DEFAULT_ENV_DIR: &str = "/etc/ployz";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreDemoteTarget {
    pub successor_nats_url: NatsClientUrl,
    env_dir: PathBuf,
}

impl CoreDemoteTarget {
    #[must_use]
    pub fn new(successor_nats_url: NatsClientUrl) -> Self {
        Self {
            successor_nats_url,
            env_dir: PathBuf::from(DEFAULT_ENV_DIR),
        }
    }

    #[must_use]
    pub fn with_env_dir(mut self, env_dir: PathBuf) -> Self {
        self.env_dir = env_dir;
        self
    }
}

pub fn demote_local_core(
    target: &CoreDemoteTarget,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let supervisor = detect_supervisor(runner)?;
    repoint_non_core_roles_with(
        &target.env_dir,
        &target.successor_nats_url,
        supervisor,
        runner,
    )?;

    let control = SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Control);
    let nats = SupervisorUnitTarget::NatsServer;
    execute_supervisor_commands(
        runner,
        supervisor
            .commands_for_targets(SupervisorChange::Disable, &[control.clone(), nats.clone()]),
    )?;
    apply_supervisor_change(supervisor, SupervisorChange::Stop, &nats, runner)?;
    if let Err(stop_error) =
        apply_supervisor_change(supervisor, SupervisorChange::Stop, &control, runner)
    {
        let kill_result =
            apply_supervisor_change(supervisor, SupervisorChange::Kill, &control, runner);
        let retry_stop_result =
            apply_supervisor_change(supervisor, SupervisorChange::Stop, &control, runner);
        if kill_result.is_err() && retry_stop_result.is_err() {
            return Err(stop_error);
        }
    }
    ensure_unit_inactive(supervisor, &control, runner)?;
    Ok(())
}

fn ensure_unit_inactive(
    supervisor: SupervisorBackend,
    target: &SupervisorUnitTarget,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    if apply_supervisor_change(supervisor, SupervisorChange::IsActive, target, runner).is_ok() {
        let service = supervisor.service_name(target);
        return Err(failure_message(format!(
            "{service} is still active after demotion"
        )));
    }
    Ok(())
}

/// Repoint this machine's non-core role env files at the successor core and
/// restart the roles. The machine-local intent mirror is deliberately left in
/// place: `core-promote` runs this right after starting the promoted
/// `ployzd-control`, which still has to read that mirror to seed its store
/// (`PLOYZ_SEED_FROM_MIRROR`), and the mirror is epoch-gated so the restarted
/// roles repair it from the successor's higher-epoch drumbeat — the daemon
/// owns the mirror's lifecycle, Host Runner never deletes it.
pub fn repoint_non_core_roles(
    env_dir: &Path,
    successor: &NatsClientUrl,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let supervisor = detect_supervisor(runner)?;
    repoint_non_core_roles_with(env_dir, successor, supervisor, runner)
}

fn repoint_non_core_roles_with(
    env_dir: &Path,
    successor: &NatsClientUrl,
    supervisor: SupervisorBackend,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    repoint_machine_role(env_dir, successor, supervisor, runner)?;
    repoint_fixed_role(
        env_dir,
        successor,
        DaemonProcessRole::Gateway,
        "ployzd-gateway.env",
        supervisor,
        runner,
    )?;
    repoint_fixed_role(
        env_dir,
        successor,
        DaemonProcessRole::Dns,
        "ployzd-dns.env",
        supervisor,
        runner,
    )
}

fn repoint_machine_role(
    env_dir: &Path,
    successor: &NatsClientUrl,
    supervisor: SupervisorBackend,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let path = env_dir.join("ployzd-machine.env");
    let Some(contents) = read_optional_env(&path)? else {
        return Ok(());
    };
    let machine_id = env_value(&contents, "PLOYZ_MACHINE_ID")
        .ok_or_else(|| failure_message(format!("{} missing PLOYZ_MACHINE_ID", path.display())))?;
    let role = DaemonProcessRole::Machine(MachineId::try_new(machine_id).map_err(|error| {
        failure_message(format!(
            "{} has invalid PLOYZ_MACHINE_ID: {error}",
            path.display()
        ))
    })?);
    write_repointed_env(&path, &contents, successor)?;
    restart_repointed_role(supervisor, &SupervisorUnitTarget::PloyzdRole(role), runner)
}

fn repoint_fixed_role(
    env_dir: &Path,
    successor: &NatsClientUrl,
    role: DaemonProcessRole,
    file_name: &str,
    supervisor: SupervisorBackend,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let path = env_dir.join(file_name);
    let Some(contents) = read_optional_env(&path)? else {
        return Ok(());
    };
    write_repointed_env(&path, &contents, successor)?;
    restart_repointed_role(supervisor, &SupervisorUnitTarget::PloyzdRole(role), runner)
}

fn restart_repointed_role(
    supervisor: SupervisorBackend,
    target: &SupervisorUnitTarget,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    if apply_supervisor_change(supervisor, SupervisorChange::Restart, target, runner).is_ok() {
        return Ok(());
    }
    let _ = apply_supervisor_change(supervisor, SupervisorChange::Kill, target, runner);
    apply_supervisor_change(supervisor, SupervisorChange::Restart, target, runner)
}

fn detect_supervisor(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<SupervisorBackend, FailureMessage> {
    let os_release = runner.read_os_release()?;
    let profile =
        detect_host_platform(&os_release).map_err(|error| failure_message(error.to_string()))?;
    Ok(profile.supervisor().into())
}

fn apply_supervisor_change(
    supervisor: SupervisorBackend,
    change: SupervisorChange,
    target: &SupervisorUnitTarget,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    execute_supervisor_commands(runner, supervisor.commands(change, target))
}

fn read_optional_env(path: &Path) -> Result<Option<String>, FailureMessage> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(failure_message(format!(
            "failed to read {}: {error}",
            path.display()
        ))),
    }
}

fn write_repointed_env(
    path: &Path,
    contents: &str,
    successor: &NatsClientUrl,
) -> Result<(), FailureMessage> {
    let updated = replace_env_value(contents, "PLOYZ_NATS_URL", successor.as_str())
        .ok_or_else(|| failure_message(format!("{} missing PLOYZ_NATS_URL", path.display())))?;
    fs::write(path, updated)
        .map_err(|error| failure_message(format!("failed to write {}: {error}", path.display())))
}

fn replace_env_value(contents: &str, key: &str, value: &str) -> Option<String> {
    let mut found = false;
    let mut output = String::new();
    for line in contents.lines() {
        if line.starts_with(key) && line.as_bytes().get(key.len()) == Some(&b'=') {
            found = true;
            output.push_str(key);
            output.push('=');
            output.push_str(value);
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    found.then_some(output)
}

fn env_value<'a>(contents: &'a str, key: &str) -> Option<&'a str> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message.into()).expect("generated failure message is non-empty")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingRunner {
        systemctl_calls: Vec<Vec<String>>,
        fail_systemctl_once: Vec<Vec<String>>,
        active_systemctl: Vec<Vec<String>>,
        os_release: Option<String>,
        openrc_calls: Vec<String>,
    }

    impl HostRunnerCommandRunner for RecordingRunner {
        fn command(
            &mut self,
            program: &str,
            args: &[&str],
        ) -> Result<HostRunnerCommandOutput, FailureMessage> {
            if program != "systemctl" {
                self.openrc_calls
                    .push(format!("{program} {}", args.join(" ")));
                let success = !(program == "rc-service" && args.get(1) == Some(&"status"));
                return Ok(HostRunnerCommandOutput {
                    success,
                    exit_code: Some(if success { 0 } else { 3 }),
                    stdout: String::new(),
                    failure: if success {
                        String::new()
                    } else {
                        "simulated inactive service".to_owned()
                    },
                });
            }
            let call: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            self.systemctl_calls.push(call.clone());
            let failed = self
                .fail_systemctl_once
                .iter()
                .position(|failure| failure == &call)
                .map(|index| self.fail_systemctl_once.remove(index))
                .is_some();
            let inactive = args.first() == Some(&"is-active")
                && args.get(1) == Some(&"--quiet")
                && !self.active_systemctl.contains(&call);
            let success = !failed && !inactive;
            Ok(HostRunnerCommandOutput {
                success,
                exit_code: Some(if success { 0 } else { 1 }),
                stdout: String::new(),
                failure: if success {
                    String::new()
                } else if inactive {
                    "simulated inactive unit".to_owned()
                } else {
                    "simulated systemctl failure".to_owned()
                },
            })
        }

        fn read_os_release(&mut self) -> Result<String, FailureMessage> {
            Ok(self
                .os_release
                .clone()
                .unwrap_or_else(|| "ID=ubuntu\nVERSION_ID=24.04\n".to_owned()))
        }

        fn is_linux(&mut self) -> bool {
            true
        }

        fn current_uid(&mut self) -> Result<u32, FailureMessage> {
            Ok(0)
        }

        fn download(&mut self, _url: &str, _destination: &Path) -> Result<(), FailureMessage> {
            Ok(())
        }

        fn docker_info(&mut self) -> Result<(), FailureMessage> {
            Ok(())
        }

        fn docker_is_installed(&mut self) -> bool {
            true
        }

        fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
            Ok(true)
        }

        fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
            Ok(true)
        }
    }

    #[test]
    fn demote_local_core_repoints_local_roles_then_disables_core_units() {
        let env = tempfile::tempdir().expect("env dir");
        fs::write(
            env.path().join("ployzd-machine.env"),
            "PLOYZ_NATS_URL=tls://old:4222\nPLOYZ_MACHINE_ID=machine_1\n",
        )
        .expect("write machine env");
        fs::write(
            env.path().join("ployzd-gateway.env"),
            "PLOYZ_NATS_URL=tls://old:4222\nPLOYZ_MACHINE_ID=machine_1\n",
        )
        .expect("write gateway env");
        let mut runner = RecordingRunner::default();
        let target =
            CoreDemoteTarget::new(NatsClientUrl::try_new("tls://new:4222").expect("valid url"))
                .with_env_dir(env.path().to_path_buf());

        demote_local_core(&target, &mut runner).expect("demote succeeds");

        assert_eq!(
            runner.systemctl_calls,
            vec![
                vec![
                    "restart".to_owned(),
                    "ployzd-machine-machine_1.service".to_owned(),
                ],
                vec!["restart".to_owned(), "ployzd-gateway.service".to_owned(),],
                vec![
                    "disable".to_owned(),
                    "ployzd-control.service".to_owned(),
                    "nats-server.service".to_owned(),
                ],
                vec!["stop".to_owned(), "nats-server.service".to_owned(),],
                vec!["stop".to_owned(), "ployzd-control.service".to_owned(),],
                vec![
                    "is-active".to_owned(),
                    "--quiet".to_owned(),
                    "ployzd-control.service".to_owned(),
                ],
            ]
        );
        assert!(
            fs::read_to_string(env.path().join("ployzd-machine.env"))
                .expect("machine env reads")
                .starts_with("PLOYZ_NATS_URL=tls://new:4222\n")
        );
    }

    #[test]
    fn demote_local_core_uses_openrc_on_alpine() {
        let env = tempfile::tempdir().expect("env dir");
        let mut runner = RecordingRunner {
            os_release: Some("ID=alpine\nVERSION_ID=3.22\n".to_owned()),
            ..RecordingRunner::default()
        };
        let target =
            CoreDemoteTarget::new(NatsClientUrl::try_new("tls://new:4222").expect("valid url"))
                .with_env_dir(env.path().to_path_buf());

        demote_local_core(&target, &mut runner).expect("demote succeeds");

        assert_eq!(
            runner.openrc_calls,
            [
                "rc-update del ployzd-control default",
                "rc-update del nats-server default",
                "rc-service nats-server stop",
                "rc-service ployzd-control stop",
                "rc-service ployzd-control status",
            ]
        );
        assert!(runner.systemctl_calls.is_empty());
    }

    #[test]
    fn demote_local_core_kills_stuck_control_after_nats_stops() {
        let env = tempfile::tempdir().expect("env dir");
        fs::write(
            env.path().join("ployzd-machine.env"),
            "PLOYZ_NATS_URL=tls://old:4222\nPLOYZ_MACHINE_ID=machine_1\n",
        )
        .expect("write machine env");
        let mut runner = RecordingRunner {
            fail_systemctl_once: vec![vec!["stop".to_owned(), "ployzd-control.service".to_owned()]],
            ..RecordingRunner::default()
        };
        let target =
            CoreDemoteTarget::new(NatsClientUrl::try_new("tls://new:4222").expect("valid url"))
                .with_env_dir(env.path().to_path_buf());

        demote_local_core(&target, &mut runner).expect("demote succeeds");

        assert_eq!(
            runner.systemctl_calls,
            vec![
                vec![
                    "restart".to_owned(),
                    "ployzd-machine-machine_1.service".to_owned(),
                ],
                vec![
                    "disable".to_owned(),
                    "ployzd-control.service".to_owned(),
                    "nats-server.service".to_owned(),
                ],
                vec!["stop".to_owned(), "nats-server.service".to_owned(),],
                vec!["stop".to_owned(), "ployzd-control.service".to_owned(),],
                vec![
                    "kill".to_owned(),
                    "--signal=SIGKILL".to_owned(),
                    "ployzd-control.service".to_owned(),
                ],
                vec!["stop".to_owned(), "ployzd-control.service".to_owned(),],
                vec![
                    "is-active".to_owned(),
                    "--quiet".to_owned(),
                    "ployzd-control.service".to_owned(),
                ],
            ]
        );
    }

    #[test]
    fn demote_local_core_ignores_failed_kill_when_control_is_inactive() {
        let env = tempfile::tempdir().expect("env dir");
        fs::write(
            env.path().join("ployzd-machine.env"),
            "PLOYZ_NATS_URL=tls://old:4222\nPLOYZ_MACHINE_ID=machine_1\n",
        )
        .expect("write machine env");
        let mut runner = RecordingRunner {
            fail_systemctl_once: vec![
                vec!["stop".to_owned(), "ployzd-control.service".to_owned()],
                vec![
                    "kill".to_owned(),
                    "--signal=SIGKILL".to_owned(),
                    "ployzd-control.service".to_owned(),
                ],
            ],
            ..RecordingRunner::default()
        };
        let target =
            CoreDemoteTarget::new(NatsClientUrl::try_new("tls://new:4222").expect("valid url"))
                .with_env_dir(env.path().to_path_buf());

        demote_local_core(&target, &mut runner).expect("demote succeeds");

        assert_eq!(
            runner.systemctl_calls,
            vec![
                vec![
                    "restart".to_owned(),
                    "ployzd-machine-machine_1.service".to_owned(),
                ],
                vec![
                    "disable".to_owned(),
                    "ployzd-control.service".to_owned(),
                    "nats-server.service".to_owned(),
                ],
                vec!["stop".to_owned(), "nats-server.service".to_owned(),],
                vec!["stop".to_owned(), "ployzd-control.service".to_owned(),],
                vec![
                    "kill".to_owned(),
                    "--signal=SIGKILL".to_owned(),
                    "ployzd-control.service".to_owned(),
                ],
                vec!["stop".to_owned(), "ployzd-control.service".to_owned(),],
                vec![
                    "is-active".to_owned(),
                    "--quiet".to_owned(),
                    "ployzd-control.service".to_owned(),
                ],
            ]
        );
    }

    #[test]
    fn demote_local_core_preserves_stop_error_when_fallbacks_fail() {
        let env = tempfile::tempdir().expect("env dir");
        fs::write(
            env.path().join("ployzd-machine.env"),
            "PLOYZ_NATS_URL=tls://old:4222\nPLOYZ_MACHINE_ID=machine_1\n",
        )
        .expect("write machine env");
        let mut runner = RecordingRunner {
            fail_systemctl_once: vec![
                vec!["stop".to_owned(), "ployzd-control.service".to_owned()],
                vec![
                    "kill".to_owned(),
                    "--signal=SIGKILL".to_owned(),
                    "ployzd-control.service".to_owned(),
                ],
                vec!["stop".to_owned(), "ployzd-control.service".to_owned()],
            ],
            ..RecordingRunner::default()
        };
        let target =
            CoreDemoteTarget::new(NatsClientUrl::try_new("tls://new:4222").expect("valid url"))
                .with_env_dir(env.path().to_path_buf());

        let error = demote_local_core(&target, &mut runner).expect_err("demote fails");

        assert_eq!(error.to_string(), "simulated systemctl failure");
        assert_eq!(
            runner.systemctl_calls,
            vec![
                vec![
                    "restart".to_owned(),
                    "ployzd-machine-machine_1.service".to_owned(),
                ],
                vec![
                    "disable".to_owned(),
                    "ployzd-control.service".to_owned(),
                    "nats-server.service".to_owned(),
                ],
                vec!["stop".to_owned(), "nats-server.service".to_owned(),],
                vec!["stop".to_owned(), "ployzd-control.service".to_owned(),],
                vec![
                    "kill".to_owned(),
                    "--signal=SIGKILL".to_owned(),
                    "ployzd-control.service".to_owned(),
                ],
                vec!["stop".to_owned(), "ployzd-control.service".to_owned(),],
            ]
        );
    }

    #[test]
    fn demote_local_core_fails_when_control_stays_active_after_kill() {
        let env = tempfile::tempdir().expect("env dir");
        fs::write(
            env.path().join("ployzd-machine.env"),
            "PLOYZ_NATS_URL=tls://old:4222\nPLOYZ_MACHINE_ID=machine_1\n",
        )
        .expect("write machine env");
        let mut runner = RecordingRunner {
            fail_systemctl_once: vec![vec!["stop".to_owned(), "ployzd-control.service".to_owned()]],
            active_systemctl: vec![vec![
                "is-active".to_owned(),
                "--quiet".to_owned(),
                "ployzd-control.service".to_owned(),
            ]],
            ..RecordingRunner::default()
        };
        let target =
            CoreDemoteTarget::new(NatsClientUrl::try_new("tls://new:4222").expect("valid url"))
                .with_env_dir(env.path().to_path_buf());

        let error = demote_local_core(&target, &mut runner).expect_err("demote fails");

        assert_eq!(
            error.to_string(),
            "ployzd-control.service is still active after demotion"
        );
    }

    #[test]
    fn repoint_non_core_roles_kills_and_retries_stuck_restart() {
        let env = tempfile::tempdir().expect("env dir");
        fs::write(
            env.path().join("ployzd-gateway.env"),
            "PLOYZ_NATS_URL=tls://old:4222\nPLOYZ_NATS_CA_FILE=/ca.pem\nPLOYZ_NATS_NKEY_SEED_FILE=/machine.seed\n",
        )
        .expect("write gateway env");
        let successor = NatsClientUrl::try_new("tls://127.0.0.1:4222").expect("valid url");
        let mut runner = RecordingRunner {
            fail_systemctl_once: vec![vec![
                "restart".to_owned(),
                "ployzd-gateway.service".to_owned(),
            ]],
            ..RecordingRunner::default()
        };

        repoint_non_core_roles(env.path(), &successor, &mut runner).expect("repoint succeeds");

        assert_eq!(
            runner.systemctl_calls,
            vec![
                vec!["restart".to_owned(), "ployzd-gateway.service".to_owned(),],
                vec![
                    "kill".to_owned(),
                    "--signal=SIGKILL".to_owned(),
                    "ployzd-gateway.service".to_owned(),
                ],
                vec!["restart".to_owned(), "ployzd-gateway.service".to_owned(),],
            ]
        );
    }

    #[test]
    fn repoint_non_core_roles_updates_only_nats_url_and_restarts_roles() {
        let env = tempfile::tempdir().expect("env dir");
        let material_dir = env.path().join("material");
        fs::create_dir(&material_dir).expect("material dir");
        fs::write(material_dir.join("intent-mirror.json"), "{}").expect("write mirror");
        let machine_seed = material_dir.join("machine.seed");
        fs::write(
            env.path().join("ployzd-machine.env"),
            format!(
                "PLOYZ_NATS_URL=tls://old:4222\nPLOYZ_NATS_CA_FILE=/ca.pem\nPLOYZ_NATS_NKEY_SEED_FILE={}\nPLOYZ_MACHINE_ID=machine_1\n",
                machine_seed.display()
            ),
        )
        .expect("write machine env");
        fs::write(
            env.path().join("ployzd-dns.env"),
            "PLOYZ_NATS_URL=tls://old:4222\nPLOYZ_NATS_CA_FILE=/ca.pem\nPLOYZ_NATS_NKEY_SEED_FILE=/machine.seed\n",
        )
        .expect("write dns env");
        let successor = NatsClientUrl::try_new("tls://127.0.0.1:4222").expect("valid url");
        let mut runner = RecordingRunner::default();

        repoint_non_core_roles(env.path(), &successor, &mut runner).expect("repoint succeeds");

        assert_eq!(
            runner.systemctl_calls,
            vec![
                vec![
                    "restart".to_owned(),
                    "ployzd-machine-machine_1.service".to_owned(),
                ],
                vec!["restart".to_owned(), "ployzd-dns.service".to_owned(),],
            ]
        );
        assert_eq!(
            fs::read_to_string(env.path().join("ployzd-machine.env")).expect("machine env reads"),
            format!(
                "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE=/ca.pem\nPLOYZ_NATS_NKEY_SEED_FILE={}\nPLOYZ_MACHINE_ID=machine_1\n",
                machine_seed.display()
            ),
        );
    }

    #[test]
    fn repoint_non_core_roles_leaves_the_intent_mirror_for_promoted_control() {
        // core-promote calls repoint_non_core_roles after starting the
        // promoted ployzd-control, which reads the mirror asynchronously to
        // seed its store. Deleting the mirror here would race that first
        // read, so repoint must leave it untouched.
        let env = tempfile::tempdir().expect("env dir");
        let material_dir = env.path().join("material");
        fs::create_dir(&material_dir).expect("material dir");
        let mirror = material_dir.join("intent-mirror.json");
        fs::write(&mirror, "{}").expect("write mirror");
        let machine_seed = material_dir.join("machine.seed");
        fs::write(
            env.path().join("ployzd-machine.env"),
            format!(
                "PLOYZ_NATS_URL=tls://old:4222\nPLOYZ_NATS_CA_FILE=/ca.pem\nPLOYZ_NATS_NKEY_SEED_FILE={}\nPLOYZ_MACHINE_ID=machine_1\n",
                machine_seed.display()
            ),
        )
        .expect("write machine env");
        let successor = NatsClientUrl::try_new("tls://127.0.0.1:4222").expect("valid url");
        let mut runner = RecordingRunner::default();

        repoint_non_core_roles(env.path(), &successor, &mut runner).expect("repoint succeeds");

        assert!(mirror.exists());
    }
}
