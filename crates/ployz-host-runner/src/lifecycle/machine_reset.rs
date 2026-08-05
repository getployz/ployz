//! On-host teardown of Ployz-controlled machine state.

use std::fs;
use std::path::Path;
use std::time::Duration;

use ployz_core::operation::FailureMessage;

use crate::builtin_wireguard::DEFAULT_WIREGUARD_IFNAME;
use crate::{
    HostRunnerCommandRunner, PloyzdRole, SupervisorBackend, SupervisorDirectories,
    SupervisorUnitTarget, SystemHostRunnerCommandRunner,
};

use super::production::LinuxSubstrate;

const PLOYZ_STATE_DIRECTORY: &str = "/var/lib/ployz";
const PRESERVED_STORAGE_DIRECTORIES: [&str; 2] = ["volumes", "zfs"];
const CORROSION_STOP_TIMEOUT: Duration = Duration::from_secs(90);

/// Stops Ployz services and removes the local control-plane state that identifies
/// this machine while preserving workload-volume storage.
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
    let supervisor = substrate.supervisor()?;
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
    drop(substrate);
    stop_corrosion_service(supervisor, runner)?;
    remove_ployz_wireguard_interface(runner)?;
    remove_ployz_control_state(state)
}

fn stop_corrosion_service(
    supervisor: SupervisorBackend,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let (program, args) = match supervisor {
        SupervisorBackend::Systemd => ("systemctl", ["stop", "ployz-corrosion.service"]),
        SupervisorBackend::OpenRc => ("rc-service", ["ployz-corrosion", "stop"]),
    };
    let output = runner.command_with_timeout(program, &args, CORROSION_STOP_TIMEOUT)?;
    if output.success {
        return Ok(());
    }
    Err(failure(output.failure))
}

fn remove_ployz_control_state(state: &Path) -> Result<(), FailureMessage> {
    let entries = match fs::read_dir(state) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(failure(error)),
    };
    for entry in entries {
        let entry = entry.map_err(failure)?;
        if PRESERVED_STORAGE_DIRECTORIES.contains(&entry.file_name().to_string_lossy().as_ref()) {
            continue;
        }
        let file_type = entry.file_type().map_err(failure)?;
        let path = entry.path();
        if file_type.is_dir() {
            fs::remove_dir_all(path).map_err(failure)?;
        } else {
            fs::remove_file(path).map_err(failure)?;
        }
    }
    Ok(())
}

fn remove_ployz_wireguard_interface(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let output = runner.command("ip", &["link", "delete", DEFAULT_WIREGUARD_IFNAME])?;
    if output.success || wireguard_interface_is_already_absent(&output) {
        return Ok(());
    }
    Err(failure(output.failure))
}

fn wireguard_interface_is_already_absent(output: &crate::HostRunnerCommandOutput) -> bool {
    output.exit_code == Some(1)
        && (output.failure.contains(&format!(
            "Cannot find device \"{DEFAULT_WIREGUARD_IFNAME}\""
        )) || output.failure.contains(&format!(
            "Device \"{DEFAULT_WIREGUARD_IFNAME}\" does not exist"
        )))
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

    #[derive(Debug, Clone, Copy)]
    enum WireguardInterface {
        Present,
        Absent,
        DeletionFailed,
    }

    #[derive(Debug)]
    struct RecordingRunner {
        linux: bool,
        uid: u32,
        wireguard_interface: WireguardInterface,
        corrosion_stop_times_out: bool,
        calls: Vec<String>,
        timeout_calls: Vec<(String, Duration)>,
    }

    impl RecordingRunner {
        fn root_linux() -> Self {
            Self {
                linux: true,
                uid: 0,
                wireguard_interface: WireguardInterface::Present,
                corrosion_stop_times_out: false,
                calls: Vec::new(),
                timeout_calls: Vec::new(),
            }
        }

        fn respond(
            &mut self,
            program: &str,
            args: &[&str],
        ) -> Result<HostRunnerCommandOutput, FailureMessage> {
            self.calls.push(format!("{program} {}", args.join(" ")));
            let wireguard_interface_change =
                program == "ip" && args == ["link", "delete", DEFAULT_WIREGUARD_IFNAME];
            let stdout = if program == "cat" && args == ["/etc/os-release"] {
                "ID=ubuntu\nVERSION_ID=24.04\n".to_owned()
            } else {
                String::new()
            };
            let (success, exit_code, failure) =
                match (wireguard_interface_change, self.wireguard_interface) {
                    (true, WireguardInterface::Absent) => {
                        (false, Some(1), "Cannot find device \"ployz0\"".to_owned())
                    }
                    (true, WireguardInterface::DeletionFailed) => {
                        (false, Some(1), "Operation not permitted".to_owned())
                    }
                    _ => (true, Some(0), String::new()),
                };
            Ok(HostRunnerCommandOutput {
                success,
                exit_code,
                stdout,
                stdout_truncated: false,
                failure,
            })
        }
    }

    impl HostRunnerCommandRunner for RecordingRunner {
        fn command(
            &mut self,
            program: &str,
            args: &[&str],
        ) -> Result<HostRunnerCommandOutput, FailureMessage> {
            self.respond(program, args)
        }

        fn command_with_timeout(
            &mut self,
            program: &str,
            args: &[&str],
            timeout: Duration,
        ) -> Result<HostRunnerCommandOutput, FailureMessage> {
            self.timeout_calls
                .push((format!("{program} {}", args.join(" ")), timeout));
            if self.corrosion_stop_times_out
                && program == "systemctl"
                && args == ["stop", "ployz-corrosion.service"]
            {
                return Err(failure(format!(
                    "systemctl stop ployz-corrosion.service timed out after {}s",
                    timeout.as_secs()
                )));
            }
            self.respond(program, args)
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
    fn root_linux_reset_stops_only_ployz_units_and_removes_control_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = directory.path().join("ployz");
        fs::create_dir_all(state.join("subscriptions")).expect("Ployz state directory");
        fs::write(state.join("corrosion.db"), "Corrosion rows").expect("Corrosion database");
        fs::write(state.join("subscriptions/active"), "subscription").expect("Ployz state");
        fs::create_dir_all(state.join("volumes/workload")).expect("volume mount root");
        fs::write(state.join("volumes/workload/data"), "volume data").expect("workload volume");
        fs::create_dir_all(state.join("zfs")).expect("Ployz-owned pool backing directory");
        fs::write(state.join("zfs/ployz.img"), "pool backing data").expect("pool backing file");
        let docker_owned = directory.path().join("docker");
        fs::create_dir_all(&docker_owned).expect("Docker directory");
        fs::write(docker_owned.join("container"), "workload").expect("Docker workload");

        let mut runner = RecordingRunner::root_linux();
        reset_linux_machine(&state, &mut runner).expect("reset succeeds");

        assert!(state.exists());
        assert!(!state.join("corrosion.db").exists());
        assert!(!state.join("subscriptions").exists());
        assert_eq!(
            fs::read_to_string(state.join("volumes/workload/data")).expect("Ployz volume remains"),
            "volume data"
        );
        assert_eq!(
            fs::read_to_string(state.join("zfs/ployz.img")).expect("Ployz pool backing remains"),
            "pool backing data"
        );
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
            wireguard_interface: WireguardInterface::Present,
            corrosion_stop_times_out: false,
            calls: Vec::new(),
            timeout_calls: Vec::new(),
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
            wireguard_interface: WireguardInterface::Present,
            corrosion_stop_times_out: false,
            calls: Vec::new(),
            timeout_calls: Vec::new(),
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
        runner.wireguard_interface = WireguardInterface::Absent;

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

    #[test]
    fn reset_keeps_control_state_when_wireguard_interface_deletion_fails() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = directory.path().join("ployz");
        fs::create_dir_all(&state).expect("Ployz state directory");
        fs::write(state.join("corrosion.db"), "Corrosion rows").expect("Corrosion database");
        let mut runner = RecordingRunner::root_linux();
        runner.wireguard_interface = WireguardInterface::DeletionFailed;

        assert_eq!(
            reset_linux_machine(&state, &mut runner)
                .expect_err("wireguard deletion failure stops reset")
                .to_string(),
            "Operation not permitted"
        );
        assert!(state.join("corrosion.db").exists());
    }

    #[test]
    fn reset_preserves_control_state_when_corrosion_shutdown_reaches_its_bounded_timeout() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = directory.path().join("ployz");
        fs::create_dir_all(&state).expect("Ployz state directory");
        fs::write(state.join("corrosion.db"), "Corrosion rows").expect("Corrosion database");
        let mut runner = RecordingRunner::root_linux();
        runner.corrosion_stop_times_out = true;

        assert_eq!(
            reset_linux_machine(&state, &mut runner)
                .expect_err("Corrosion timeout stops reset")
                .to_string(),
            "systemctl stop ployz-corrosion.service timed out after 90s"
        );
        assert!(state.join("corrosion.db").exists());
        assert_eq!(
            runner.timeout_calls,
            [(
                "systemctl stop ployz-corrosion.service".to_owned(),
                CORROSION_STOP_TIMEOUT,
            )]
        );
        assert_eq!(
            runner.calls,
            [
                "cat /etc/os-release",
                "systemctl stop ployzd-keeper.service",
                "systemctl stop ployzd-api.service",
                "systemctl stop ployzd-gateway.service",
                "systemctl stop ployzd-dns.service",
            ]
        );
    }
}
