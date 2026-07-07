use std::fs;
use std::path::{Path, PathBuf};

use crate::command::KeeperCommandRunner;
use crate::systemd::SupervisorUnitTarget;
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
    runner: &mut impl KeeperCommandRunner,
) -> Result<(), FailureMessage> {
    repoint_non_core_roles(&target.env_dir, &target.successor_nats_url, runner)?;

    let control = SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Control).unit_name();
    let nats = SupervisorUnitTarget::NatsServer.unit_name();
    runner.systemctl(&["disable", "--now", &control, &nats])
}

fn repoint_non_core_roles(
    env_dir: &Path,
    successor: &NatsClientUrl,
    runner: &mut impl KeeperCommandRunner,
) -> Result<(), FailureMessage> {
    repoint_machine_role(env_dir, successor, runner)?;
    repoint_fixed_role(
        env_dir,
        successor,
        DaemonProcessRole::Gateway,
        "ployzd-gateway.env",
        runner,
    )?;
    repoint_fixed_role(
        env_dir,
        successor,
        DaemonProcessRole::Dns,
        "ployzd-dns.env",
        runner,
    )
}

fn repoint_machine_role(
    env_dir: &Path,
    successor: &NatsClientUrl,
    runner: &mut impl KeeperCommandRunner,
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
    runner.systemctl(&[
        "restart",
        &SupervisorUnitTarget::PloyzdRole(role).unit_name(),
    ])
}

fn repoint_fixed_role(
    env_dir: &Path,
    successor: &NatsClientUrl,
    role: DaemonProcessRole,
    file_name: &str,
    runner: &mut impl KeeperCommandRunner,
) -> Result<(), FailureMessage> {
    let path = env_dir.join(file_name);
    let Some(contents) = read_optional_env(&path)? else {
        return Ok(());
    };
    write_repointed_env(&path, &contents, successor)?;
    runner.systemctl(&[
        "restart",
        &SupervisorUnitTarget::PloyzdRole(role).unit_name(),
    ])
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
    }

    impl KeeperCommandRunner for RecordingRunner {
        fn is_linux(&mut self) -> bool {
            true
        }

        fn current_uid(&mut self) -> Result<u32, FailureMessage> {
            Ok(0)
        }

        fn systemctl(&mut self, args: &[&str]) -> Result<(), FailureMessage> {
            self.systemctl_calls
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
            Ok(())
        }

        fn download(&mut self, _url: &str, _destination: &Path) -> Result<(), FailureMessage> {
            Ok(())
        }

        fn docker_info(&mut self) -> Result<(), FailureMessage> {
            Ok(())
        }

        fn enable_docker_service(&mut self) -> Result<(), FailureMessage> {
            Ok(())
        }

        fn run_docker_install_script(&mut self, _script: &Path) -> Result<(), FailureMessage> {
            Ok(())
        }

        fn prepare_dataplane_host(&mut self) -> Result<(), FailureMessage> {
            Ok(())
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
                    "--now".to_owned(),
                    "ployzd-control.service".to_owned(),
                    "nats-server.service".to_owned(),
                ]
            ]
        );
        assert!(
            fs::read_to_string(env.path().join("ployzd-machine.env"))
                .expect("machine env reads")
                .starts_with("PLOYZ_NATS_URL=tls://new:4222\n")
        );
    }
}
