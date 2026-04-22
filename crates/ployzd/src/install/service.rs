use std::fs;
use std::path::{Path, PathBuf};

use ployz_config::{Os, RuntimeTarget, ServiceMode};

use crate::install::{InstallManifest, SERVICE_LABEL, ServiceBackend};
use crate::platform::HostPlatform;

use super::manifest::{runtime_target_name, service_mode_name};
use super::render::{systemd_quote, xml_escape};
use super::sys::{home_dir_for_current_user, nix_like_uid, run_command, set_executable};

pub(super) fn ensure_user_service(
    platform: HostPlatform,
    ployzd_path: &Path,
    data_dir: &Path,
    socket_path: &str,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
) -> Result<(), String> {
    match platform.os {
        Os::Linux => install_systemd_user_service(
            ployzd_path,
            data_dir,
            socket_path,
            runtime_target,
            service_mode,
        ),
        Os::Darwin => install_launch_agent(
            ployzd_path,
            data_dir,
            socket_path,
            runtime_target,
            service_mode,
        ),
        Os::Other => Err("user services are not supported on this platform".into()),
    }
}

pub(super) fn user_backend(platform: HostPlatform) -> Result<ServiceBackend, String> {
    match platform.os {
        Os::Linux => Ok(ServiceBackend::SystemdUser),
        Os::Darwin => Ok(ServiceBackend::LaunchAgent),
        Os::Other => Err("user services are not supported on this platform".into()),
    }
}

fn install_systemd_user_service(
    ployzd_path: &Path,
    data_dir: &Path,
    socket_path: &str,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
) -> Result<(), String> {
    let home = home_dir_for_current_user()?;
    let unit_dir = home.join(".config/systemd/user");
    fs::create_dir_all(&unit_dir)
        .map_err(|error| format!("create systemd user dir '{}': {error}", unit_dir.display()))?;
    let unit_path = unit_dir.join("ployzd.service");
    let unit = format!(
        "[Unit]\nDescription=Ployz control plane daemon\nAfter=default.target\n\n[Service]\nType=simple\nExecStart={} --data-dir {} --socket {} run --runtime {} --service-mode {}\nRestart=always\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(&ployzd_path.display().to_string()),
        systemd_quote(&data_dir.display().to_string()),
        systemd_quote(socket_path),
        runtime_target_name(runtime_target),
        service_mode_name(service_mode),
    );
    fs::write(&unit_path, unit)
        .map_err(|error| format!("write systemd user unit '{}': {error}", unit_path.display()))?;
    run_command("systemctl", ["--user", "daemon-reload"])?;
    run_command("systemctl", ["--user", "enable", "--now", "ployzd.service"])?;
    Ok(())
}

fn install_launch_agent(
    ployzd_path: &Path,
    data_dir: &Path,
    socket_path: &str,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
) -> Result<(), String> {
    let home = home_dir_for_current_user()?;
    let agents_dir = home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents_dir).map_err(|error| {
        format!(
            "create LaunchAgents dir '{}': {error}",
            agents_dir.display()
        )
    })?;
    let plist_path = agents_dir.join(format!("{SERVICE_LABEL}.plist"));
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{SERVICE_LABEL}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string>\n    <string>--data-dir</string>\n    <string>{}</string>\n    <string>--socket</string>\n    <string>{}</string>\n    <string>run</string>\n    <string>--runtime</string>\n    <string>{}</string>\n    <string>--service-mode</string>\n    <string>{}</string>\n  </array>\n  <key>KeepAlive</key>\n  <true/>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n",
        xml_escape(&ployzd_path.display().to_string()),
        xml_escape(&data_dir.display().to_string()),
        xml_escape(socket_path),
        runtime_target_name(runtime_target),
        service_mode_name(service_mode),
    );
    fs::write(&plist_path, plist)
        .map_err(|error| format!("write LaunchAgent '{}': {error}", plist_path.display()))?;
    let uid = nix_like_uid()?;
    let domain = format!("gui/{uid}");
    let plist_str = plist_path.display().to_string();
    let _ = run_command(
        "launchctl",
        ["bootout", domain.as_str(), plist_str.as_str()],
    );
    run_command(
        "launchctl",
        ["bootstrap", domain.as_str(), plist_str.as_str()],
    )?;
    run_command(
        "launchctl",
        [
            "kickstart",
            "-k",
            format!("{domain}/{SERVICE_LABEL}").as_str(),
        ],
    )?;
    Ok(())
}

pub(super) fn promote_system_binaries(manifest: &InstallManifest) -> Result<(), String> {
    let system_bin_dir = PathBuf::from("/usr/local/bin");
    fs::create_dir_all(&system_bin_dir).map_err(|error| {
        format!(
            "create system bin dir '{}': {error}",
            system_bin_dir.display()
        )
    })?;
    let copies = [
        (&manifest.installer_path, system_bin_dir.join("ployz.sh")),
        (&manifest.ployz_path, system_bin_dir.join("ployz")),
        (&manifest.ployzd_path, system_bin_dir.join("ployzd")),
        (&manifest.gateway_path, system_bin_dir.join("ployz-gateway")),
        (&manifest.dns_path, system_bin_dir.join("ployz-dns")),
        (&manifest.corrosion_path, system_bin_dir.join("corrosion")),
    ];
    for (src, dest) in copies {
        let Some(file_name) = dest.file_name() else {
            return Err(format!("invalid system binary path '{}'", dest.display()));
        };
        let temp_dest = system_bin_dir.join(format!(
            ".{}.tmp-{}",
            file_name.to_string_lossy(),
            std::process::id()
        ));
        fs::copy(src, &temp_dest).map_err(|error| {
            format!(
                "copy '{}' to temporary '{}': {error}",
                src.display(),
                temp_dest.display()
            )
        })?;
        set_executable(&temp_dest)?;
        fs::rename(&temp_dest, &dest).map_err(|error| {
            let _ = fs::remove_file(&temp_dest);
            format!(
                "rename temporary '{}' to '{}': {error}",
                temp_dest.display(),
                dest.display()
            )
        })?;
    }
    Ok(())
}

pub(super) fn install_system_service(
    assets_dir: &Path,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
) -> Result<(), String> {
    if runtime_target != RuntimeTarget::Host || service_mode != ServiceMode::System {
        return Err("system service install requires host runtime with system service mode".into());
    }
    let source_unit = assets_dir.join("systemd/ployzd.service");
    let unit_path = PathBuf::from("/etc/systemd/system/ployzd.service");
    let Some(parent) = unit_path.parent() else {
        return Err(format!(
            "invalid systemd unit path '{}'",
            unit_path.display()
        ));
    };
    fs::create_dir_all(parent)
        .map_err(|error| format!("create systemd dir '{}': {error}", parent.display()))?;
    fs::copy(&source_unit, &unit_path).map_err(|error| {
        format!(
            "copy systemd unit '{}' to '{}': {error}",
            source_unit.display(),
            unit_path.display()
        )
    })?;
    run_command("systemctl", ["daemon-reload"])?;
    run_command("systemctl", ["enable", "--now", "ployzd.service"])?;
    Ok(())
}
