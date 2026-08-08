//! Installed identity and filesystem-access contract for privileged role setup.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use ployz_core::operation::FailureMessage;
use ployz_core::roles::PloyzdRole;

use super::{
    FileMode, HostPackageFamily, HostRunnerCommandRunner, PloyzdArtifactStore,
    PloyzdRoleEnvironmentFile, SupervisorBackend, SupervisorDirectories, SupervisorKind,
    SupervisorUnitSpec, detect_host_platform, write_durable_file,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixUser {
    Root,
    PloyzApi,
    PloyzGateway,
    Unrelated,
}

impl UnixUser {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::PloyzApi => "ployz-api",
            Self::PloyzGateway => "ployz-gateway",
            Self::Unrelated => "nobody",
        }
    }
}

impl fmt::Display for UnixUser {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixGroup {
    Root,
    PloyzApi,
    PloyzGateway,
    PloyzControl,
    Docker,
    Unrelated,
}

impl UnixGroup {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::PloyzApi => "ployz-api",
            Self::PloyzGateway => "ployz-gateway",
            Self::PloyzControl => "ployz-control",
            Self::Docker => "docker",
            Self::Unrelated => "nogroup",
        }
    }
}

impl fmt::Display for UnixGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub const API_USER: UnixUser = UnixUser::PloyzApi;
pub const API_GROUP: UnixGroup = UnixGroup::PloyzApi;
pub const CONTROL_GROUP: UnixGroup = UnixGroup::PloyzControl;
pub const DOCKER_GROUP: UnixGroup = UnixGroup::Docker;

pub const API_EVIDENCE_DIRECTORY: &str = "api/evidence";
pub const API_LEASE_DIRECTORY: &str = "api/lease";
/// Verified candidates awaiting Keeper adoption into the root-owned live artifact store.
pub const API_UPGRADE_STAGING_DIRECTORY: &str = "api/upgrade-staging";
pub const UPGRADE_RUNTIME_DIRECTORY: &str = "/run/ployz";
pub const UPGRADE_SOCKET_PATH: &str = "/run/ployz/keeper-upgrade.sock";
pub const CONTROL_RUNTIME_DIRECTORY: &str = "/run/ployz-control";
pub const CONTROL_SOCKET_PATH: &str = "/run/ployz-control/keeper-control.sock";

const API_READABLE_STATE_FILES: &[&str] = &[
    "door.key",
    "door.crt",
    "door.fingerprint",
    "join-substrate.json",
];
const API_SUPPLEMENTARY_GROUPS: &[UnixGroup] = &[UnixGroup::Docker, UnixGroup::PloyzControl];

/// Installed process identity shared by both supervisor renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstalledRolePrivilege {
    user: UnixUser,
    primary_group: UnixGroup,
    supplementary_groups: &'static [UnixGroup],
    no_new_privileges: bool,
}

impl InstalledRolePrivilege {
    #[must_use]
    pub const fn for_role(role: PloyzdRole) -> Option<Self> {
        match role {
            PloyzdRole::Keeper => Some(Self {
                user: UnixUser::Root,
                primary_group: UnixGroup::PloyzControl,
                supplementary_groups: &[],
                no_new_privileges: false,
            }),
            PloyzdRole::Api => Some(Self {
                user: UnixUser::PloyzApi,
                primary_group: UnixGroup::PloyzApi,
                supplementary_groups: API_SUPPLEMENTARY_GROUPS,
                no_new_privileges: true,
            }),
            PloyzdRole::Gateway => Some(Self {
                user: UnixUser::PloyzGateway,
                primary_group: UnixGroup::PloyzGateway,
                supplementary_groups: &[],
                no_new_privileges: true,
            }),
            PloyzdRole::Dns => None,
        }
    }

    #[must_use]
    pub const fn user(self) -> UnixUser {
        self.user
    }

    #[must_use]
    pub const fn primary_group(self) -> UnixGroup {
        self.primary_group
    }

    #[must_use]
    pub const fn supplementary_groups(self) -> &'static [UnixGroup] {
        self.supplementary_groups
    }

    #[must_use]
    pub const fn no_new_privileges(self) -> bool {
        self.no_new_privileges
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiRuntimeAccess {
    TraverseDirectory,
    ReadFile,
    ReadWriteDirectory,
    ReadWriteSocket,
}

impl ApiRuntimeAccess {
    const fn required_bits(self) -> u16 {
        match self {
            Self::TraverseDirectory => 0o1,
            Self::ReadFile => 0o4,
            Self::ReadWriteDirectory => 0o3,
            Self::ReadWriteSocket => 0o6,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledIdentity {
    user: UnixUser,
    primary_group: UnixGroup,
    supplementary_groups: Vec<UnixGroup>,
}

impl InstalledIdentity {
    #[must_use]
    pub fn new(
        user: UnixUser,
        primary_group: UnixGroup,
        supplementary_groups: impl IntoIterator<Item = UnixGroup>,
    ) -> Self {
        Self {
            user,
            primary_group,
            supplementary_groups: supplementary_groups.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn api() -> Self {
        Self::new(
            API_USER,
            API_GROUP,
            API_SUPPLEMENTARY_GROUPS.iter().copied(),
        )
    }

    #[must_use]
    pub fn unrelated() -> Self {
        Self::new(UnixUser::Unrelated, UnixGroup::Unrelated, [])
    }

    #[must_use]
    pub const fn user(&self) -> UnixUser {
        self.user
    }

    #[must_use]
    pub const fn primary_group(&self) -> UnixGroup {
        self.primary_group
    }

    #[must_use]
    pub fn supplementary_groups(&self) -> &[UnixGroup] {
        &self.supplementary_groups
    }

    fn belongs_to(&self, group: UnixGroup) -> bool {
        self.primary_group == group || self.supplementary_groups.contains(&group)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallDisposition {
    EnsureDirectory,
    EnsureReadOnlyFile,
    ExistingSocket,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRuntimeAccessEntry {
    path: PathBuf,
    owner: UnixUser,
    group: UnixGroup,
    mode: u16,
    access: ApiRuntimeAccess,
    install: InstallDisposition,
}

impl ApiRuntimeAccessEntry {
    fn new(
        path: impl Into<PathBuf>,
        owner: UnixUser,
        group: UnixGroup,
        mode: u16,
        access: ApiRuntimeAccess,
        install: InstallDisposition,
    ) -> Self {
        Self {
            path: path.into(),
            owner,
            group,
            mode,
            access,
            install,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn owner(&self) -> UnixUser {
        self.owner
    }

    #[must_use]
    pub const fn group(&self) -> UnixGroup {
        self.group
    }

    #[must_use]
    pub const fn mode(&self) -> u16 {
        self.mode
    }

    #[must_use]
    pub fn allows(&self, identity: &InstalledIdentity) -> bool {
        let bits = if identity.user == self.owner {
            (self.mode >> 6) & 0o7
        } else if identity.belongs_to(self.group) {
            (self.mode >> 3) & 0o7
        } else {
            self.mode & 0o7
        };
        bits & self.access.required_bits() == self.access.required_bits()
    }

    fn install_commands(&self) -> Result<Vec<PrivilegeInstallCommand>, FailureMessage> {
        let path = self.path.to_str().ok_or_else(|| {
            failure(format!(
                "API runtime path {} is not UTF-8",
                self.path.display()
            ))
        })?;
        let owner = self.owner.to_string();
        let group = self.group.to_string();
        let mode = format!("{:04o}", self.mode);
        let commands = match self.install {
            InstallDisposition::EnsureDirectory => vec![PrivilegeInstallCommand::new(
                "install",
                ["-d", "-o", &owner, "-g", &group, "-m", &mode, path],
            )],
            InstallDisposition::EnsureReadOnlyFile => vec![
                PrivilegeInstallCommand::new(
                    "chown",
                    [format!("{owner}:{group}"), path.to_owned()],
                ),
                PrivilegeInstallCommand::new("chmod", [mode, path.to_owned()]),
            ],
            InstallDisposition::ExistingSocket | InstallDisposition::External => Vec::new(),
        };
        Ok(commands)
    }

    fn existing_socket_migration_commands(
        &self,
    ) -> Result<Vec<PrivilegeInstallCommand>, FailureMessage> {
        let mut commands = Vec::new();
        if self.install == InstallDisposition::ExistingSocket {
            let path = shell_single_quote(&self.path)?;
            let owner = self.owner.to_string();
            let group = self.group.to_string();
            let mode = format!("{:04o}", self.mode);
            commands.push(shell_owned(format!(
                "test ! -S {path} || {{ chown {owner}:{group} {path} && chmod {mode} {path}; }}"
            )));
        }
        Ok(commands)
    }

    fn verification_command(&self) -> Result<PrivilegeInstallCommand, FailureMessage> {
        let path = shell_single_quote(&self.path)?;
        let kind = match self.install {
            InstallDisposition::EnsureDirectory => "-d",
            InstallDisposition::EnsureReadOnlyFile => "-f",
            InstallDisposition::ExistingSocket | InstallDisposition::External => "-S",
        };
        Ok(shell_owned(format!(
            "test {kind} {path} && test \"$(stat -c '%U:%G %a' {path})\" = '{}:{} {:o}'",
            self.owner, self.group, self.mode,
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRuntimeAccessMatrix {
    entries: Vec<ApiRuntimeAccessEntry>,
}

impl ApiRuntimeAccessMatrix {
    #[must_use]
    pub fn for_state(state: &Path) -> Self {
        let mut entries = vec![ApiRuntimeAccessEntry::new(
            state,
            UnixUser::Root,
            UnixGroup::PloyzApi,
            0o710,
            ApiRuntimeAccess::TraverseDirectory,
            InstallDisposition::EnsureDirectory,
        )];
        entries.extend(API_READABLE_STATE_FILES.iter().map(|file| {
            ApiRuntimeAccessEntry::new(
                state.join(file),
                UnixUser::Root,
                UnixGroup::PloyzApi,
                0o640,
                ApiRuntimeAccess::ReadFile,
                InstallDisposition::EnsureReadOnlyFile,
            )
        }));
        entries.extend([
            ApiRuntimeAccessEntry::new(
                state.join(API_EVIDENCE_DIRECTORY),
                UnixUser::PloyzApi,
                UnixGroup::PloyzApi,
                0o750,
                ApiRuntimeAccess::ReadWriteDirectory,
                InstallDisposition::EnsureDirectory,
            ),
            ApiRuntimeAccessEntry::new(
                state.join(API_LEASE_DIRECTORY),
                UnixUser::PloyzApi,
                UnixGroup::PloyzApi,
                0o700,
                ApiRuntimeAccess::ReadWriteDirectory,
                InstallDisposition::EnsureDirectory,
            ),
            ApiRuntimeAccessEntry::new(
                state.join(API_UPGRADE_STAGING_DIRECTORY),
                UnixUser::PloyzApi,
                UnixGroup::PloyzControl,
                0o770,
                ApiRuntimeAccess::ReadWriteDirectory,
                InstallDisposition::EnsureDirectory,
            ),
            ApiRuntimeAccessEntry::new(
                "/var/run/docker.sock",
                UnixUser::Root,
                UnixGroup::Docker,
                0o660,
                ApiRuntimeAccess::ReadWriteSocket,
                InstallDisposition::External,
            ),
            ApiRuntimeAccessEntry::new(
                UPGRADE_RUNTIME_DIRECTORY,
                UnixUser::Root,
                UnixGroup::PloyzControl,
                0o750,
                ApiRuntimeAccess::TraverseDirectory,
                InstallDisposition::EnsureDirectory,
            ),
            ApiRuntimeAccessEntry::new(
                UPGRADE_SOCKET_PATH,
                UnixUser::Root,
                UnixGroup::PloyzControl,
                0o660,
                ApiRuntimeAccess::ReadWriteSocket,
                InstallDisposition::ExistingSocket,
            ),
            ApiRuntimeAccessEntry::new(
                CONTROL_RUNTIME_DIRECTORY,
                UnixUser::Root,
                UnixGroup::PloyzControl,
                0o750,
                ApiRuntimeAccess::TraverseDirectory,
                InstallDisposition::EnsureDirectory,
            ),
            ApiRuntimeAccessEntry::new(
                CONTROL_SOCKET_PATH,
                UnixUser::Root,
                UnixGroup::PloyzControl,
                0o660,
                ApiRuntimeAccess::ReadWriteSocket,
                InstallDisposition::ExistingSocket,
            ),
        ]);
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[ApiRuntimeAccessEntry] {
        &self.entries
    }

    fn install_commands(&self) -> Result<Vec<PrivilegeInstallCommand>, FailureMessage> {
        let mut commands = Vec::new();
        for entry in &self.entries {
            commands.extend(entry.install_commands()?);
        }
        Ok(commands)
    }

    fn existing_socket_migration_commands(
        &self,
    ) -> Result<Vec<PrivilegeInstallCommand>, FailureMessage> {
        let mut commands = Vec::new();
        for entry in &self.entries {
            commands.extend(entry.existing_socket_migration_commands()?);
        }
        Ok(commands)
    }

    fn verification_commands(&self) -> Result<Vec<PrivilegeInstallCommand>, FailureMessage> {
        self.entries
            .iter()
            .map(ApiRuntimeAccessEntry::verification_command)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegeInstallCommand {
    program: &'static str,
    args: Vec<String>,
}

impl PrivilegeInstallCommand {
    fn new(program: &'static str, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            program,
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    #[must_use]
    pub const fn program(&self) -> &'static str {
        self.program
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

impl fmt::Display for PrivilegeInstallCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.program, self.args.join(" "))
    }
}

pub fn installed_role_privilege_commands(
    state: &Path,
    package_family: HostPackageFamily,
) -> Result<Vec<PrivilegeInstallCommand>, FailureMessage> {
    let mut commands = account_commands(package_family);
    commands.extend(ApiRuntimeAccessMatrix::for_state(state).install_commands()?);
    Ok(commands)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiPrivilegeMigrationOutcome {
    rewritten_units: usize,
}

impl ApiPrivilegeMigrationOutcome {
    #[must_use]
    pub const fn rewritten_units(self) -> usize {
        self.rewritten_units
    }
}

/// Converges a root-running systemd installation to the installed API
/// privilege contract before any non-Keeper role is restarted.
pub fn migrate_existing_systemd_api_privileges(
    state: &Path,
    directories: &SupervisorDirectories,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<ApiPrivilegeMigrationOutcome, FailureMessage> {
    let os_release = runner.read_os_release()?;
    let profile = detect_host_platform(&os_release).map_err(failure)?;
    if profile.supervisor() != SupervisorKind::Systemd {
        return Err(failure(
            "existing-host API privilege migration requires systemd",
        ));
    }
    let matrix = ApiRuntimeAccessMatrix::for_state(state);
    let artifact_store = PloyzdArtifactStore::new(state.to_path_buf()).map_err(failure)?;
    let environment = PloyzdRoleEnvironmentFile::new(state.join("ployzd.env")).map_err(failure)?;
    let gateway_environment = gateway_environment_from_shared(&environment)?;
    write_durable_file(
        state,
        "ployz-gateway.env",
        FileMode::Secret0600,
        &gateway_environment,
    )?;
    let systemd_directory = directories.directory(SupervisorBackend::Systemd);
    let mut expected_units = Vec::new();
    for role in [
        PloyzdRole::Keeper,
        PloyzdRole::Api,
        PloyzdRole::Gateway,
        PloyzdRole::Dns,
    ] {
        let role_environment = match role {
            PloyzdRole::Gateway => {
                PloyzdRoleEnvironmentFile::new(state.join("ployz-gateway.env")).map_err(failure)?
            }
            PloyzdRole::Keeper | PloyzdRole::Api | PloyzdRole::Dns => environment.clone(),
        };
        let spec = SupervisorUnitSpec::PloyzdRole {
            role,
            artifact_store: artifact_store.clone(),
            environment_file: role_environment,
        };
        let rendered = SupervisorBackend::Systemd.render(&spec).map_err(failure)?;
        let path = systemd_directory.join(rendered.file_name());
        expected_units.push((
            path,
            rendered.file_name().to_owned(),
            rendered.contents().as_bytes().to_vec(),
        ));
    }
    let units_current = expected_units
        .iter()
        .all(|(path, _, expected)| fs::read(path).is_ok_and(|installed| installed == *expected));
    if units_current {
        let mut verification = account_verification_commands(profile.package_family());
        verification.extend(matrix.verification_commands()?);
        if commands_succeed(runner, &verification)? {
            return Ok(ApiPrivilegeMigrationOutcome { rewritten_units: 0 });
        }
    }

    let mut commands = installed_role_privilege_commands(state, profile.package_family())?;
    commands.extend(matrix.existing_socket_migration_commands()?);
    run_commands(runner, &commands)?;

    let mut rewritten_units = 0;
    for (path, file_name, contents) in &expected_units {
        let already_current = fs::read(path).is_ok_and(|installed| installed == *contents);
        if !already_current {
            write_durable_file(
                systemd_directory,
                file_name,
                FileMode::Executable0755,
                contents,
            )?;
            rewritten_units += 1;
        }
    }

    run_commands(
        runner,
        &[PrivilegeInstallCommand::new("systemctl", ["daemon-reload"])],
    )?;
    run_commands(
        runner,
        &account_verification_commands(profile.package_family()),
    )?;
    run_commands(runner, &matrix.verification_commands()?)?;
    for (path, _, expected) in expected_units {
        let installed = fs::read(&path).map_err(|error| {
            failure(format!(
                "failed to verify migrated systemd unit {}: {error}",
                path.display()
            ))
        })?;
        if installed != expected {
            return Err(failure(format!(
                "migrated systemd unit {} did not retain its rendered contract",
                path.display()
            )));
        }
    }
    Ok(ApiPrivilegeMigrationOutcome { rewritten_units })
}

fn commands_succeed(
    runner: &mut impl HostRunnerCommandRunner,
    commands: &[PrivilegeInstallCommand],
) -> Result<bool, FailureMessage> {
    for command in commands {
        let args = command
            .args()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if !runner.command(command.program(), &args)?.success {
            return Ok(false);
        }
    }
    Ok(true)
}

fn run_commands(
    runner: &mut impl HostRunnerCommandRunner,
    commands: &[PrivilegeInstallCommand],
) -> Result<(), FailureMessage> {
    for command in commands {
        let args = command
            .args()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let output = runner.command(command.program(), &args)?;
        if !output.success {
            return Err(failure(format!(
                "API privilege migration command `{command}` failed: {}",
                output.failure
            )));
        }
    }
    Ok(())
}

fn account_commands(package_family: HostPackageFamily) -> Vec<PrivilegeInstallCommand> {
    match package_family {
        HostPackageFamily::Alpine => vec![
            shell("getent group ployz-api >/dev/null || addgroup -S ployz-api"),
            shell("getent group ployz-gateway >/dev/null || addgroup -S ployz-gateway"),
            shell("getent group ployz-control >/dev/null || addgroup -S ployz-control"),
            shell(
                "getent passwd ployz-api >/dev/null || adduser -S -D -H -h /nonexistent -s /sbin/nologin -G ployz-api ployz-api",
            ),
            shell(
                "getent passwd ployz-gateway >/dev/null || adduser -S -D -H -h /nonexistent -s /sbin/nologin -G ployz-gateway ployz-gateway",
            ),
            shell(
                "id -nG ployz-api | tr ' ' '\\n' | grep -Fxq docker || addgroup ployz-api docker",
            ),
            shell(
                "id -nG ployz-api | tr ' ' '\\n' | grep -Fxq ployz-control || addgroup ployz-api ployz-control",
            ),
            PrivilegeInstallCommand::new("passwd", ["-l", API_USER.as_str()]),
            PrivilegeInstallCommand::new("passwd", ["-l", "ployz-gateway"]),
            shell("test \"$(id -gn ployz-api)\" = ployz-api"),
            shell(
                "getent passwd ployz-api | awk -F: '$1 == \"ployz-api\" && $6 == \"/nonexistent\" && $7 == \"/sbin/nologin\" { found = 1 } END { exit !found }'",
            ),
            shell(
                "getent passwd ployz-gateway | awk -F: '$1 == \"ployz-gateway\" && $6 == \"/nonexistent\" && $7 == \"/sbin/nologin\" { found = 1 } END { exit !found }'",
            ),
            verify_supplementary_groups(),
        ],
        HostPackageFamily::Debian
        | HostPackageFamily::Rpm
        | HostPackageFamily::Arch
        | HostPackageFamily::Suse => vec![
            shell("getent group ployz-api >/dev/null || groupadd --system ployz-api"),
            shell("getent group ployz-gateway >/dev/null || groupadd --system ployz-gateway"),
            shell("getent group ployz-control >/dev/null || groupadd --system ployz-control"),
            shell(
                "getent passwd ployz-api >/dev/null || useradd --system --gid ployz-api --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin ployz-api",
            ),
            shell(
                "getent passwd ployz-gateway >/dev/null || useradd --system --gid ployz-gateway --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin ployz-gateway",
            ),
            PrivilegeInstallCommand::new(
                "usermod",
                [
                    "--gid",
                    API_GROUP.as_str(),
                    "--home",
                    "/nonexistent",
                    "--shell",
                    "/usr/sbin/nologin",
                    API_USER.as_str(),
                ],
            ),
            PrivilegeInstallCommand::new(
                "usermod",
                ["--groups", "docker,ployz-control", API_USER.as_str()],
            ),
            PrivilegeInstallCommand::new("usermod", ["--lock", API_USER.as_str()]),
            PrivilegeInstallCommand::new(
                "usermod",
                [
                    "--gid",
                    "ployz-gateway",
                    "--home",
                    "/nonexistent",
                    "--shell",
                    "/usr/sbin/nologin",
                    "ployz-gateway",
                ],
            ),
            PrivilegeInstallCommand::new("usermod", ["--lock", "ployz-gateway"]),
            verify_supplementary_groups(),
        ],
    }
}

fn account_verification_commands(
    package_family: HostPackageFamily,
) -> Vec<PrivilegeInstallCommand> {
    let shell_path = match package_family {
        HostPackageFamily::Alpine => "/sbin/nologin",
        HostPackageFamily::Debian
        | HostPackageFamily::Rpm
        | HostPackageFamily::Arch
        | HostPackageFamily::Suse => "/usr/sbin/nologin",
    };
    vec![
        shell_owned(format!(
            "api_gid=\"$(getent group ployz-api | cut -d: -f3)\" && test -n \"$api_gid\" && getent passwd ployz-api | awk -F: -v gid=\"$api_gid\" -v shell=\"{shell_path}\" '$1 == \"ployz-api\" && $4 == gid && $6 == \"/nonexistent\" && $7 == shell {{ found = 1 }} END {{ exit !found }}'"
        )),
        shell("getent group ployz-control >/dev/null"),
        verify_supplementary_groups(),
        shell(
            "passwd -S ployz-api | awk '$1 == \"ployz-api\" && ($2 == \"L\" || $2 == \"LK\") { found = 1 } END { exit !found }'",
        ),
        shell_owned(format!(
            "gateway_gid=\"$(getent group ployz-gateway | cut -d: -f3)\" && test -n \"$gateway_gid\" && getent passwd ployz-gateway | awk -F: -v gid=\"$gateway_gid\" -v shell=\"{shell_path}\" '$1 == \"ployz-gateway\" && $4 == gid && $6 == \"/nonexistent\" && $7 == shell {{ found = 1 }} END {{ exit !found }}'"
        )),
        shell(
            "passwd -S ployz-gateway | awk '$1 == \"ployz-gateway\" && ($2 == \"L\" || $2 == \"LK\") { found = 1 } END { exit !found }'",
        ),
    ]
}

pub fn gateway_environment_contents(
    corrosion_api_addr: &str,
    corrosion_bearer_token: &str,
    cluster_id: &str,
    machine_id: &str,
) -> Result<Vec<u8>, FailureMessage> {
    for (name, value) in [
        ("PLOYZ_CORROSION_API_ADDR", corrosion_api_addr),
        ("PLOYZ_CORROSION_BEARER_TOKEN", corrosion_bearer_token),
        ("PLOYZ_CLUSTER_ID", cluster_id),
        ("PLOYZ_MACHINE_ID", machine_id),
    ] {
        if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(failure(format!(
                "{name} cannot be rendered into the gateway environment"
            )));
        }
    }
    Ok(format!(
        "PLOYZ_CORROSION_API_ADDR={corrosion_api_addr}\nPLOYZ_CORROSION_BEARER_TOKEN={corrosion_bearer_token}\nPLOYZ_CLUSTER_ID={cluster_id}\nPLOYZ_MACHINE_ID={machine_id}\nPLOYZ_GATEWAY_LISTEN_ADDR=0.0.0.0:80\n"
    )
    .into_bytes())
}

fn gateway_environment_from_shared(
    environment: &PloyzdRoleEnvironmentFile,
) -> Result<Vec<u8>, FailureMessage> {
    let contents = fs::read_to_string(environment.path()).map_err(|error| {
        failure(format!(
            "failed to read existing ployzd environment {}: {error}",
            environment.path().display()
        ))
    })?;
    let mut api_addr = None;
    let mut token = None;
    let mut cluster_id = None;
    let mut machine_id = None;
    for line in contents.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let slot = match name {
            "PLOYZ_CORROSION_API_ADDR" => &mut api_addr,
            "PLOYZ_CORROSION_BEARER_TOKEN" => &mut token,
            "PLOYZ_CLUSTER_ID" => &mut cluster_id,
            "PLOYZ_MACHINE_ID" => &mut machine_id,
            _ => continue,
        };
        if slot.replace(value).is_some() {
            return Err(failure(format!(
                "existing ployzd environment contains duplicate {name}"
            )));
        }
    }
    let (Some(api_addr), Some(token), Some(cluster_id), Some(machine_id)) =
        (api_addr, token, cluster_id, machine_id)
    else {
        return Err(failure(
            "existing ployzd environment lacks gateway Corrosion, cluster, or machine settings",
        ));
    };
    gateway_environment_contents(api_addr, token, cluster_id, machine_id)
}

fn shell(script: &'static str) -> PrivilegeInstallCommand {
    PrivilegeInstallCommand::new("sh", ["-c", script])
}

fn shell_owned(script: String) -> PrivilegeInstallCommand {
    PrivilegeInstallCommand::new("sh", ["-c".to_owned(), script])
}

fn shell_single_quote(path: &Path) -> Result<String, FailureMessage> {
    let path = path
        .to_str()
        .ok_or_else(|| failure(format!("API runtime path {} is not UTF-8", path.display())))?;
    Ok(format!("'{}'", path.replace('\'', "'\\''")))
}

fn verify_supplementary_groups() -> PrivilegeInstallCommand {
    shell(
        "test \"$(id -nG ployz-api | tr ' ' '\\n' | sort -u | tr '\\n' ' ')\" = \"docker ployz-api ployz-control \"",
    )
}

fn failure(message: impl fmt::Display) -> FailureMessage {
    FailureMessage::try_new(message.to_string())
        .expect("privilege contract generated a non-empty failure")
}
