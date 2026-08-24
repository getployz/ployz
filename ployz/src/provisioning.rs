use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    process::Command,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use clap::ArgMatches;
use ployz_core::StorageChoice;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProvisionError {
    #[error("ssh+go provisioning is not implemented; use system ssh")]
    SshGo,
    #[error("remote machine destination is empty")]
    EmptyDestination,
    #[error("remote machine destination is required")]
    MissingDestination,
    #[error("run ssh whoami: {0}")]
    Whoami(#[source] io::Error),
    #[error("ssh whoami failed: {0}")]
    WhoamiFailed(String),
    #[error("ssh whoami returned non-UTF-8 output")]
    WhoamiUtf8,
    #[error("ssh whoami returned an empty user")]
    EmptyUser,
    #[error("check remote sudo: {0}")]
    Sudo(#[source] io::Error),
    #[error("remote user {user} needs passwordless sudo to install Ployz")]
    SudoRequired { user: String },
    #[error("run Ployz installer: {0}")]
    Install(#[source] io::Error),
    #[error("Ployz installer exited with {status}")]
    InstallFailed { status: std::process::ExitStatus },
    #[error("run this command with sudo")]
    NotRoot,
    #[error("read storage choice: {0}")]
    StorageInput(#[source] io::Error),
    #[error(transparent)]
    StorageChoice(#[from] ployz_core::ValueError),
    #[error("zfs storage preparation requires the installer; remove --no-install")]
    ZfsWithoutInstaller,
}

/// Resolve Machine storage preparation once before provisioning or enrollment.
pub(crate) fn resolve_storage(matches: &ArgMatches) -> Result<StorageChoice, ProvisionError> {
    let storage = match matches.get_one::<StorageChoice>("storage").copied() {
        Some(storage) => storage,
        None if matches.get_flag("yes")
            || !io::stdin().is_terminal()
            || !io::stdout().is_terminal() =>
        {
            StorageChoice::None
        }
        None => {
            print!(
                "Storage preparation [zfs/none] (none keeps this Machine currently stateless): "
            );
            io::stdout().flush().map_err(ProvisionError::StorageInput)?;
            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .map_err(ProvisionError::StorageInput)?;
            let answer = answer.trim();
            if answer.is_empty() {
                StorageChoice::None
            } else {
                StorageChoice::parse(answer)?
            }
        }
    };
    if storage == StorageChoice::Zfs && matches.get_flag("no-install") {
        return Err(ProvisionError::ZfsWithoutInstaller);
    }
    announce_storage(storage);
    Ok(storage)
}

pub(crate) fn announce_storage(storage: StorageChoice) {
    if storage == StorageChoice::None {
        println!("Storage: none — this Machine currently supports stateless workloads only.");
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn encoded_installer() -> String {
    STANDARD.encode(include_bytes!("../../scripts/install.sh"))
}

fn pipefail(pipeline: &str) -> String {
    format!("set -o pipefail; {pipeline}")
}

fn install_command(
    script: &str,
    version: &str,
    group_user: Option<&str>,
    via_sudo: bool,
    storage: StorageChoice,
) -> String {
    let script = shell_quote(script);
    let version = shell_quote(version);
    let group = group_user
        .map(|user| format!("PLOYZ_GROUP_ADD_USER={} ", shell_quote(user)))
        .unwrap_or_default();
    let sudo = if via_sudo { "sudo " } else { "" };
    format!(
        "printf '%s' {script} | base64 -d | {sudo}{group}PLOYZ_STORAGE={} PLOYZ_VERSION={version} bash",
        shell_quote(storage.as_str())
    )
}

fn installer_status(result: io::Result<std::process::ExitStatus>) -> Result<(), ProvisionError> {
    let status = result.map_err(ProvisionError::Install)?;
    if status.success() {
        Ok(())
    } else {
        Err(ProvisionError::InstallFailed { status })
    }
}

fn ssh_parts(destination: &str) -> Result<(String, Option<String>), ProvisionError> {
    if destination.starts_with("ssh+go://") {
        return Err(ProvisionError::SshGo);
    }
    let destination = destination
        .strip_prefix("ssh://")
        .or_else(|| destination.strip_prefix("ssh+cli://"))
        .unwrap_or(destination);
    if destination.is_empty() {
        return Err(ProvisionError::EmptyDestination);
    }
    if let Some((host, port)) = destination.rsplit_once(':')
        && !port.is_empty()
        && port.chars().all(|character| character.is_ascii_digit())
    {
        return Ok((host.to_owned(), Some(port.to_owned())));
    }
    Ok((destination.to_owned(), None))
}

fn ssh_key(matches: &ArgMatches) -> PathBuf {
    let key = matches
        .get_one::<String>("ssh-key")
        .expect("ssh-key has a default");
    key.strip_prefix("~/").map_or_else(
        || PathBuf::from(key),
        |relative| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(relative)
        },
    )
}

fn ssh_command(matches: &ArgMatches) -> Result<(Command, String), ProvisionError> {
    let destination = matches
        .get_one::<String>("destination")
        .ok_or(ProvisionError::MissingDestination)?;
    let (destination, port) = ssh_parts(destination)?;
    let mut command = Command::new("ssh");
    command.arg("-i").arg(ssh_key(matches));
    if let Some(port) = port {
        command.arg("-p").arg(port);
    }
    Ok((command, destination))
}

pub fn provision(matches: &ArgMatches, storage: StorageChoice) -> Result<(), ProvisionError> {
    let (mut whoami, destination) = ssh_command(matches)?;
    let output = whoami
        .arg(&destination)
        .arg("whoami")
        .output()
        .map_err(ProvisionError::Whoami)?;
    if !output.status.success() {
        return Err(ProvisionError::WhoamiFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let user = String::from_utf8(output.stdout).map_err(|_| ProvisionError::WhoamiUtf8)?;
    let user = user.trim();
    if user.is_empty() {
        return Err(ProvisionError::EmptyUser);
    }

    if user != "root" {
        let (mut sudo, destination) = ssh_command(matches)?;
        let status = sudo
            .arg(destination)
            .arg("sudo true")
            .status()
            .map_err(ProvisionError::Sudo)?;
        if !status.success() {
            return Err(ProvisionError::SudoRequired {
                user: user.to_owned(),
            });
        }
    }

    let encoded = encoded_installer();
    let version = matches
        .get_one::<String>("version")
        .expect("version has a default");
    let via_sudo = user != "root";
    let remote = format!(
        "bash -c {}",
        shell_quote(&pipefail(&install_command(
            &encoded,
            version,
            via_sudo.then_some(user),
            via_sudo,
            storage,
        )))
    );
    let (mut install, destination) = ssh_command(matches)?;
    installer_status(install.arg(destination).arg(remote).status())
}

/// Install and start local `ployzd` with the embedded Machine installer.
///
/// # Errors
///
/// Returns [`ProvisionError::NotRoot`] when this process is not root.
/// Returns [`ProvisionError::Install`] when the installer cannot be spawned,
/// or [`ProvisionError::InstallFailed`] when it exits non-zero.
pub fn provision_local(version: &str, storage: StorageChoice) -> Result<(), ProvisionError> {
    if !process_is_root() {
        return Err(ProvisionError::NotRoot);
    }
    let group_user = std::env::var("SUDO_USER")
        .ok()
        .filter(|user| !user.is_empty());
    installer_status(
        Command::new("bash")
            .arg("-c")
            .arg(pipefail(&install_command(
                &encoded_installer(),
                version,
                group_user.as_deref(),
                false,
                storage,
            )))
            .status(),
    )
}

pub(crate) fn process_is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|uid| uid.trim() == "0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::StorageChoice;

    #[test]
    fn embedded_installer_command_preserves_root_sudo_and_local_group() {
        assert_eq!(
            install_command("SCRIPT", "latest", None, false, StorageChoice::None),
            "printf '%s' 'SCRIPT' | base64 -d | PLOYZ_STORAGE='none' PLOYZ_VERSION='latest' bash"
        );
        assert_eq!(
            install_command("SCRIPT", "1.2.3", Some("deploy"), true, StorageChoice::Zfs,),
            "printf '%s' 'SCRIPT' | base64 -d | sudo PLOYZ_GROUP_ADD_USER='deploy' PLOYZ_STORAGE='zfs' PLOYZ_VERSION='1.2.3' bash"
        );
        assert_eq!(
            install_command("SCRIPT", "1.2.3", Some("nick"), false, StorageChoice::None,),
            "printf '%s' 'SCRIPT' | base64 -d | PLOYZ_GROUP_ADD_USER='nick' PLOYZ_STORAGE='none' PLOYZ_VERSION='1.2.3' bash"
        );
    }

    #[test]
    fn storage_resolution_honors_explicit_and_safe_noninteractive_choices() {
        assert_eq!(
            storage_matches(["ployz", "machine", "add", "root@host", "--storage", "zfs"]),
            StorageChoice::Zfs
        );
        assert_eq!(
            storage_matches(["ployz", "machine", "init", "root@host", "--storage", "none"]),
            StorageChoice::None
        );
        assert_eq!(
            storage_matches(["ployz", "machine", "add", "root@host", "--yes"]),
            StorageChoice::None
        );
    }

    #[test]
    fn zfs_requires_the_installer() {
        let matches = crate::cli::command()
            .try_get_matches_from([
                "ployz",
                "machine",
                "add",
                "root@host",
                "--storage",
                "zfs",
                "--no-install",
            ])
            .unwrap();
        assert_eq!(
            resolve_storage(
                matches
                    .subcommand_matches("machine")
                    .unwrap()
                    .subcommand_matches("add")
                    .unwrap(),
            )
            .unwrap_err()
            .to_string(),
            "zfs storage preparation requires the installer; remove --no-install"
        );
    }

    fn storage_matches<const N: usize>(args: [&str; N]) -> StorageChoice {
        let matches = crate::cli::command().try_get_matches_from(args).unwrap();
        let (_, matches) = matches
            .subcommand_matches("machine")
            .unwrap()
            .subcommand()
            .unwrap();
        resolve_storage(matches).unwrap()
    }

    #[test]
    fn local_provision_requires_root() {
        if process_is_root() {
            return;
        }
        assert!(matches!(
            provision_local(env!("CARGO_PKG_VERSION"), StorageChoice::None),
            Err(ProvisionError::NotRoot)
        ));
    }
}
