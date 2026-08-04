//! SSH bootstrap delivery and durable operator-peer key material.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use defguard_wireguard_rs::key::Key;
use ployz_core::ids::PeerId;
use ployz_core::network::WireGuardPublicKey;
use serde::{Deserialize, Serialize};
use wait_timeout::ChildExt as _;

use crate::commands::{InitCommand, InitDriver, SshTarget, render_service_urls, render_storage};

const SSH_PROCESS_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshPeerKey {
    pub peer_id: PeerId,
    pub peer_name: String,
    private_key: String,
    pub public_key: WireGuardPublicKey,
}

impl fmt::Debug for SshPeerKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SshPeerKey")
            .field("peer_id", &self.peer_id)
            .field("peer_name", &self.peer_name)
            .field("private_key", &"[REDACTED]")
            .field("public_key", &self.public_key)
            .finish()
    }
}

impl SshPeerKey {
    pub fn load_or_create(
        config_home: &Path,
        target: &SshTarget,
        peer_name: String,
    ) -> Result<Self, SshInitError> {
        let directory = config_home.join("mesh/init-peers");
        fs::create_dir_all(&directory).map_err(SshInitError::Filesystem)?;
        set_private_directory(&directory)?;
        let path = directory.join(format!("{}.json", hex(target.as_str().as_bytes())));
        match fs::read(&path) {
            Ok(bytes) => {
                protect_key_file(&path)?;
                return decode_key(&bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(SshInitError::Filesystem(error)),
        }

        let private_key = Key::generate();
        let public_key = WireGuardPublicKey::try_new(private_key.public_key().to_string())
            .map_err(|error| SshInitError::InvalidKey(error.to_string()))?;
        let key = Self {
            peer_id: PeerId::generate(),
            peer_name,
            private_key: private_key.to_string(),
            public_key,
        };
        let encoded = serde_json::to_vec_pretty(&key)
            .map_err(|error| SshInitError::InvalidKey(error.to_string()))?;
        let temporary = directory.join(format!(".{}.tmp", PeerId::generate()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(SshInitError::Filesystem)?;
        file.write_all(&encoded).map_err(SshInitError::Filesystem)?;
        file.sync_all().map_err(SshInitError::Filesystem)?;
        drop(file);
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {
                fs::remove_file(&temporary).map_err(SshInitError::Filesystem)?;
                sync_directory(&directory)?;
                Ok(key)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temporary);
                decode_key(&fs::read(path).map_err(SshInitError::Filesystem)?)
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(SshInitError::Filesystem(error))
            }
        }
    }

    #[must_use]
    pub fn remote_script(&self, command: &InitCommand) -> String {
        let mut args = vec!["sudo".to_owned(), "ployz".to_owned(), "init".to_owned()];
        args.extend([
            "--storage".to_owned(),
            render_storage(command.storage).to_owned(),
        ]);
        args.extend([
            "--container-network".to_owned(),
            command.container_network.as_string(),
            "--service-urls".to_owned(),
            render_service_urls(&command.service_urls),
        ]);
        push_option(&mut args, "--cluster-name", command.cluster_name.as_deref());
        push_option(&mut args, "--machine-name", command.machine_name.as_deref());
        let inferred_endpoint = match &command.driver {
            InitDriver::SshTarget(target) => target.inferred_wireguard_endpoint(),
            InitDriver::OnHost | InitDriver::Cloud(_) | InitDriver::SshPeer(_) => None,
        };
        if let Some(endpoint) = command.wireguard_endpoint.or(inferred_endpoint) {
            args.extend(["--wireguard-endpoint".to_owned(), endpoint.to_string()]);
        }
        args.extend([
            "--driver-peer-id".to_owned(),
            self.peer_id.to_string(),
            "--driver-peer-name".to_owned(),
            self.peer_name.clone(),
            "--driver-peer-public-key".to_owned(),
            self.public_key.as_str().to_owned(),
        ]);
        let init_line = args
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "curl -fsSL --connect-timeout 10 --max-time 120 --retry 3 https://ployz.sh | sh && {init_line}"
        )
    }

    pub fn run(&self, target: &SshTarget, command: &InitCommand) -> Result<(), SshInitError> {
        let script = self.remote_script(command);
        let remote_command = format!("sh -lc {}", shell_quote(&script));
        let mut child = Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "--",
                target.as_str(),
                &remote_command,
            ])
            .spawn()
            .map_err(SshInitError::Launch)?;
        let Some(status) = child
            .wait_timeout(SSH_PROCESS_TIMEOUT)
            .map_err(SshInitError::Launch)?
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SshInitError::TimedOut);
        };
        if status.success() {
            Ok(())
        } else {
            Err(SshInitError::RemoteFailed {
                code: status.code(),
            })
        }
    }
}

fn decode_key(bytes: &[u8]) -> Result<SshPeerKey, SshInitError> {
    let key: SshPeerKey = serde_json::from_slice(bytes)
        .map_err(|error| SshInitError::InvalidKey(error.to_string()))?;
    let private = Key::try_from(key.private_key.as_str())
        .map_err(|error| SshInitError::InvalidKey(error.to_string()))?;
    let expected = private.public_key().to_string();
    if expected != key.public_key.as_str() {
        return Err(SshInitError::InvalidKey(
            "stored public and private WireGuard keys disagree".to_owned(),
        ));
    }
    Ok(key)
}

fn push_option(args: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        args.extend([flag.to_owned(), value.to_owned()]);
    }
}

#[must_use]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn set_private_directory(path: &Path) -> Result<(), SshInitError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(SshInitError::Filesystem)?;
    }
    Ok(())
}

fn protect_key_file(path: &Path) -> Result<(), SshInitError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(SshInitError::Filesystem)?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), SshInitError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(SshInitError::Filesystem)
}

#[must_use]
pub fn default_config_home(home: &Path) -> PathBuf {
    home.join(".ployz")
}

#[derive(Debug, thiserror::Error)]
pub enum SshInitError {
    #[error("could not persist SSH driver peer: {0}")]
    Filesystem(std::io::Error),
    #[error("SSH driver peer key is invalid: {0}")]
    InvalidKey(String),
    #[error("could not launch ssh: {0}")]
    Launch(std::io::Error),
    #[error("SSH init exceeded its bounded deadline")]
    TimedOut,
    #[error("remote init failed{code_suffix}", code_suffix = code.map(|code| format!(" with exit code {code}")).unwrap_or_default())]
    RemoteFailed { code: Option<i32> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, parse_command};

    fn init(args: &[&str]) -> InitCommand {
        let Command::Init(command) =
            parse_command(args.iter().map(ToString::to_string)).expect("init parses")
        else {
            panic!("expected init")
        };
        *command
    }

    #[test]
    fn key_is_persisted_and_reused_for_retry() {
        let temp = tempfile::tempdir().expect("temp dir");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let first = SshPeerKey::load_or_create(temp.path(), &target, "laptop".to_owned())
            .expect("first key");
        let second = SshPeerKey::load_or_create(temp.path(), &target, "ignored".to_owned())
            .expect("same key");
        assert_eq!(first, second);
        assert_eq!(
            fs::read_dir(temp.path().join("mesh/init-peers"))
                .expect("key directory")
                .count(),
            1
        );
    }

    #[test]
    fn remote_line_stages_then_runs_exact_on_host_primitive() {
        let temp = tempfile::tempdir().expect("temp dir");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let key = SshPeerKey::load_or_create(temp.path(), &target, "Nick's laptop".to_owned())
            .expect("key");
        let script = key.remote_script(&init(&[
            "init",
            "root@203.0.113.7",
            "--storage",
            "plain",
            "--service-urls",
            "custom:apps.example.com",
        ]));
        assert!(script.starts_with(
            "curl -fsSL --connect-timeout 10 --max-time 120 --retry 3 https://ployz.sh | sh && 'sudo' 'ployz' 'init'"
        ));
        assert!(script.contains("'--driver-peer-public-key'"));
        assert!(script.contains("'--wireguard-endpoint' '203.0.113.7:51820'"));
        assert!(script.contains("'Nick'\"'\"'s laptop'"));
        assert!(!script.contains(&key.private_key));
        assert!(!format!("{key:?}").contains(&key.private_key));
    }

    #[test]
    fn shell_quoting_cannot_open_a_second_command() {
        assert_eq!(shell_quote("x'; reboot #"), "'x'\"'\"'; reboot #'");
    }

    #[test]
    fn explicit_wireguard_endpoint_wins_over_ssh_ip() {
        let temp = tempfile::tempdir().expect("temp dir");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let key =
            SshPeerKey::load_or_create(temp.path(), &target, "laptop".to_owned()).expect("key");
        let script = key.remote_script(&init(&[
            "init",
            "root@203.0.113.7",
            "--wireguard-endpoint",
            "198.51.100.8:51999",
        ]));
        assert!(script.contains("'--wireguard-endpoint' '198.51.100.8:51999'"));
        assert!(!script.contains("203.0.113.7:51820"));
    }
}
