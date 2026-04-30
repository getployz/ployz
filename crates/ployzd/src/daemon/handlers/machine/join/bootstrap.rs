use std::path::{Path, PathBuf};

use ployz_api::{InstallRuntimeTarget, InstallServiceMode, InstallSource, MachineInstallOptions};

use crate::daemon::ssh::{SshOptions, run_ssh, run_ssh_with_stdin};

const REMOTE_STATUS_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployzctl\" status >/dev/null";
const REMOTE_PLOYZCTL_VERSION_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployzctl\" --version";

pub(super) async fn bootstrap_remote_machine(
    target: &str,
    install: &MachineInstallOptions,
    ssh_options: &SshOptions,
) -> Result<(), String> {
    let local_version = local_ployzctl_version()?;
    if let Ok(remote_version) = run_ssh(target, REMOTE_PLOYZCTL_VERSION_COMMAND, ssh_options).await
    {
        if remote_version.trim() == local_version.trim() {
            tracing::info!(
                %target,
                version = remote_version.trim(),
                "machine add bootstrap: remote ployzctl version already matches, skipping install"
            );
            return run_ssh(target, REMOTE_STATUS_COMMAND, ssh_options)
                .await
                .map(|_| ());
        }
        tracing::info!(
            %target,
            local_version = local_version.trim(),
            remote_version = remote_version.trim(),
            "machine add bootstrap: remote ployzctl version mismatch, reinstalling"
        );
    } else {
        tracing::info!(%target, "machine add bootstrap: remote ployzctl missing, installing");
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

fn local_ployzctl_version() -> Result<String, String> {
    let ployzctl_path = local_ployzctl_path()?;
    let output = std::process::Command::new(&ployzctl_path)
        .arg("--version")
        .output()
        .map_err(|error| format!("run '{}' --version: {error}", ployzctl_path.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "'{}' --version failed (status: {}){}",
            ployzctl_path.display(),
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

fn local_ployzctl_path() -> Result<PathBuf, String> {
    #[cfg(test)]
    if let Some(path) = crate::daemon::ssh::test_ssh_env_value("PLOYZ_TEST_LOCAL_PLOYZCTL") {
        return Ok(PathBuf::from(path));
    }

    let current_exe =
        std::env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;
    let candidates = [
        current_exe.with_file_name("ployzctl"),
        current_exe
            .parent()
            .map(|parent| parent.join("ployzctl"))
            .unwrap_or_else(|| PathBuf::from("ployzctl")),
        PathBuf::from("/usr/local/bin/ployzctl"),
        PathBuf::from("/usr/bin/ployzctl"),
    ];
    for candidate in candidates {
        if Path::new(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err("ployzctl binary not found next to current daemon".into())
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
