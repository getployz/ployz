//! Bounded host effects for the cluster-wide container isolation map.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::corrosion::DesiredContainerIsolation;
use ployz_core::operation::FailureMessage;

use crate::{
    FileMode, HostRunnerCommandOutput, HostRunnerCommandRunner, SystemHostRunnerCommandRunner,
    write_durable_file,
};

const MAX_ROWS_FILE_BYTES: usize = 4 * 1_024 * 1_024;
pub const DEFAULT_EBPF_PIN_PATH: &str = "/sys/fs/bpf/ployz";
pub const DEFAULT_ISOLATION_CGROUP_ROOT: &str = "/sys/fs/cgroup";
pub const DEFAULT_ISOLATION_ROWS_PATH: &str = "/run/ployz/container-isolation.rows";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerIsolationHostConfig {
    ctl_path: PathBuf,
    bytecode_path: PathBuf,
    tc_pin_path: PathBuf,
    cgroup_root: PathBuf,
    rows_path: PathBuf,
    command_timeout: Duration,
}

impl ContainerIsolationHostConfig {
    pub fn try_new(
        ctl_path: PathBuf,
        bytecode_path: PathBuf,
        tc_pin_path: PathBuf,
        cgroup_root: PathBuf,
        rows_path: PathBuf,
        command_timeout: Duration,
    ) -> Result<Self, ContainerIsolationConfigError> {
        for (field, path) in [
            ("control program", &ctl_path),
            ("bytecode", &bytecode_path),
            ("eBPF pin", &tc_pin_path),
            ("cgroup root", &cgroup_root),
            ("rows file", &rows_path),
        ] {
            validate_absolute_path(field, path)?;
        }
        if command_timeout.is_zero() {
            return Err(ContainerIsolationConfigError::ZeroCommandTimeout);
        }
        Ok(Self {
            ctl_path,
            bytecode_path,
            tc_pin_path,
            cgroup_root,
            rows_path,
            command_timeout,
        })
    }
}

fn validate_absolute_path(
    field: &'static str,
    path: &Path,
) -> Result<(), ContainerIsolationConfigError> {
    if path.as_os_str().is_empty() {
        Err(ContainerIsolationConfigError::EmptyPath { field })
    } else if !path.is_absolute() {
        Err(ContainerIsolationConfigError::RelativePath {
            field,
            value: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContainerIsolationConfigError {
    #[error("{field} path must not be empty")]
    EmptyPath { field: &'static str },
    #[error("{field} path must be absolute: {value:?}")]
    RelativePath { field: &'static str, value: PathBuf },
    #[error("container isolation command timeout must not be zero")]
    ZeroCommandTimeout,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContainerIsolationHostError {
    #[error("container isolation control program is missing: {path}")]
    MissingControlProgram { path: PathBuf },
    #[error("container isolation bytecode is missing: {path}")]
    MissingBytecode { path: PathBuf },
    #[error("container isolation bpffs pin parent is missing: {path}")]
    MissingBpffs { path: PathBuf },
    #[error("container isolation cgroup root is not cgroup v2: {path}")]
    MissingCgroupV2 { path: PathBuf },
    #[error("desired container isolation set has {desired} entries, capacity is {capacity}")]
    DesiredSetTooLarge { desired: usize, capacity: usize },
    #[error("container isolation rows file has {bytes} bytes, limit is {limit}")]
    RowsFileTooLarge { bytes: usize, limit: usize },
    #[error("container isolation host effect failed: {message}")]
    HostEffect { message: String },
}

pub struct ContainerIsolationHost<Runner = SystemHostRunnerCommandRunner> {
    config: ContainerIsolationHostConfig,
    runner: Runner,
}

impl ContainerIsolationHost<SystemHostRunnerCommandRunner> {
    #[must_use]
    pub fn new(config: ContainerIsolationHostConfig) -> Self {
        let runner = SystemHostRunnerCommandRunner::new(config.command_timeout);
        Self { config, runner }
    }
}

impl<Runner> ContainerIsolationHost<Runner>
where
    Runner: HostRunnerCommandRunner,
{
    #[must_use]
    pub const fn with_runner(config: ContainerIsolationHostConfig, runner: Runner) -> Self {
        Self { config, runner }
    }

    pub fn converge(
        &mut self,
        desired: &DesiredContainerIsolation,
    ) -> Result<(), ContainerIsolationHostError> {
        self.check_preconditions()?;
        validate_entry_count(desired.entries.len())?;
        let mut encoded = String::new();
        for entry in &desired.entries {
            use std::fmt::Write as _;
            writeln!(&mut encoded, "{} {}", entry.ip, entry.namespace_id).map_err(|error| {
                ContainerIsolationHostError::HostEffect {
                    message: format!("encode complete rows file: {error}"),
                }
            })?;
        }
        let encoded = encoded.into_bytes();
        if encoded.len() > MAX_ROWS_FILE_BYTES {
            return Err(ContainerIsolationHostError::RowsFileTooLarge {
                bytes: encoded.len(),
                limit: MAX_ROWS_FILE_BYTES,
            });
        }
        let Some(parent) = self.config.rows_path.parent() else {
            return Err(ContainerIsolationHostError::HostEffect {
                message: "rows file has no parent directory".to_owned(),
            });
        };
        fs::create_dir_all(parent).map_err(|error| ContainerIsolationHostError::HostEffect {
            message: format!("create rows directory {}: {error}", parent.display()),
        })?;
        write_durable_file(
            parent,
            self.config
                .rows_path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| ContainerIsolationHostError::HostEffect {
                    message: "rows file name is not valid Unicode".to_owned(),
                })?,
            FileMode::Plain,
            &encoded,
        )
        .map_err(host_effect)?;

        let ctl = self.config.ctl_path.display().to_string();
        let tc_pin_path = self.config.tc_pin_path.display().to_string();
        let bytecode = self.config.bytecode_path.display().to_string();
        let prefix = desired.prefix.as_string();
        let cgroup_root = self.config.cgroup_root.display().to_string();
        self.require(
            &ctl,
            &[
                "--pin-path",
                &tc_pin_path,
                "isolation",
                "ensure-attached",
                &bytecode,
                &prefix,
                &cgroup_root,
            ],
        )?;
        let rows_path = self.config.rows_path.display().to_string();
        self.require(
            &ctl,
            &[
                "--pin-path",
                &tc_pin_path,
                "isolation",
                "replace-all",
                &rows_path,
            ],
        )?;
        Ok(())
    }

    fn check_preconditions(&mut self) -> Result<(), ContainerIsolationHostError> {
        if !self.config.ctl_path.is_file() {
            return Err(ContainerIsolationHostError::MissingControlProgram {
                path: self.config.ctl_path.clone(),
            });
        }
        if !self.config.bytecode_path.is_file() {
            return Err(ContainerIsolationHostError::MissingBytecode {
                path: self.config.bytecode_path.clone(),
            });
        }
        if !self.config.cgroup_root.join("cgroup.controllers").is_file() {
            return Err(ContainerIsolationHostError::MissingCgroupV2 {
                path: self.config.cgroup_root.clone(),
            });
        }
        let Some(bpffs_root) = self.config.tc_pin_path.parent() else {
            return Err(ContainerIsolationHostError::MissingBpffs {
                path: self.config.tc_pin_path.clone(),
            });
        };
        if !bpffs_root.is_dir() {
            return Err(ContainerIsolationHostError::MissingBpffs {
                path: bpffs_root.to_path_buf(),
            });
        }
        let bpffs_root_text = bpffs_root.display().to_string();
        let filesystem = self
            .runner
            .command_with_timeout(
                "stat",
                &["-f", "-c", "%t", "--", &bpffs_root_text],
                self.config.command_timeout,
            )
            .map_err(host_effect)?;
        if !filesystem.success || filesystem.stdout.trim() != "cafe4a11" {
            return Err(ContainerIsolationHostError::MissingBpffs {
                path: bpffs_root.to_path_buf(),
            });
        }
        Ok(())
    }

    fn require(&mut self, program: &str, args: &[&str]) -> Result<(), ContainerIsolationHostError> {
        let output = self
            .runner
            .command_with_timeout(program, args, self.config.command_timeout)
            .map_err(host_effect)?;
        require_success(output)
    }
}

fn validate_entry_count(desired: usize) -> Result<(), ContainerIsolationHostError> {
    let capacity = usize::try_from(ployz_ebpf_common::MAX_ISOLATION_ENTRIES)
        .expect("isolation map capacity fits usize");
    if desired > capacity {
        Err(ContainerIsolationHostError::DesiredSetTooLarge { desired, capacity })
    } else {
        Ok(())
    }
}

fn require_success(output: HostRunnerCommandOutput) -> Result<(), ContainerIsolationHostError> {
    if output.success {
        Ok(())
    } else {
        Err(ContainerIsolationHostError::HostEffect {
            message: output.failure,
        })
    }
}

fn host_effect(error: FailureMessage) -> ContainerIsolationHostError {
    ContainerIsolationHostError::HostEffect {
        message: error.as_str().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::corrosion::ContainerIsolationEntry;
    use ployz_core::ids::CorrosionNamespaceName;
    use ployz_core::network::MachineEndpointSupernet;
    use std::sync::{Arc, Mutex};

    type RecordedCommand = (String, Vec<String>);
    type RecordedCommands = Arc<Mutex<Vec<RecordedCommand>>>;

    #[derive(Clone)]
    struct RecordingRunner {
        commands: RecordedCommands,
        bpffs_ready: bool,
    }

    impl HostRunnerCommandRunner for RecordingRunner {
        fn command(
            &mut self,
            program: &str,
            args: &[&str],
        ) -> Result<HostRunnerCommandOutput, FailureMessage> {
            self.commands.lock().expect("commands").push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            let success = true;
            Ok(HostRunnerCommandOutput {
                success,
                exit_code: Some(0),
                stdout: if program == "stat" && self.bpffs_ready {
                    "cafe4a11\n".to_owned()
                } else {
                    String::new()
                },
                stdout_truncated: false,
                failure: String::new(),
            })
        }

        fn is_linux(&mut self) -> bool {
            true
        }
        fn current_uid(&mut self) -> Result<u32, FailureMessage> {
            Ok(0)
        }
        fn download(&mut self, _: &str, _: &Path) -> Result<(), FailureMessage> {
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
        fn docker_has_insecure_registry(&mut self, _: &str) -> Result<bool, FailureMessage> {
            Ok(true)
        }
    }

    fn fixture() -> (
        tempfile::TempDir,
        ContainerIsolationHostConfig,
        RecordedCommands,
    ) {
        let directory = tempfile::tempdir().expect("tempdir");
        let ctl = directory.path().join("ployz-ebpf-control");
        let bytecode = directory.path().join("isolation.o");
        fs::write(&ctl, b"ctl").expect("ctl");
        fs::write(&bytecode, b"bytecode").expect("bytecode");
        let bpffs = directory.path().join("bpffs");
        fs::create_dir(&bpffs).expect("bpffs");
        let cgroup = directory.path().join("cgroup");
        fs::create_dir(&cgroup).expect("cgroup");
        fs::write(cgroup.join("cgroup.controllers"), b"cpu").expect("controllers");
        let rows = directory.path().join("run/container-isolation.rows");
        let commands = Arc::new(Mutex::new(Vec::new()));
        let config = ContainerIsolationHostConfig::try_new(
            ctl,
            bytecode,
            bpffs.join("isolation"),
            cgroup,
            rows,
            Duration::from_secs(1),
        )
        .expect("config");
        (directory, config, commands)
    }

    #[test]
    fn invokes_the_approved_exact_controller_commands() {
        let (_directory, config, commands) = fixture();
        let ctl = config.ctl_path.display().to_string();
        let bytecode = config.bytecode_path.display().to_string();
        let cgroup = config.cgroup_root.display().to_string();
        let rows = config.rows_path.display().to_string();
        let tc_pin = config.tc_pin_path.display().to_string();
        let runner = RecordingRunner {
            commands: Arc::clone(&commands),
            bpffs_ready: true,
        };
        let mut host = ContainerIsolationHost::with_runner(config, runner);
        host.converge(&DesiredContainerIsolation {
            prefix: MachineEndpointSupernet::try_new("10.77.0.0/16").expect("prefix"),
            entries: vec![ContainerIsolationEntry {
                ip: "10.77.1.2".parse().expect("ip"),
                namespace_id: CorrosionNamespaceName::try_new("prod").expect("namespace"),
            }],
        })
        .expect("converge");

        let commands = commands.lock().expect("commands");
        let [filesystem_probe, controller_commands @ ..] = commands.as_slice() else {
            panic!("filesystem probe and controller commands");
        };
        assert_eq!(filesystem_probe.0, "stat");
        assert_eq!(
            controller_commands,
            vec![
                (
                    ctl.clone(),
                    vec![
                        "--pin-path".to_owned(),
                        tc_pin.clone(),
                        "isolation".to_owned(),
                        "ensure-attached".to_owned(),
                        bytecode,
                        "10.77.0.0/16".to_owned(),
                        cgroup
                    ]
                ),
                (
                    ctl,
                    vec![
                        "--pin-path".to_owned(),
                        tc_pin,
                        "isolation".to_owned(),
                        "replace-all".to_owned(),
                        rows.clone()
                    ]
                ),
            ]
            .as_slice()
        );
        assert_eq!(
            fs::read_to_string(&rows).expect("complete rows file"),
            "10.77.1.2 prod\n"
        );
    }

    #[test]
    fn rejects_non_cgroup_v2_before_any_command() {
        let (_directory, config, commands) = fixture();
        fs::remove_file(config.cgroup_root.join("cgroup.controllers")).expect("remove marker");
        let mut host = ContainerIsolationHost::with_runner(
            config,
            RecordingRunner {
                commands: Arc::clone(&commands),
                bpffs_ready: true,
            },
        );
        let error = host
            .converge(&DesiredContainerIsolation {
                prefix: MachineEndpointSupernet::try_new("10.77.0.0/16").expect("prefix"),
                entries: Vec::new(),
            })
            .expect_err("missing cgroup v2");
        assert!(matches!(
            error,
            ContainerIsolationHostError::MissingCgroupV2 { .. }
        ));
        assert!(commands.lock().expect("commands").is_empty());
    }

    #[test]
    fn plain_directory_is_not_accepted_as_bpffs() {
        let (_directory, config, commands) = fixture();
        let mut host = ContainerIsolationHost::with_runner(
            config,
            RecordingRunner {
                commands: Arc::clone(&commands),
                bpffs_ready: false,
            },
        );
        let error = host
            .converge(&DesiredContainerIsolation {
                prefix: MachineEndpointSupernet::try_new("10.77.0.0/16").expect("prefix"),
                entries: Vec::new(),
            })
            .expect_err("plain directory is not bpffs");
        assert!(matches!(
            error,
            ContainerIsolationHostError::MissingBpffs { .. }
        ));
        let commands = commands.lock().expect("commands");
        let [filesystem_probe] = commands.as_slice() else {
            panic!("only filesystem probe");
        };
        assert_eq!(filesystem_probe.0, "stat");
    }

    #[test]
    fn desired_entry_bound_uses_the_shared_ebpf_capacity() {
        let capacity = usize::try_from(ployz_ebpf_common::MAX_ISOLATION_ENTRIES).expect("capacity");
        assert_eq!(validate_entry_count(capacity), Ok(()));
        assert!(matches!(
            validate_entry_count(capacity + 1),
            Err(ContainerIsolationHostError::DesiredSetTooLarge {
                desired,
                capacity: found,
            }) if desired == capacity + 1 && found == capacity
        ));
    }
}
