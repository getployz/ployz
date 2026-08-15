use std::{io, path::PathBuf, process::Command};

use base64::{Engine, engine::general_purpose::STANDARD};
use clap::ArgMatches;
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
    #[error("run remote Ployz installer: {0}")]
    Install(#[source] io::Error),
    #[error("remote Ployz installer exited with {status}")]
    InstallFailed { status: std::process::ExitStatus },
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn install_command(script: &str, user: &str, version: &str) -> String {
    let script = shell_quote(script);
    let version = shell_quote(version);
    if user == "root" {
        format!("printf '%s' {script} | base64 -d | PLOYZ_VERSION={version} bash")
    } else {
        let user = shell_quote(user);
        format!(
            "printf '%s' {script} | base64 -d | sudo PLOYZ_GROUP_ADD_USER={user} PLOYZ_VERSION={version} bash"
        )
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

pub fn provision(matches: &ArgMatches) -> Result<(), ProvisionError> {
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

    let encoded = STANDARD.encode(include_bytes!("../../scripts/install.sh"));
    let version = matches
        .get_one::<String>("version")
        .expect("version has a default");
    let remote = format!(
        "bash -c {}",
        shell_quote(&format!(
            "set -o pipefail; {}",
            install_command(&encoded, user, version)
        ))
    );
    let (mut install, destination) = ssh_command(matches)?;
    let status = install
        .arg(destination)
        .arg(remote)
        .status()
        .map_err(ProvisionError::Install)?;
    if status.success() {
        Ok(())
    } else {
        Err(ProvisionError::InstallFailed { status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_installer_command_preserves_root_and_sudo_paths() {
        assert_eq!(
            install_command("SCRIPT", "root", "latest"),
            "printf '%s' 'SCRIPT' | base64 -d | PLOYZ_VERSION='latest' bash"
        );
        assert_eq!(
            install_command("SCRIPT", "deploy", "1.2.3"),
            "printf '%s' 'SCRIPT' | base64 -d | sudo PLOYZ_GROUP_ADD_USER='deploy' PLOYZ_VERSION='1.2.3' bash"
        );
    }
}
