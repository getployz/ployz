//! On-host teardown of Ployz-controlled machine state.

use std::fs;
use std::path::Path;

use ployz_core::operation::FailureMessage;

use crate::builtin_wireguard::DEFAULT_WIREGUARD_IFNAME;
use crate::{
    HostRunnerCommandRunner, PloyzdRole, SupervisorDirectories, SupervisorUnitTarget,
    SystemHostRunnerCommandRunner,
};

use super::production::{CorrosionServiceChange, LinuxSubstrate};

const PLOYZ_STATE_DIRECTORY: &str = "/var/lib/ployz";

/// Stops Ployz services and removes the local state that identifies this machine.
pub fn run_linux_machine_reset() -> Result<(), FailureMessage> {
    let mut runner = SystemHostRunnerCommandRunner::default();
    reset_linux_machine(Path::new(PLOYZ_STATE_DIRECTORY), &mut runner)
}

/// Executes the local reset effect against one Ployz state directory.
///
/// The caller supplies the command runner so the privileged supervisor work has
/// the same test seam as founding and joining.
pub fn reset_linux_machine(
    state: &Path,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    require_linux_root(runner)?;

    let mut profile = None;
    let directories = SupervisorDirectories::host_defaults();
    let mut substrate = LinuxSubstrate::new(state, runner, &mut profile, &directories);
    for role in [
        PloyzdRole::Keeper,
        PloyzdRole::Api,
        PloyzdRole::Gateway,
        PloyzdRole::Dns,
    ] {
        substrate.run_supervisor(
            crate::SupervisorChange::Stop,
            &SupervisorUnitTarget::PloyzdRole(role),
        )?;
    }
    substrate.change_corrosion_service(CorrosionServiceChange::Stop)?;
    remove_ployz_wireguard_interface(runner)?;
    match fs::remove_dir_all(state) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(failure(error)),
    }
}

fn remove_ployz_wireguard_interface(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let output = runner.command("ip", &["link", "delete", DEFAULT_WIREGUARD_IFNAME])?;
    if output.success || output.exit_code == Some(1) {
        return Ok(());
    }
    Err(failure(output.failure))
}

fn require_linux_root(runner: &mut impl HostRunnerCommandRunner) -> Result<(), FailureMessage> {
    if !runner.is_linux() {
        return Err(failure("ployz machine reset requires Linux"));
    }
    if runner.current_uid()? != 0 {
        return Err(failure("ployz machine reset must run as root"));
    }
    Ok(())
}

fn failure(error: impl std::fmt::Display) -> FailureMessage {
    FailureMessage::try_new(error.to_string()).unwrap_or_else(|_| {
        FailureMessage::try_new("machine reset host effect failed")
            .expect("constant failure message is non-empty")
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::HostRunnerCommandOutput;

    #[derive(Debug)]
    struct RecordingRunner {
        linux: bool,
        uid: u32,
        wireguard_interface_absent: bool,
        calls: Vec<String>,
    }

    impl RecordingRunner {
        fn root_linux() -> Self {
            Self {
                linux: true,
                uid: 0,
                wireguard_interface_absent: false,
                calls: Vec::new(),
            }
        }
    }

    impl HostRunnerCommandRunner for RecordingRunner {
        fn command(
            &mut self,
            program: &str,
            args: &[&str],
        ) -> Result<HostRunnerCommandOutput, FailureMessage> {
            self.calls.push(format!("{program} {}", args.join(" ")));
            let wireguard_interface_absent = self.wireguard_interface_absent
                && program == "ip"
                && args == ["link", "delete", DEFAULT_WIREGUARD_IFNAME];
            let stdout = if program == "cat" && args == ["/etc/os-release"] {
                "ID=ubuntu\nVERSION_ID=24.04\n".to_owned()
            } else {
                String::new()
            };
            Ok(HostRunnerCommandOutput {
                success: !wireguard_interface_absent,
                exit_code: Some(if wireguard_interface_absent { 1 } else { 0 }),
                stdout,
                stdout_truncated: false,
                failure: if wireguard_interface_absent {
                    "Cannot find device \"ployz0\"".to_owned()
                } else {
                    String::new()
                },
            })
        }

        fn is_linux(&mut self) -> bool {
            self.linux
        }

        fn current_uid(&mut self) -> Result<u32, FailureMessage> {
            Ok(self.uid)
        }

        fn download(&mut self, _url: &str, _destination: &Path) -> Result<(), FailureMessage> {
            Err(failure("reset does not download artifacts"))
        }

        fn docker_info(&mut self) -> Result<(), FailureMessage> {
            Err(failure("reset does not inspect Docker"))
        }

        fn docker_is_installed(&mut self) -> bool {
            panic!("reset does not inspect Docker")
        }

        fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
            panic!("reset does not inspect Docker")
        }

        fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
            panic!("reset does not inspect Docker")
        }
    }

    #[test]
    fn root_linux_reset_stops_only_ployz_units_and_removes_ployz_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = directory.path().join("ployz");
        fs::create_dir_all(state.join("subscriptions")).expect("Ployz state directory");
        fs::write(state.join("corrosion.db"), "Corrosion rows").expect("Corrosion database");
        fs::write(state.join("subscriptions/active"), "subscription").expect("Ployz state");
        let docker_owned = directory.path().join("docker");
        fs::create_dir_all(&docker_owned).expect("Docker directory");
        fs::write(docker_owned.join("container"), "workload").expect("Docker workload");

        let mut runner = RecordingRunner::root_linux();
        reset_linux_machine(&state, &mut runner).expect("reset succeeds");

        assert!(!state.exists());
        assert_eq!(
            fs::read_to_string(docker_owned.join("container")).expect("Docker workload remains"),
            "workload"
        );
        assert_eq!(
            runner.calls,
            [
                "cat /etc/os-release",
                "systemctl stop ployzd-keeper.service",
                "systemctl stop ployzd-api.service",
                "systemctl stop ployzd-gateway.service",
                "systemctl stop ployzd-dns.service",
                "systemctl stop ployz-corrosion.service",
                "ip link delete ployz0",
            ]
        );
        assert!(runner.calls.iter().all(|call| !call.contains("docker")));
    }

    #[test]
    fn reset_refuses_non_linux_or_non_root_hosts_without_touching_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = directory.path().join("ployz");
        fs::create_dir_all(&state).expect("Ployz state directory");

        let mut non_linux = RecordingRunner {
            linux: false,
            uid: 0,
            wireguard_interface_absent: false,
            calls: Vec::new(),
        };
        assert_eq!(
            reset_linux_machine(&state, &mut non_linux)
                .expect_err("non-Linux reset refuses")
                .to_string(),
            "ployz machine reset requires Linux"
        );
        assert!(state.exists());
        assert!(non_linux.calls.is_empty());

        let mut non_root = RecordingRunner {
            linux: true,
            uid: 1_000,
            wireguard_interface_absent: false,
            calls: Vec::new(),
        };
        assert_eq!(
            reset_linux_machine(&state, &mut non_root)
                .expect_err("non-root reset refuses")
                .to_string(),
            "ployz machine reset must run as root"
        );
        assert!(state.exists());
        assert!(non_root.calls.is_empty());
    }

    #[test]
    fn reset_accepts_state_and_interface_already_removed_by_a_previous_reset() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = directory.path().join("ployz");
        let mut runner = RecordingRunner::root_linux();
        runner.wireguard_interface_absent = true;

        reset_linux_machine(&state, &mut runner).expect("absent state is already reset");

        assert!(!state.exists());
        assert_eq!(
            runner.calls,
            [
                "cat /etc/os-release",
                "systemctl stop ployzd-keeper.service",
                "systemctl stop ployzd-api.service",
                "systemctl stop ployzd-gateway.service",
                "systemctl stop ployzd-dns.service",
                "systemctl stop ployz-corrosion.service",
                "ip link delete ployz0",
            ]
        );
    }
}
