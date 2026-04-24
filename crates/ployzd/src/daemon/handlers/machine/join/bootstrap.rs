use std::path::{Path, PathBuf};

use ployz_api::{InstallRuntimeTarget, InstallServiceMode, InstallSource, MachineInstallOptions};

use crate::daemon::ssh::{SshOptions, run_ssh, run_ssh_with_stdin};

const REMOTE_STATUS_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" status >/dev/null";
const REMOTE_PLOYZ_VERSION_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" --version";

pub(super) async fn bootstrap_remote_machine(
    target: &str,
    install: &MachineInstallOptions,
    ssh_options: &SshOptions,
) -> Result<(), String> {
    let local_version = local_ployz_version()?;
    if let Ok(remote_version) = run_ssh(target, REMOTE_PLOYZ_VERSION_COMMAND, ssh_options).await {
        if remote_version.trim() == local_version.trim() {
            tracing::info!(
                %target,
                version = remote_version.trim(),
                "machine add bootstrap: remote ployz version already matches, skipping install"
            );
            return run_ssh(target, REMOTE_STATUS_COMMAND, ssh_options)
                .await
                .map(|_| ());
        }
        tracing::info!(
            %target,
            local_version = local_version.trim(),
            remote_version = remote_version.trim(),
            "machine add bootstrap: remote ployz version mismatch, reinstalling"
        );
    } else {
        tracing::info!(%target, "machine add bootstrap: remote ployz missing, installing");
    }

    let installer_path = ployz_install::find_installer_script()?;
    let installer = std::fs::read(&installer_path)
        .map_err(|error| format!("read installer '{}': {error}", installer_path.display()))?;
    let remote_command = format!("bash -s -- {}", install_script_args(install));
    run_ssh_with_stdin(target, &remote_command, &installer, ssh_options).await?;
    run_ssh(target, REMOTE_STATUS_COMMAND, ssh_options)
        .await
        .map(|_| ())
}

fn local_ployz_version() -> Result<String, String> {
    let ployz_path = local_ployz_path()?;
    let output = std::process::Command::new(&ployz_path)
        .arg("--version")
        .output()
        .map_err(|error| format!("run '{}' --version: {error}", ployz_path.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "'{}' --version failed (status: {}){}",
            ployz_path.display(),
            output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".into()),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn local_ployz_path() -> Result<PathBuf, String> {
    let current_exe =
        std::env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;
    let candidates = [
        current_exe.with_file_name("ployz"),
        current_exe
            .parent()
            .map(|parent| parent.join("ployz"))
            .unwrap_or_else(|| PathBuf::from("ployz")),
        PathBuf::from("/usr/local/bin/ployz"),
        PathBuf::from("/usr/bin/ployz"),
    ];
    for candidate in candidates {
        if Path::new(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err("ployz binary not found next to current daemon".into())
}

fn install_script_args(install: &MachineInstallOptions) -> String {
    let mut args = vec!["install".to_string()];
    if let Some(runtime_target) = install.runtime_target {
        args.push("--runtime".into());
        args.push(
            match runtime_target {
                InstallRuntimeTarget::Docker => "docker",
                InstallRuntimeTarget::Host => "host",
            }
            .into(),
        );
    }
    if let Some(service_mode) = install.service_mode {
        args.push("--service-mode".into());
        args.push(
            match service_mode {
                InstallServiceMode::User => "user",
                InstallServiceMode::System => "system",
            }
            .into(),
        );
    }
    if let Some(source) = &install.source {
        args.push("--source".into());
        args.push(
            match source {
                InstallSource::Release => "release",
                InstallSource::Git => "git",
            }
            .into(),
        );
    }
    if let Some(version) = &install.version {
        args.push("--version".into());
        args.push(shell_quote(version));
    }
    if let Some(git_url) = &install.git_url {
        args.push("--git-url".into());
        args.push(shell_quote(git_url));
    }
    if let Some(git_ref) = &install.git_ref {
        args.push("--git-ref".into());
        args.push(shell_quote(git_ref));
    }

    args.join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
