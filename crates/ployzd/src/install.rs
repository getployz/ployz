use crate::Mode;
use ployz_sdk::{Affordances, Os, default_config_path, default_data_dir, default_socket_path};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SERVICE_LABEL: &str = "dev.ployz.ployzd";
const INSTALL_DIR_NAME: &str = "install";
const MANIFEST_FILE_NAME: &str = "manifest.env";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceBackend {
    SystemdUser,
    SystemdSystem,
    LaunchAgent,
}

impl ServiceBackend {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemdUser => "systemd-user",
            Self::SystemdSystem => "systemd-system",
            Self::LaunchAgent => "launch-agent",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "systemd-user" => Ok(Self::SystemdUser),
            "systemd-system" => Ok(Self::SystemdSystem),
            "launch-agent" => Ok(Self::LaunchAgent),
            other => Err(format!("unsupported service backend '{other}'")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallManifest {
    pub source_kind: String,
    pub source_version: Option<String>,
    pub source_git_url: Option<String>,
    pub source_git_ref: Option<String>,
    pub bin_dir: PathBuf,
    pub assets_dir: PathBuf,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub socket_path: String,
    pub installer_path: PathBuf,
    pub ployz_path: PathBuf,
    pub ployzd_path: PathBuf,
    pub gateway_path: PathBuf,
    pub dns_path: PathBuf,
    pub corrosion_path: PathBuf,
    pub requested_mode: Mode,
    pub configured_mode: Option<Mode>,
    pub service_backend: Option<ServiceBackend>,
}

#[derive(Debug, Clone)]
struct ClientPaths {
    config_path: PathBuf,
    data_dir: PathBuf,
    socket_path: String,
}

impl InstallManifest {
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = fs::read_to_string(path)
            .map_err(|error| format!("read install manifest '{}': {error}", path.display()))?;
        let mut source_kind = None;
        let mut source_version = None;
        let mut source_git_url = None;
        let mut source_git_ref = None;
        let mut bin_dir = None;
        let mut assets_dir = None;
        let mut config_path = None;
        let mut data_dir = None;
        let mut socket_path = None;
        let mut installer_path = None;
        let mut ployz_path = None;
        let mut ployzd_path = None;
        let mut gateway_path = None;
        let mut dns_path = None;
        let mut corrosion_path = None;
        let mut requested_mode = None;
        let mut configured_mode = None;
        let mut service_backend = None;

        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, raw_value)) = trimmed.split_once('=') else {
                return Err(format!(
                    "invalid install manifest line in '{}': {trimmed}",
                    path.display()
                ));
            };
            let value = parse_shell_value(raw_value)?;
            match key {
                "SOURCE_KIND" => source_kind = Some(value),
                "SOURCE_VERSION" => source_version = non_empty(value),
                "SOURCE_GIT_URL" => source_git_url = non_empty(value),
                "SOURCE_GIT_REF" => source_git_ref = non_empty(value),
                "BIN_DIR" => bin_dir = Some(PathBuf::from(value)),
                "ASSETS_DIR" => assets_dir = Some(PathBuf::from(value)),
                "CONFIG_PATH" => config_path = Some(PathBuf::from(value)),
                "DATA_DIR" => data_dir = Some(PathBuf::from(value)),
                "SOCKET_PATH" => socket_path = Some(value),
                "INSTALLER_PATH" => installer_path = Some(PathBuf::from(value)),
                "PLOYZ_PATH" => ployz_path = Some(PathBuf::from(value)),
                "PLOYZD_PATH" => ployzd_path = Some(PathBuf::from(value)),
                "PLOYZ_GATEWAY_PATH" => gateway_path = Some(PathBuf::from(value)),
                "PLOYZ_DNS_PATH" => dns_path = Some(PathBuf::from(value)),
                "CORROSION_PATH" => corrosion_path = Some(PathBuf::from(value)),
                "REQUESTED_MODE" => requested_mode = Some(parse_mode(&value)?),
                "CONFIGURED_MODE" => {
                    configured_mode = non_empty(value).map(|mode| parse_mode(&mode)).transpose()?
                }
                "SERVICE_BACKEND" => {
                    service_backend = non_empty(value)
                        .map(|backend| ServiceBackend::parse(&backend))
                        .transpose()?
                }
                _ => {}
            }
        }

        Ok(Self {
            source_kind: required_value(source_kind, "SOURCE_KIND", path)?,
            source_version,
            source_git_url,
            source_git_ref,
            bin_dir: required_value(bin_dir, "BIN_DIR", path)?,
            assets_dir: required_value(assets_dir, "ASSETS_DIR", path)?,
            config_path: required_value(config_path, "CONFIG_PATH", path)?,
            data_dir: required_value(data_dir, "DATA_DIR", path)?,
            socket_path: required_value(socket_path, "SOCKET_PATH", path)?,
            installer_path: required_value(installer_path, "INSTALLER_PATH", path)?,
            ployz_path: required_value(ployz_path, "PLOYZ_PATH", path)?,
            ployzd_path: required_value(ployzd_path, "PLOYZD_PATH", path)?,
            gateway_path: required_value(gateway_path, "PLOYZ_GATEWAY_PATH", path)?,
            dns_path: required_value(dns_path, "PLOYZ_DNS_PATH", path)?,
            corrosion_path: required_value(corrosion_path, "CORROSION_PATH", path)?,
            requested_mode: required_value(requested_mode, "REQUESTED_MODE", path)?,
            configured_mode,
            service_backend,
        })
    }

    pub fn store_to_path(&self, path: &Path) -> Result<(), String> {
        let Some(parent) = path.parent() else {
            return Err(format!(
                "invalid install manifest path '{}'",
                path.display()
            ));
        };
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create install manifest dir '{}': {error}",
                parent.display()
            )
        })?;
        let content = [
            env_line("SOURCE_KIND", &self.source_kind),
            env_line_opt("SOURCE_VERSION", self.source_version.as_deref()),
            env_line_opt("SOURCE_GIT_URL", self.source_git_url.as_deref()),
            env_line_opt("SOURCE_GIT_REF", self.source_git_ref.as_deref()),
            env_line("BIN_DIR", &self.bin_dir.display().to_string()),
            env_line("ASSETS_DIR", &self.assets_dir.display().to_string()),
            env_line("CONFIG_PATH", &self.config_path.display().to_string()),
            env_line("DATA_DIR", &self.data_dir.display().to_string()),
            env_line("SOCKET_PATH", &self.socket_path),
            env_line("INSTALLER_PATH", &self.installer_path.display().to_string()),
            env_line("PLOYZ_PATH", &self.ployz_path.display().to_string()),
            env_line("PLOYZD_PATH", &self.ployzd_path.display().to_string()),
            env_line(
                "PLOYZ_GATEWAY_PATH",
                &self.gateway_path.display().to_string(),
            ),
            env_line("PLOYZ_DNS_PATH", &self.dns_path.display().to_string()),
            env_line("CORROSION_PATH", &self.corrosion_path.display().to_string()),
            env_line("REQUESTED_MODE", mode_name(self.requested_mode)),
            env_line_opt("CONFIGURED_MODE", self.configured_mode.map(mode_name)),
            env_line_opt(
                "SERVICE_BACKEND",
                self.service_backend.map(ServiceBackend::as_str),
            ),
        ]
        .join("\n");
        fs::write(path, format!("{content}\n"))
            .map_err(|error| format!("write install manifest '{}': {error}", path.display()))
    }
}

pub fn daemon_install(mode: Mode, manifest_path: Option<&Path>) -> Result<InstallManifest, String> {
    let aff = Affordances::detect();
    let manifest_path = resolve_manifest_path(mode, manifest_path)?;
    let mut manifest = InstallManifest::load_from_path(&manifest_path)?;
    let config_target = resolve_config_target(&aff)?;
    let paths = client_paths(mode, &config_target.home_dir);

    validate_install_manifest(&manifest)?;
    write_client_config(&paths.config_path, &paths.data_dir, &paths.socket_path)?;

    match mode {
        Mode::Memory => {
            return Err("memory mode is not supported by `ployz daemon install`".into());
        }
        Mode::HostExec => {
            ensure_user_service(
                &aff,
                &manifest.ployzd_path,
                &paths.data_dir,
                &paths.socket_path,
                mode,
            )?;
            manifest.configured_mode = Some(mode);
            manifest.service_backend = Some(user_backend(&aff)?);
        }
        Mode::Docker => {
            if !aff.has_docker {
                return Err("docker mode requires a reachable Docker daemon".into());
            }
            ensure_user_service(
                &aff,
                &manifest.ployzd_path,
                &paths.data_dir,
                &paths.socket_path,
                mode,
            )?;
            manifest.configured_mode = Some(mode);
            manifest.service_backend = Some(user_backend(&aff)?);
        }
        Mode::HostService => {
            if aff.os != Os::Linux {
                return Err("host-service mode is only supported on Linux".into());
            }
            if !aff.is_root {
                return Err("host-service mode requires sudo/root".into());
            }
            promote_system_binaries(&manifest)?;
            install_system_service(&manifest.assets_dir, mode)?;
            manifest.configured_mode = Some(mode);
            manifest.service_backend = Some(ServiceBackend::SystemdSystem);
        }
    }

    manifest.config_path = paths.config_path;
    manifest.data_dir = paths.data_dir;
    manifest.socket_path = paths.socket_path;
    manifest.store_to_path(&manifest_path)?;
    Ok(manifest)
}

#[must_use]
pub fn default_manifest_path(aff: &Affordances) -> PathBuf {
    default_data_dir(aff)
        .join(INSTALL_DIR_NAME)
        .join(MANIFEST_FILE_NAME)
}

pub fn find_installer_script() -> Result<PathBuf, String> {
    let current_exe =
        std::env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let candidates = [
        current_exe.with_file_name("ployz.sh"),
        current_exe
            .parent()
            .map(|parent| parent.join("ployz.sh"))
            .unwrap_or_else(|| PathBuf::from("ployz.sh")),
        std::env::current_dir()
            .map(|dir| dir.join("ployz.sh"))
            .unwrap_or_else(|_| PathBuf::from("ployz.sh")),
        workspace_root
            .clone()
            .map(|root| root.join("ployz.sh"))
            .unwrap_or_else(|| PathBuf::from("ployz.sh")),
        PathBuf::from("/usr/local/bin/ployz.sh"),
        PathBuf::from("/usr/bin/ployz.sh"),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("ployz.sh installer script not found".into())
}

fn validate_install_manifest(manifest: &InstallManifest) -> Result<(), String> {
    let required = [
        &manifest.installer_path,
        &manifest.ployz_path,
        &manifest.ployzd_path,
        &manifest.gateway_path,
        &manifest.dns_path,
        &manifest.corrosion_path,
    ];
    for path in required {
        if !path.exists() {
            return Err(format!(
                "install manifest references missing file '{}'",
                path.display()
            ));
        }
    }
    Ok(())
}

fn ensure_user_service(
    aff: &Affordances,
    ployzd_path: &Path,
    data_dir: &Path,
    socket_path: &str,
    mode: Mode,
) -> Result<(), String> {
    match aff.os {
        Os::Linux => install_systemd_user_service(ployzd_path, data_dir, socket_path, mode),
        Os::Darwin => install_launch_agent(ployzd_path, data_dir, socket_path, mode),
        Os::Other => Err("user services are not supported on this platform".into()),
    }
}

fn user_backend(aff: &Affordances) -> Result<ServiceBackend, String> {
    match aff.os {
        Os::Linux => Ok(ServiceBackend::SystemdUser),
        Os::Darwin => Ok(ServiceBackend::LaunchAgent),
        Os::Other => Err("user services are not supported on this platform".into()),
    }
}

fn install_systemd_user_service(
    ployzd_path: &Path,
    data_dir: &Path,
    socket_path: &str,
    mode: Mode,
) -> Result<(), String> {
    let home = home_dir_for_current_user()?;
    let unit_dir = home.join(".config/systemd/user");
    fs::create_dir_all(&unit_dir)
        .map_err(|error| format!("create systemd user dir '{}': {error}", unit_dir.display()))?;
    let unit_path = unit_dir.join("ployzd.service");
    let unit = format!(
        "[Unit]\nDescription=Ployz control plane daemon\nAfter=default.target\n\n[Service]\nType=simple\nExecStart={} --data-dir {} --socket {} run --mode {}\nRestart=always\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(&ployzd_path.display().to_string()),
        systemd_quote(&data_dir.display().to_string()),
        systemd_quote(socket_path),
        mode_name(mode),
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
    mode: Mode,
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
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{SERVICE_LABEL}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string>\n    <string>--data-dir</string>\n    <string>{}</string>\n    <string>--socket</string>\n    <string>{}</string>\n    <string>run</string>\n    <string>--mode</string>\n    <string>{}</string>\n  </array>\n  <key>KeepAlive</key>\n  <true/>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n",
        xml_escape(&ployzd_path.display().to_string()),
        xml_escape(&data_dir.display().to_string()),
        xml_escape(socket_path),
        mode_name(mode),
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

fn promote_system_binaries(manifest: &InstallManifest) -> Result<(), String> {
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

fn install_system_service(assets_dir: &Path, mode: Mode) -> Result<(), String> {
    if mode != Mode::HostService {
        return Err("system service install requires host-service mode".into());
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

fn write_client_config(path: &Path, data_dir: &Path, socket_path: &str) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!("invalid config path '{}'", path.display()));
    };
    fs::create_dir_all(parent)
        .map_err(|error| format!("create config dir '{}': {error}", parent.display()))?;
    let body = format!(
        "data_dir = {}\nsocket = {}\n",
        toml_string(&data_dir.display().to_string()),
        toml_string(socket_path),
    );
    fs::write(path, body).map_err(|error| format!("write config '{}': {error}", path.display()))
}

fn resolve_manifest_path(mode: Mode, explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    let aff = Affordances::detect();
    if aff.is_root && mode == Mode::HostService
        && let Some(home) = sudo_user_home_dir()?
    {
        return Ok(linux_user_manifest_path(&home));
    }
    Ok(default_manifest_path(&aff))
}

fn resolve_config_target(aff: &Affordances) -> Result<ConfigTarget, String> {
    if aff.is_root && aff.os == Os::Linux
        && let Some(home_dir) = sudo_user_home_dir()?
    {
        return Ok(ConfigTarget { home_dir });
    }
    Ok(ConfigTarget {
        home_dir: home_dir_for_current_user()?,
    })
}

#[derive(Debug, Clone)]
struct ConfigTarget {
    home_dir: PathBuf,
}

fn client_paths(mode: Mode, home_dir: &Path) -> ClientPaths {
    let aff = Affordances::detect();
    if mode == Mode::HostService {
        return ClientPaths {
            config_path: linux_user_config_path(home_dir),
            data_dir: PathBuf::from("/var/lib/ployz"),
            socket_path: "/run/ployz/ployzd.sock".into(),
        };
    }

    if aff.is_root {
        return ClientPaths {
            config_path: default_config_path(),
            data_dir: default_data_dir(&aff),
            socket_path: default_socket_path(&aff),
        };
    }

    match aff.os {
        Os::Linux => ClientPaths {
            config_path: linux_user_config_path(home_dir),
            data_dir: linux_user_data_dir(home_dir),
            socket_path: default_socket_path(&aff),
        },
        Os::Darwin => ClientPaths {
            config_path: home_dir.join("Library/Application Support/ployz/config.toml"),
            data_dir: home_dir.join("Library/Application Support/ployz"),
            socket_path: default_socket_path(&aff),
        },
        Os::Other => ClientPaths {
            config_path: default_config_path(),
            data_dir: default_data_dir(&aff),
            socket_path: default_socket_path(&aff),
        },
    }
}

fn linux_user_manifest_path(home_dir: &Path) -> PathBuf {
    linux_user_data_dir(home_dir)
        .join(INSTALL_DIR_NAME)
        .join(MANIFEST_FILE_NAME)
}

fn linux_user_data_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(".local/share/ployz")
}

fn linux_user_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join(".config/ployz/config.toml")
}

fn home_dir_for_current_user() -> Result<PathBuf, String> {
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    Err("HOME is not set".into())
}

fn sudo_user_home_dir() -> Result<Option<PathBuf>, String> {
    let Some(user) = std::env::var_os("SUDO_USER") else {
        return Ok(None);
    };
    #[cfg(unix)]
    {
        use std::ffi::{CStr, CString};

        let username = user
            .into_string()
            .map_err(|_| "SUDO_USER was not valid UTF-8".to_string())?;
        let name = CString::new(username.clone())
            .map_err(|_| format!("invalid SUDO_USER '{username}'"))?;

        let configured_size = {
            // SAFETY: `sysconf` has no Rust-side preconditions.
            let size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
            if size > 0 { size as usize } else { 16_384 }
        };
        let mut buffer = vec![0_u8; configured_size];
        // SAFETY: `passwd` is a plain old data struct provided by libc.
        let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();

        // SAFETY: all pointers are valid for writes, `name` is a valid C string,
        // and `buffer` stays alive for the duration of the call.
        let status = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                &mut passwd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status != 0 {
            return Err(format!(
                "failed to resolve home directory for '{username}': errno {status}"
            ));
        }
        if result.is_null() {
            return Err(format!("failed to resolve home directory for '{username}'"));
        }
        let home_ptr = passwd.pw_dir;
        if home_ptr.is_null() {
            return Err(format!("home directory missing for '{username}'"));
        }
        // SAFETY: `pw_dir` points into `buffer`, which is still alive here, and is
        // NUL-terminated by libc on success.
        let home = unsafe { CStr::from_ptr(home_ptr) };
        Ok(Some(PathBuf::from(home.to_string_lossy().into_owned())))
    }
    #[cfg(not(unix))]
    {
        let _ = user;
        Ok(None)
    }
}

fn run_command<const N: usize>(program: &str, args: [&str; N]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("start {program}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    Err(format!("{program} {} failed: {detail}", args.join(" "),))
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Memory => "memory",
        Mode::Docker => "docker",
        Mode::HostExec => "host-exec",
        Mode::HostService => "host-service",
    }
}

fn parse_mode(value: &str) -> Result<Mode, String> {
    match value {
        "memory" => Ok(Mode::Memory),
        "docker" => Ok(Mode::Docker),
        "host-exec" => Ok(Mode::HostExec),
        "host-service" => Ok(Mode::HostService),
        other => Err(format!("unsupported runtime mode '{other}'")),
    }
}

fn env_line(key: &str, value: &str) -> String {
    format!("{key}={}", single_quote(value))
}

fn env_line_opt(key: &str, value: Option<&str>) -> String {
    env_line(key, value.unwrap_or(""))
}

fn single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn parse_shell_value(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        let inner = &trimmed[1..trimmed.len() - 1];
        return Ok(inner.replace("'\"'\"'", "'"));
    }
    if trimmed.contains(' ') {
        return Err(format!("unquoted install manifest value '{trimmed}'"));
    }
    Ok(trimmed.to_string())
}

fn required_value<T>(value: Option<T>, key: &str, path: &Path) -> Result<T, String> {
    value.ok_or_else(|| format!("missing {key} in install manifest '{}'", path.display()))
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

fn toml_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

fn systemd_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn set_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("stat '{}': {error}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("chmod '{}': {error}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn nix_like_uid() -> Result<u32, String> {
    #[cfg(unix)]
    {
        // SAFETY: simple libc call with no preconditions.
        Ok(unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        Err("launchd user services require a unix-like system".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trip_preserves_fields() {
        let manifest = InstallManifest {
            source_kind: "payload".into(),
            source_version: Some("v1.2.3".into()),
            source_git_url: None,
            source_git_ref: None,
            bin_dir: PathBuf::from("/tmp/bin dir"),
            assets_dir: PathBuf::from("/tmp/assets"),
            config_path: PathBuf::from("/tmp/config.toml"),
            data_dir: PathBuf::from("/tmp/data"),
            socket_path: "/tmp/socket.sock".into(),
            installer_path: PathBuf::from("/tmp/bin/ployz.sh"),
            ployz_path: PathBuf::from("/tmp/bin/ployz"),
            ployzd_path: PathBuf::from("/tmp/bin/ployzd"),
            gateway_path: PathBuf::from("/tmp/bin/ployz-gateway"),
            dns_path: PathBuf::from("/tmp/bin/ployz-dns"),
            corrosion_path: PathBuf::from("/tmp/bin/corrosion"),
            requested_mode: Mode::HostExec,
            configured_mode: Some(Mode::HostService),
            service_backend: Some(ServiceBackend::SystemdSystem),
        };

        let path =
            std::env::temp_dir().join(format!("ployz-install-manifest-{}.env", std::process::id()));
        manifest.store_to_path(&path).expect("store manifest");
        let loaded = InstallManifest::load_from_path(&path).expect("load manifest");
        assert_eq!(loaded.source_kind, "payload");
        assert_eq!(loaded.source_version.as_deref(), Some("v1.2.3"));
        assert_eq!(loaded.bin_dir, PathBuf::from("/tmp/bin dir"));
        assert_eq!(loaded.requested_mode, Mode::HostExec);
        assert_eq!(loaded.configured_mode, Some(Mode::HostService));
        assert_eq!(loaded.service_backend, Some(ServiceBackend::SystemdSystem));
        fs::remove_file(path).expect("remove temp manifest");
    }
}
