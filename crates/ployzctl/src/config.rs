//! Local cluster context: the on-disk record of the cluster that
//! `ployzctl machine init` created, so later commands can connect without
//! `--nats` or environment variables.
//!
//! Precedence stays explicit: `--nats` beats environment variables, which
//! beat this file. The context stores no secrets directly — it points at the
//! operator seed and CA files — but it is still written with restrictive
//! permissions and an atomic rename so a crash never leaves a torn file.

use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::ssh::{SshTarget, SshTargetParseError};
use ployz_core::ids::{MachineId, SubjectTokenError};
use ployz_nats::connect::{NatsClientUrl, NatsClientUrlError};
use serde::{Deserialize, Serialize};

/// Directory under the user config root that holds `ployzctl` state.
pub const PLOYZ_CONFIG_DIR_NAME: &str = "ployz";
/// File name of the active cluster context inside the config directory.
pub const CLUSTER_CONTEXT_FILE_NAME: &str = "context.json";

#[cfg(unix)]
const CLUSTER_CONTEXT_FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const CLUSTER_CONTEXT_DIR_MODE: u32 = 0o700;

/// Connection material for the active cluster, as recorded by
/// `ployzctl machine init`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterContext {
    pub nats_url: NatsClientUrl,
    pub nats_ca_file: PathBuf,
    pub operator_seed_file: PathBuf,
    /// The cluster-static low-privilege Join seed `machine add` embeds in
    /// install lines. Contexts written before remote machine add carry none.
    pub join_seed_file: Option<PathBuf>,
    /// Optional local delivery handles for machines in this cluster.
    pub machines: Vec<ClusterContextMachine>,
}

impl ClusterContext {
    #[must_use]
    pub fn with_machine_ssh(mut self, machine_id: MachineId, target: SshTarget) -> Self {
        match self
            .machines
            .iter_mut()
            .find(|machine| machine.machine_id == machine_id)
        {
            Some(machine) => machine.ssh = Some(target),
            None => self.machines.push(ClusterContextMachine {
                machine_id,
                ssh: Some(target),
            }),
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterContextMachine {
    pub machine_id: MachineId,
    pub ssh: Option<SshTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterContextMaterial<'material> {
    pub ca_pem: &'material str,
    pub operator_seed: &'material str,
    pub join_seed: Option<&'material str>,
}

/// Serde shape of the context file. Kept separate from [`ClusterContext`] so
/// the in-memory type can hold the validated [`NatsClientUrl`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClusterContextFile {
    nats_url: String,
    nats_ca_file: PathBuf,
    operator_seed_file: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    join_seed_file: Option<PathBuf>,
    machines: Vec<ClusterContextMachineFile>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClusterContextMachineFile {
    machine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ssh: Option<String>,
}

/// Resolves the default context file path from `XDG_CONFIG_HOME` or
/// `HOME/.config`. `None` means the environment names no config root, which
/// callers treat the same as a missing context file.
#[must_use]
pub fn default_cluster_context_path() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(
        config_home
            .join(PLOYZ_CONFIG_DIR_NAME)
            .join(CLUSTER_CONTEXT_FILE_NAME),
    )
}

pub fn load_cluster_context(path: &Path) -> Result<Option<ClusterContext>, ClusterContextError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ClusterContextError::Read {
                path: path.to_owned(),
                message: error.to_string(),
            });
        }
    };
    let file: ClusterContextFile =
        serde_json::from_str(&raw).map_err(|error| ClusterContextError::Parse {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    let ClusterContextFile {
        nats_url,
        nats_ca_file,
        operator_seed_file,
        join_seed_file,
        machines,
    } = file;
    let nats_url =
        NatsClientUrl::try_new(nats_url).map_err(|error| ClusterContextError::InvalidNatsUrl {
            path: path.to_owned(),
            error,
        })?;
    let machines = machines
        .into_iter()
        .map(|machine| load_context_machine(path, machine))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(ClusterContext {
        nats_url,
        nats_ca_file,
        operator_seed_file,
        join_seed_file,
        machines,
    }))
}

/// Publishes one complete cluster context generation. Material files are
/// written into a fresh generation directory first; only after every file is
/// durable does the context atomically swap to point at that generation.
pub fn publish_cluster_context(
    path: &Path,
    nats_url: NatsClientUrl,
    material: ClusterContextMaterial<'_>,
) -> Result<ClusterContext, ClusterContextError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Err(ClusterContextError::Write {
            path: path.to_owned(),
            message: "context path has no parent directory".to_owned(),
        });
    };
    prepare_config_directory(parent)?;

    let material_dir = create_material_generation_dir(parent)?;
    let ca_file = material_dir.join("ca.pem");
    let operator_seed_file = material_dir.join("operator.seed");
    let join_seed_file = material.join_seed.map(|_| material_dir.join("join.seed"));

    let publish = (|| {
        write_new_material_file(&ca_file, material.ca_pem)?;
        write_new_material_file(&operator_seed_file, material.operator_seed)?;
        if let (Some(path), Some(contents)) = (&join_seed_file, material.join_seed) {
            write_new_material_file(path, contents)?;
        }

        let context = ClusterContext {
            nats_url,
            nats_ca_file: ca_file,
            operator_seed_file,
            join_seed_file,
            machines: Vec::new(),
        };
        save_cluster_context(path, &context)?;
        Ok(context)
    })();

    if publish.is_err() {
        let _ = fs::remove_dir_all(&material_dir);
    }
    publish
}

pub fn save_cluster_context_machine_ssh(
    path: &Path,
    machine_id: MachineId,
    target: SshTarget,
) -> Result<Option<ClusterContext>, ClusterContextError> {
    let Some(context) = load_cluster_context(path)? else {
        return Ok(None);
    };
    let context = context.with_machine_ssh(machine_id, target);
    save_cluster_context(path, &context)?;
    Ok(Some(context))
}

/// Writes the context atomically: the payload lands in a same-directory temp
/// file with owner-only permissions, then renames over the final path.
pub fn save_cluster_context(
    path: &Path,
    context: &ClusterContext,
) -> Result<(), ClusterContextError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Err(ClusterContextError::Write {
            path: path.to_owned(),
            message: "context path has no parent directory".to_owned(),
        });
    };
    prepare_config_directory(parent)?;

    let file = ClusterContextFile {
        nats_url: context.nats_url.as_str().to_owned(),
        nats_ca_file: context.nats_ca_file.clone(),
        operator_seed_file: context.operator_seed_file.clone(),
        join_seed_file: context.join_seed_file.clone(),
        machines: context.machines.iter().map(save_context_machine).collect(),
    };
    let mut payload = serde_json::to_vec_pretty(&file).expect("cluster context serializes");
    payload.push(b'\n');

    let write_error = |error: std::io::Error| ClusterContextError::Write {
        path: path.to_owned(),
        message: error.to_string(),
    };
    let mut temp_file = tempfile::NamedTempFile::new_in(parent).map_err(write_error)?;
    temp_file
        .write_all(&payload)
        .and_then(|()| temp_file.as_file().sync_all())
        .map_err(write_error)?;
    temp_file
        .persist(path)
        .map(|_| ())
        .map_err(|error| ClusterContextError::Write {
            path: path.to_owned(),
            message: error.error.to_string(),
        })?;
    Ok(())
}

/// Writes collected cluster material (CA, seeds) next to the context file
/// with owner-only permissions. Overwrites any prior file at the path.
pub fn save_cluster_material_file(path: &Path, contents: &str) -> Result<(), ClusterContextError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Err(ClusterContextError::Write {
            path: path.to_owned(),
            message: "material path has no parent directory".to_owned(),
        });
    };
    prepare_config_directory(parent)?;
    write_replaced_material_file(path, contents)
}

fn prepare_config_directory(parent: &Path) -> Result<(), ClusterContextError> {
    fs::create_dir_all(parent).map_err(|error| ClusterContextError::CreateConfigDirectory {
        path: parent.to_owned(),
        message: error.to_string(),
    })?;
    restrict_directory_mode(parent)
}

fn create_material_generation_dir(parent: &Path) -> Result<PathBuf, ClusterContextError> {
    let root = parent.join("materials");
    prepare_config_directory(&root)?;
    let directory = tempfile::Builder::new()
        .prefix("material-")
        .tempdir_in(&root)
        .map_err(|error| ClusterContextError::CreateConfigDirectory {
            path: root.clone(),
            message: error.to_string(),
        })?;
    let path = directory.keep();
    restrict_directory_mode(&path)?;
    Ok(path)
}

fn write_new_material_file(path: &Path, contents: &str) -> Result<(), ClusterContextError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    write_material_file(path, contents, options)
}

fn write_replaced_material_file(path: &Path, contents: &str) -> Result<(), ClusterContextError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Err(ClusterContextError::Write {
            path: path.to_owned(),
            message: "material path has no parent directory".to_owned(),
        });
    };

    let mut temp_file =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| ClusterContextError::Write {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    temp_file
        .write_all(contents.as_bytes())
        .and_then(|()| temp_file.as_file().sync_all())
        .map_err(|error| ClusterContextError::Write {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    temp_file
        .persist(path)
        .map(|_| ())
        .map_err(|error| ClusterContextError::Write {
            path: path.to_owned(),
            message: error.error.to_string(),
        })?;
    Ok(())
}

fn write_material_file(
    path: &Path,
    contents: &str,
    mut options: fs::OpenOptions,
) -> Result<(), ClusterContextError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(CLUSTER_CONTEXT_FILE_MODE);
    }
    let write_error = |error: std::io::Error| ClusterContextError::Write {
        path: path.to_owned(),
        message: error.to_string(),
    };
    let mut file = options.open(path).map_err(write_error)?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(write_error)
}

#[cfg(unix)]
fn restrict_directory_mode(parent: &Path) -> Result<(), ClusterContextError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(parent, fs::Permissions::from_mode(CLUSTER_CONTEXT_DIR_MODE)).map_err(
        |error| ClusterContextError::CreateConfigDirectory {
            path: parent.to_owned(),
            message: error.to_string(),
        },
    )
}

#[cfg(not(unix))]
fn restrict_directory_mode(_parent: &Path) -> Result<(), ClusterContextError> {
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterContextError {
    CreateConfigDirectory {
        path: PathBuf,
        message: String,
    },
    Read {
        path: PathBuf,
        message: String,
    },
    Parse {
        path: PathBuf,
        message: String,
    },
    InvalidNatsUrl {
        path: PathBuf,
        error: NatsClientUrlError,
    },
    InvalidMachineId {
        path: PathBuf,
        machine_id: String,
        error: SubjectTokenError,
    },
    InvalidMachineSshTarget {
        path: PathBuf,
        target: String,
        error: SshTargetParseError,
    },
    Write {
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for ClusterContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateConfigDirectory { path, message } => write!(
                formatter,
                "could not prepare config directory {}: {message}",
                path.display()
            ),
            Self::Read { path, message } => write!(
                formatter,
                "cluster context file {} is unreadable: {message}",
                path.display()
            ),
            Self::Parse { path, message } => write!(
                formatter,
                "cluster context file {} is invalid: {message}",
                path.display()
            ),
            Self::InvalidNatsUrl { path, error } => write!(
                formatter,
                "cluster context file {} has an invalid nats_url: {error:?}",
                path.display()
            ),
            Self::InvalidMachineId {
                path,
                machine_id,
                error,
            } => write!(
                formatter,
                "cluster context file {} has an invalid machine id {machine_id:?}: {error}",
                path.display()
            ),
            Self::InvalidMachineSshTarget {
                path,
                target,
                error,
            } => write!(
                formatter,
                "cluster context file {} has an invalid ssh target {target:?}: {error}",
                path.display()
            ),
            Self::Write { path, message } => write!(
                formatter,
                "could not write cluster context file {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ClusterContextError {}

fn load_context_machine(
    path: &Path,
    machine: ClusterContextMachineFile,
) -> Result<ClusterContextMachine, ClusterContextError> {
    let ClusterContextMachineFile { machine_id, ssh } = machine;
    let machine_id = MachineId::try_new(machine_id.clone()).map_err(|error| {
        ClusterContextError::InvalidMachineId {
            path: path.to_owned(),
            machine_id,
            error,
        }
    })?;
    let ssh = ssh
        .map(|target| {
            SshTarget::parse(&target).map_err(|error| {
                ClusterContextError::InvalidMachineSshTarget {
                    path: path.to_owned(),
                    target,
                    error,
                }
            })
        })
        .transpose()?;
    Ok(ClusterContextMachine { machine_id, ssh })
}

fn save_context_machine(machine: &ClusterContextMachine) -> ClusterContextMachineFile {
    ClusterContextMachineFile {
        machine_id: machine.machine_id.as_str().to_owned(),
        ssh: machine.ssh.as_ref().map(SshTarget::destination),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(url: &str) -> ClusterContext {
        ClusterContext {
            nats_url: NatsClientUrl::try_new(url).expect("test NATS URL is valid"),
            nats_ca_file: PathBuf::from("/etc/ployz/nats/ca.pem"),
            operator_seed_file: PathBuf::from("/etc/ployz/nats/operator.seed"),
            join_seed_file: None,
            machines: Vec::new(),
        }
    }

    #[test]
    fn context_round_trips_the_join_seed_file() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join(CLUSTER_CONTEXT_FILE_NAME);
        let mut saved = context("tls://203.0.113.10:4222");
        saved.join_seed_file = Some(PathBuf::from("/etc/ployz/nats/join.seed"));

        save_cluster_context(&path, &saved).expect("context saves");
        let loaded = load_cluster_context(&path).expect("context loads");

        assert_eq!(loaded, Some(saved));
    }

    #[test]
    fn context_round_trips_machine_ssh_handles() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join(CLUSTER_CONTEXT_FILE_NAME);
        let saved = context("tls://203.0.113.10:4222").with_machine_ssh(
            MachineId::try_new("sg-core-1").expect("valid machine id"),
            SshTarget::parse("root@203.0.113.10").expect("valid ssh target"),
        );

        save_cluster_context(&path, &saved).expect("context saves");
        let loaded = load_cluster_context(&path).expect("context loads");

        assert_eq!(loaded, Some(saved));
    }

    #[test]
    fn context_machine_ssh_update_replaces_the_existing_handle() {
        let context = context("tls://203.0.113.10:4222")
            .with_machine_ssh(
                MachineId::try_new("sg-core-1").expect("valid machine id"),
                SshTarget::parse("root@203.0.113.10").expect("valid ssh target"),
            )
            .with_machine_ssh(
                MachineId::try_new("sg-core-1").expect("valid machine id"),
                SshTarget::parse("admin@203.0.113.10").expect("valid ssh target"),
            );

        let [machine] = context.machines.as_slice() else {
            panic!("expected one recorded machine");
        };
        assert_eq!(
            machine.ssh.as_ref().map(SshTarget::destination),
            Some("admin@203.0.113.10".to_owned())
        );
    }

    #[test]
    fn save_machine_ssh_updates_an_existing_context() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join(CLUSTER_CONTEXT_FILE_NAME);
        save_cluster_context(&path, &context("tls://203.0.113.10:4222")).expect("context saves");

        let updated = save_cluster_context_machine_ssh(
            &path,
            MachineId::try_new("sg-core-1").expect("valid machine id"),
            SshTarget::parse("root@203.0.113.10").expect("valid ssh target"),
        )
        .expect("machine ssh saves")
        .expect("context exists");

        let [machine] = updated.machines.as_slice() else {
            panic!("expected one recorded machine");
        };
        assert_eq!(
            machine.ssh.as_ref().map(SshTarget::destination),
            Some("root@203.0.113.10".to_owned())
        );
        assert_eq!(
            load_cluster_context(&path).expect("context loads"),
            Some(updated)
        );
    }

    #[test]
    fn save_machine_ssh_ignores_a_missing_context() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join(CLUSTER_CONTEXT_FILE_NAME);

        let updated = save_cluster_context_machine_ssh(
            &path,
            MachineId::try_new("sg-core-1").expect("valid machine id"),
            SshTarget::parse("root@203.0.113.10").expect("valid ssh target"),
        )
        .expect("missing context is not an error");

        assert_eq!(updated, None);
    }

    /// Some contexts may not carry a Join seed if they are only used for
    /// operator reads.
    #[test]
    fn context_without_join_seed_field_still_loads() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join(CLUSTER_CONTEXT_FILE_NAME);
        fs::write(
            &path,
            r#"{"nats_url":"tls://203.0.113.10:4222","nats_ca_file":"/etc/ployz/nats/ca.pem","operator_seed_file":"/etc/ployz/nats/operator.seed","machines":[]}"#,
        )
        .expect("context file writes");

        let loaded = load_cluster_context(&path).expect("context loads");

        assert_eq!(loaded, Some(context("tls://203.0.113.10:4222")));
    }

    #[test]
    fn context_without_machines_field_is_invalid() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join(CLUSTER_CONTEXT_FILE_NAME);
        fs::write(
            &path,
            r#"{"nats_url":"tls://203.0.113.10:4222","nats_ca_file":"/etc/ployz/nats/ca.pem","operator_seed_file":"/etc/ployz/nats/operator.seed","join_seed_file":"/etc/ployz/nats/join.seed"}"#,
        )
        .expect("context file writes");

        let error = load_cluster_context(&path).expect_err("missing machines is invalid");

        assert!(error.to_string().contains("missing field `machines`"));
    }

    #[test]
    fn material_files_are_owner_only_and_overwritable() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("ployz").join("operator.seed");

        save_cluster_material_file(&path, "SUAAAA").expect("material saves");
        save_cluster_material_file(&path, "SUBBBB").expect("material resaves");

        assert_eq!(fs::read_to_string(&path).expect("material reads"), "SUBBBB");
        ployz_test_support::fs::assert_file_mode(&path, 0o600);
    }

    #[test]
    fn publish_context_uses_a_complete_material_generation() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("ployz").join(CLUSTER_CONTEXT_FILE_NAME);

        let first = publish_cluster_context(
            &path,
            NatsClientUrl::try_new("tls://203.0.113.10:4222").expect("valid nats url"),
            ClusterContextMaterial {
                ca_pem: "first-ca",
                operator_seed: "first-operator",
                join_seed: Some("first-join"),
            },
        )
        .expect("first context publishes");
        let second = publish_cluster_context(
            &path,
            NatsClientUrl::try_new("tls://203.0.113.11:4222").expect("valid nats url"),
            ClusterContextMaterial {
                ca_pem: "second-ca",
                operator_seed: "second-operator",
                join_seed: Some("second-join"),
            },
        )
        .expect("second context publishes");

        assert_ne!(
            first.nats_ca_file.parent(),
            second.nats_ca_file.parent(),
            "each publish gets its own material generation"
        );
        assert_eq!(
            fs::read_to_string(&first.nats_ca_file).expect("first ca reads"),
            "first-ca"
        );
        assert_eq!(
            fs::read_to_string(&second.nats_ca_file).expect("second ca reads"),
            "second-ca"
        );
        assert_eq!(
            fs::read_to_string(second.join_seed_file.as_ref().expect("join seed exists"))
                .expect("second join reads"),
            "second-join"
        );
        assert_eq!(
            load_cluster_context(&path).expect("context loads"),
            Some(second.clone())
        );
        ployz_test_support::fs::assert_file_mode(&second.nats_ca_file, 0o600);
        ployz_test_support::fs::assert_file_mode(
            second.nats_ca_file.parent().expect("material dir exists"),
            0o700,
        );
    }

    #[test]
    fn save_then_load_round_trips_the_context() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("ployz").join(CLUSTER_CONTEXT_FILE_NAME);
        let saved = context("tls://203.0.113.10:4222");

        save_cluster_context(&path, &saved).expect("context saves");
        let loaded = load_cluster_context(&path).expect("context loads");

        assert_eq!(loaded, Some(saved));
    }

    #[test]
    fn save_writes_owner_only_file_and_directory_permissions() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let config_dir = dir.path().join("ployz");
        let path = config_dir.join(CLUSTER_CONTEXT_FILE_NAME);

        save_cluster_context(&path, &context("tls://203.0.113.10:4222")).expect("context saves");

        ployz_test_support::fs::assert_file_mode(&path, 0o600);
        ployz_test_support::fs::assert_file_mode(&config_dir, 0o700);
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let config_dir = dir.path().join("ployz");
        let path = config_dir.join(CLUSTER_CONTEXT_FILE_NAME);

        save_cluster_context(&path, &context("tls://203.0.113.10:4222")).expect("context saves");
        save_cluster_context(&path, &context("tls://203.0.113.11:4222")).expect("context resaves");

        let entries: Vec<_> = fs::read_dir(&config_dir)
            .expect("config dir is listable")
            .map(|entry| entry.expect("dir entry is readable").file_name())
            .collect();
        assert_eq!(
            entries,
            vec![std::ffi::OsString::from(CLUSTER_CONTEXT_FILE_NAME)]
        );

        let reloaded = load_cluster_context(&path).expect("context loads");
        assert_eq!(reloaded, Some(context("tls://203.0.113.11:4222")));
    }

    #[test]
    fn load_missing_context_is_none() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("absent").join(CLUSTER_CONTEXT_FILE_NAME);

        assert_eq!(
            load_cluster_context(&path).expect("missing file loads"),
            None
        );
    }

    #[test]
    fn load_rejects_invalid_json_with_the_file_path() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join(CLUSTER_CONTEXT_FILE_NAME);
        fs::write(&path, "{not json").expect("corrupt file writes");

        let error = load_cluster_context(&path).expect_err("corrupt context fails");

        let ClusterContextError::Parse {
            path: error_path, ..
        } = &error
        else {
            panic!("expected parse error, got {error:?}");
        };
        assert_eq!(error_path, &path);
        assert!(error.to_string().contains("cluster context file"));
    }

    #[test]
    fn load_rejects_invalid_nats_url() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join(CLUSTER_CONTEXT_FILE_NAME);
        fs::write(
            &path,
            r#"{"nats_url":"tls://bad url","nats_ca_file":"/ca.pem","operator_seed_file":"/op.seed","machines":[]}"#,
        )
        .expect("context file writes");

        let error = load_cluster_context(&path).expect_err("invalid nats_url fails");

        assert!(matches!(error, ClusterContextError::InvalidNatsUrl { .. }));
        assert!(error.to_string().contains("invalid nats_url"));
    }

    #[test]
    fn load_rejects_invalid_machine_ssh_target() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join(CLUSTER_CONTEXT_FILE_NAME);
        fs::write(
            &path,
            r#"{"nats_url":"tls://203.0.113.10:4222","nats_ca_file":"/ca.pem","operator_seed_file":"/op.seed","machines":[{"machine_id":"sg-core-1","ssh":"-oProxyCommand=bad"}]}"#,
        )
        .expect("context file writes");

        let error = load_cluster_context(&path).expect_err("invalid ssh target fails");

        assert!(matches!(
            error,
            ClusterContextError::InvalidMachineSshTarget { .. }
        ));
        assert!(error.to_string().contains("invalid ssh target"));
    }
}
