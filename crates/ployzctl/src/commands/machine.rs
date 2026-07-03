use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;

use clap::Args;
use ployz_core::ids::{MachineId, OperationId, SubjectTokenError};
use ployz_core::install::{InstallArtifactVersion, MachineJoinBundle, MachineJoinClusterName};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::OperationIdempotencyKey;
use ployz_core::roles::InstallRolePolicy;
use ployz_core::state::GatewayServingStatus;
use ployz_sdk_types::{
    AcceptedOperation, MachineAddAccepted, MachineAddRequest, MachineInspectRequest,
    MachineListRequest, MachineListResult, MachineSnapshot, MachineUpdateRequest,
};

pub use ployz_sdk_types::MachineName;
pub use ployz_sdk_types::{
    BootstrapCommandError, MachineBootstrapUrl, MachineJoinRuntimeNatsUrl, MachineJoinToken,
};

use crate::bootstrap_command::{
    BootstrapInstaller, BootstrapRelease, DEFAULT_BOOTSTRAP_URL, DEFAULT_CLUSTER_NAME,
    DEFAULT_RELEASE_CHANNEL, JoinBootstrapCommand,
};
use crate::client_ids::generate_client_machine_update_id;
use crate::commands::role_policy::RolePolicyCli;
use crate::commands::{PloyzctlCliError, invalid_value};
use crate::ssh::SshTarget;

/// Quick-start machine identity: the machine ID and machine name are the same
/// value (R5), derived from the remote hostname unless `--name` overrides
/// it (R6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineIdentity {
    pub machine_id: MachineId,
    pub name: MachineName,
}

impl MachineIdentity {
    /// Derives the identity from the remote system hostname. Fails before
    /// any remote install when the hostname is not a valid Ployz identity.
    pub fn from_remote_hostname(hostname: &str) -> Result<Self, MachineIdentityError> {
        Self::try_new(hostname).map_err(|source| MachineIdentityError::InvalidHostname {
            hostname: hostname.to_owned(),
            source,
        })
    }

    /// Derives the identity from the explicit `--name` override.
    pub fn from_name_override(name: &str) -> Result<Self, MachineIdentityError> {
        Self::try_new(name).map_err(|source| MachineIdentityError::InvalidNameOverride {
            name: name.to_owned(),
            source,
        })
    }

    fn try_new(value: &str) -> Result<Self, SubjectTokenError> {
        let machine_id = MachineId::try_new(value)?;
        let name = MachineName::try_new(value)?;
        Ok(Self { machine_id, name })
    }
}

/// Picks the quick-start identity: `--name` wins when present, the remote
/// hostname otherwise.
pub fn derive_machine_identity(
    hostname: &str,
    name_override: Option<&str>,
) -> Result<MachineIdentity, MachineIdentityError> {
    match name_override {
        Some(name) => MachineIdentity::from_name_override(name),
        None => MachineIdentity::from_remote_hostname(hostname),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineIdentityError {
    #[error(
        "remote hostname \"{hostname}\" is not a valid Ployz machine identity ({source}); pass --name NAME to choose one explicitly"
    )]
    InvalidHostname {
        hostname: String,
        source: SubjectTokenError,
    },
    #[error("--name \"{name}\" is not a valid Ployz machine identity ({source})")]
    InvalidNameOverride {
        name: String,
        source: SubjectTokenError,
    },
}

/// `ployzctl machine init USER@HOST`: form a first-machine cluster on the
/// remote machine through SSH and record local client context (R4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineInitCommand {
    pub target: SshTarget,
    /// `--name` override, validated at parse time (R6). `None` derives the
    /// identity from the remote hostname at runtime.
    pub identity_override: Option<MachineIdentity>,
    /// Gateway and DNS default to install (R8); `--no-gateway`/`--no-dns`
    /// are the explicit opt-outs.
    pub roles: InstallRolePolicy,
    /// Release selection used by bootstrap delivery. Defaults to the alpha
    /// channel; exact versions are explicit.
    pub release: BootstrapRelease,
    /// `--release-manifest` override; `None` derives the URL from the
    /// version and the remote machine architecture.
    pub release_manifest_url: Option<String>,
    /// Return after the activation operation is accepted instead of waiting
    /// for terminal status.
    pub detach: bool,
    /// Recorded in the install spec so machine-add prints real install
    /// lines; also the default installer source.
    pub bootstrap_url: MachineBootstrapUrl,
    pub cluster_name: MachineJoinClusterName,
    /// `--installer-script`: run an installer already on the remote machine
    /// instead of piping the bootstrap URL through `sh` (test seam).
    pub installer_script: Option<String>,
}

impl MachineInitCommand {
    /// How the remote machine obtains and runs the installer.
    #[must_use]
    pub fn installer(&self) -> BootstrapInstaller {
        match &self.installer_script {
            Some(path) => BootstrapInstaller::RemoteScript(path.clone()),
            None => BootstrapInstaller::BootstrapUrl(self.bootstrap_url.clone()),
        }
    }
}

/// What a successful `machine init` reports: the activation operation and
/// the local context later commands connect through (R13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineInitOutput {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub context_path: PathBuf,
}

impl MachineInitOutput {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nfirst-machine {} active\ncontext {}\nnext: ployzctl deploy --image IMAGE --route HOSTNAME:PORT\n",
            self.operation_id.as_str(),
            self.machine_id.as_str(),
            self.context_path.display(),
        )
    }
}

/// `ployzctl machine add USER@HOST`: submit the normal machine-add
/// operation, join the remote machine through SSH, and watch the operation
/// to completion (R9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddRemoteCommand {
    pub target: SshTarget,
    /// `--name` override; `None` derives the identity from the remote
    /// hostname at runtime.
    pub identity_override: Option<MachineIdentity>,
    /// Gateway and DNS default to install (R8); `--no-gateway` opts out
    /// of the gateway role for this machine.
    pub roles: InstallRolePolicy,
    /// Return after the remote join installer is delivered instead of
    /// waiting for terminal status.
    pub detach: bool,
    /// `--installer-script`: run an installer already on the remote machine
    /// instead of the accepted bootstrap URL (test seam).
    pub installer_script: Option<String>,
}

impl MachineAddRemoteCommand {
    #[must_use]
    pub fn installer(&self) -> MachineAddInstaller {
        match &self.installer_script {
            Some(path) => MachineAddInstaller::RemoteScript(path.clone()),
            None => MachineAddInstaller::FromAcceptedBootstrapUrl,
        }
    }
}

/// How the remote join obtains the installer. The bootstrap URL is
/// server-provided (it arrives in the machine-add acceptance), so the
/// product variant carries no payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineAddInstaller {
    FromAcceptedBootstrapUrl,
    RemoteScript(String),
}

/// What a completed remote `machine add` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddRemoteOutput {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
}

impl MachineAddRemoteOutput {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nmachine {}\nmachine-add completed\n",
            self.operation_id.as_str(),
            self.machine_id.as_str(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddRemoteDetachedOutput {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
}

impl MachineAddRemoteDetachedOutput {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nmachine {}\nwatch ployzctl ops watch {}\n",
            self.operation_id.as_str(),
            self.machine_id.as_str(),
            self.operation_id.as_str(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddCommand {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
}

impl MachineAddCommand {
    #[must_use]
    pub fn into_request(self) -> MachineAddRequest {
        MachineAddRequest {
            operation_id: self.operation_id,
            idempotency_key: self.idempotency_key,
            machine_id: self.machine_id,
            name: self.name,
            roles: self.roles,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MachineAddOutput {
    pub machine_id: MachineId,
    pub accepted: AcceptedOperation,
    pub bootstrap_url: MachineBootstrapUrl,
    pub join_bundle: MachineJoinBundle,
    pub join_token: MachineJoinToken,
    /// The cluster-static low-privilege Join credential printed into the
    /// install command line (it may only redeem/report a join).
    pub join_seed: NatsUserSeed,
}

impl MachineAddOutput {
    #[must_use]
    pub fn from_accepted(accepted: MachineAddAccepted, join_seed: NatsUserSeed) -> Self {
        Self {
            machine_id: accepted.machine_id,
            accepted: accepted.accepted,
            bootstrap_url: accepted.bootstrap_url,
            join_bundle: accepted.join_bundle,
            join_token: accepted.join_token,
            join_seed,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nmachine {}\njoin-token {}\ninstall {}\n",
            self.accepted.operation_id.as_str(),
            self.machine_id.as_str(),
            self.join_token.as_str(),
            self.install_command(&MachineAddInstaller::FromAcceptedBootstrapUrl, None),
        )
    }

    /// The full join-mode installer invocation for the remote machine: the
    /// printed install line and the SSH-driven remote add run exactly the
    /// same command.
    #[must_use]
    pub fn install_command(
        &self,
        installer: &MachineAddInstaller,
        machine_public_ip: Option<IpAddr>,
    ) -> String {
        self.join_bootstrap_command(installer, machine_public_ip)
            .render()
    }

    #[must_use]
    pub fn join_bootstrap_command(
        &self,
        installer: &MachineAddInstaller,
        machine_public_ip: Option<IpAddr>,
    ) -> JoinBootstrapCommand {
        JoinBootstrapCommand {
            installer: match installer {
                MachineAddInstaller::FromAcceptedBootstrapUrl => {
                    BootstrapInstaller::BootstrapUrl(self.bootstrap_url.clone())
                }
                MachineAddInstaller::RemoteScript(path) => {
                    BootstrapInstaller::RemoteScript(path.clone())
                }
            },
            version: self.join_bundle.material.ployzd.version.as_str().to_owned(),
            runtime_nats_url: self.runtime_nats_url().clone(),
            trusted_ca_b64: self.trusted_ca_b64(),
            join_seed: self.join_seed.clone(),
            join_token: self.join_token.clone(),
            machine_public_ip,
        }
    }

    #[must_use]
    fn runtime_nats_url(&self) -> &MachineJoinRuntimeNatsUrl {
        &self.join_bundle.material.runtime_nats_url
    }

    /// The cluster CA (public material) base64-packed for the env line.
    #[must_use]
    fn trusted_ca_b64(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(self.join_bundle.material.trusted_nats.ca_pem.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineUpdateOutput {
    pub accepted: AcceptedOperation,
}

impl MachineUpdateOutput {
    #[must_use]
    pub const fn from_accepted(accepted: AcceptedOperation) -> Self {
        Self { accepted }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nwatch ployzctl ops watch {}\n",
            self.accepted.operation_id.as_str(),
            self.accepted.operation_id.as_str()
        )
    }
}

impl fmt::Debug for MachineAddOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachineAddOutput")
            .field("machine_id", &self.machine_id)
            .field("accepted", &self.accepted)
            .field("bootstrap_url", &self.bootstrap_url)
            .field("join_bundle", &self.join_bundle)
            .field("join_token", &self.join_token)
            .field("join_seed", &self.join_seed)
            .finish()
    }
}

/// `machine add` keeps both shapes: the quick-start remote-target mode
/// (`machine add root@host`) and the low-level explicit mode for tests and
/// automation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedMachineAdd {
    Explicit(MachineAddCommand),
    Remote(MachineAddRemoteCommand),
}

pub(crate) fn machine_add_command(
    parsed: MachineAddCli,
) -> Result<ParsedMachineAdd, PloyzctlCliError> {
    match parsed.into_mode()? {
        MachineAddCliMode::Remote {
            target,
            name,
            roles,
            detach,
            installer_script,
        } => {
            let target =
                SshTarget::parse(&target).map_err(|error| invalid_value("<target>", error))?;
            let identity_override = name
                .as_deref()
                .map(MachineIdentity::from_name_override)
                .transpose()
                .map_err(|error| invalid_value("--name", error))?;
            Ok(ParsedMachineAdd::Remote(MachineAddRemoteCommand {
                target,
                identity_override,
                roles,
                detach,
                installer_script: validate_installer_script(installer_script)?,
            }))
        }
        MachineAddCliMode::Explicit {
            operation,
            idempotency_key,
            machine,
            name,
            roles,
        } => Ok(ParsedMachineAdd::Explicit(MachineAddCommand {
            operation_id: OperationId::try_new(operation)
                .map_err(|error| invalid_value("--operation", error))?,
            idempotency_key: OperationIdempotencyKey::try_new(idempotency_key)
                .map_err(|error| invalid_value("--idempotency-key", error))?,
            machine_id: MachineId::try_new(machine)
                .map_err(|error| invalid_value("--machine", error))?,
            name: MachineName::try_new(name).map_err(|error| invalid_value("--name", error))?,
            roles,
        })),
    }
}

pub(crate) fn machine_add_remote_command(
    parsed: MachineAddRemoteCli,
) -> Result<MachineAddRemoteCommand, PloyzctlCliError> {
    let MachineAddRemoteCli {
        target,
        name,
        roles,
        detach,
        installer_script,
    } = parsed;
    let target = SshTarget::parse(&target).map_err(|error| invalid_value("<target>", error))?;
    let identity_override = name
        .as_deref()
        .map(MachineIdentity::from_name_override)
        .transpose()
        .map_err(|error| invalid_value("--name", error))?;
    Ok(MachineAddRemoteCommand {
        target,
        identity_override,
        roles: roles.into_policy(),
        detach,
        installer_script: validate_installer_script(installer_script)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MachineAddCliMode {
    Remote {
        target: String,
        name: Option<String>,
        roles: InstallRolePolicy,
        detach: bool,
        installer_script: Option<String>,
    },
    Explicit {
        operation: String,
        idempotency_key: String,
        machine: String,
        name: String,
        roles: InstallRolePolicy,
    },
}

#[derive(Debug, Args)]
pub(crate) struct MachineAddCli {
    /// Remote machine to join (`user@host`); the quick-start mode.
    target: Option<String>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long, conflicts_with = "target")]
    machine: Option<String>,
    #[arg(long, conflicts_with = "target")]
    operation: Option<String>,
    #[arg(long, conflicts_with = "target")]
    idempotency_key: Option<String>,
    #[command(flatten)]
    roles: RolePolicyCli,
    #[arg(long, requires = "target")]
    detach: bool,
    #[arg(long, requires = "target")]
    installer_script: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct MachineAddRemoteCli {
    /// Remote machine to join (`user@host`).
    target: String,
    #[arg(long)]
    name: Option<String>,
    #[command(flatten)]
    roles: RolePolicyCli,
    #[arg(long)]
    pub detach: bool,
    #[arg(long)]
    installer_script: Option<String>,
}

impl MachineAddCli {
    fn into_mode(self) -> Result<MachineAddCliMode, PloyzctlCliError> {
        let Self {
            target,
            name,
            machine,
            operation,
            idempotency_key,
            roles,
            detach,
            installer_script,
        } = self;
        let roles = roles.into_policy();

        if let Some(target) = target {
            return Ok(MachineAddCliMode::Remote {
                target,
                name,
                roles,
                detach,
                installer_script,
            });
        }

        Ok(MachineAddCliMode::Explicit {
            operation: require_explicit_machine_add_value(operation, "--operation")?,
            idempotency_key: require_explicit_machine_add_value(
                idempotency_key,
                "--idempotency-key",
            )?,
            machine: require_explicit_machine_add_value(machine, "--machine")?,
            name: require_explicit_machine_add_value(name, "--name")?,
            roles,
        })
    }
}

fn require_explicit_machine_add_value(
    value: Option<String>,
    flag: &'static str,
) -> Result<String, PloyzctlCliError> {
    value.ok_or_else(|| {
        crate::commands::cli_error(format!(
            "{flag} is required (or pass USER@HOST for a remote machine add)"
        ))
    })
}

pub(crate) fn machine_init_command(
    parsed: MachineInitCli,
) -> Result<MachineInitCommand, PloyzctlCliError> {
    let MachineInitCli {
        target,
        name,
        roles,
        version,
        channel,
        release_manifest,
        detach,
        bootstrap_url,
        cluster_name,
        installer_script,
    } = parsed;

    let target = SshTarget::parse(&target).map_err(|error| invalid_value("<target>", error))?;
    let identity_override = name
        .as_deref()
        .map(MachineIdentity::from_name_override)
        .transpose()
        .map_err(|error| invalid_value("--name", error))?;
    let roles = roles.into_policy();
    let release = match (version, channel) {
        (Some(version), _) if version.trim().is_empty() => {
            return Err(invalid_value("--version", "version is empty"));
        }
        (_, Some(channel)) if channel.trim().is_empty() => {
            return Err(invalid_value("--channel", "channel is empty"));
        }
        (Some(_), Some(_)) => {
            return Err(invalid_value(
                "--channel",
                "pass either --version or --channel, not both",
            ));
        }
        (Some(version), None) => BootstrapRelease::Version(version),
        (None, Some(channel)) => BootstrapRelease::Channel(channel),
        (None, None) => BootstrapRelease::Channel(DEFAULT_RELEASE_CHANNEL.to_owned()),
    };
    let release_manifest_url = match release_manifest {
        Some(url) if url.trim().is_empty() => {
            return Err(invalid_value("--release-manifest", "manifest URL is empty"));
        }
        other => other,
    };
    let bootstrap_url = MachineBootstrapUrl::try_new(
        bootstrap_url.unwrap_or_else(|| DEFAULT_BOOTSTRAP_URL.to_owned()),
    )
    .map_err(|error| invalid_value("--bootstrap-url", error))?;
    let cluster_name = MachineJoinClusterName::try_new(
        cluster_name.unwrap_or_else(|| DEFAULT_CLUSTER_NAME.to_owned()),
    )
    .map_err(|error| invalid_value("--cluster-name", error))?;

    Ok(MachineInitCommand {
        target,
        identity_override,
        roles,
        release,
        release_manifest_url,
        detach,
        bootstrap_url,
        cluster_name,
        installer_script: validate_installer_script(installer_script)?,
    })
}

fn validate_installer_script(
    installer_script: Option<String>,
) -> Result<Option<String>, PloyzctlCliError> {
    match installer_script {
        Some(path) if path.trim().is_empty() => Err(invalid_value(
            "--installer-script",
            "installer script path is empty",
        )),
        other => Ok(other),
    }
}

#[derive(Debug, Args)]
pub(crate) struct MachineInitCli {
    /// Remote machine to form the first machine on (`user@host`).
    target: String,
    /// Machine identity override (defaults to the remote hostname).
    #[arg(long)]
    name: Option<String>,
    #[command(flatten)]
    roles: RolePolicyCli,
    #[arg(long)]
    version: Option<String>,
    #[arg(long, conflicts_with = "version")]
    channel: Option<String>,
    #[arg(long)]
    release_manifest: Option<String>,
    #[arg(long)]
    detach: bool,
    #[arg(long)]
    bootstrap_url: Option<String>,
    #[arg(long)]
    cluster_name: Option<String>,
    #[arg(long)]
    installer_script: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineListCommand;

impl MachineListCommand {
    #[must_use]
    pub const fn into_request(self) -> MachineListRequest {
        MachineListRequest {}
    }
}

pub(crate) fn machine_list_command(_: EmptyCli) -> MachineListCommand {
    MachineListCommand
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineInspectCommand {
    pub machine_id: MachineId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineUpdateCommand {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub target_version: InstallArtifactVersion,
    pub detach: bool,
}

impl MachineUpdateCommand {
    #[must_use]
    pub fn into_request(self) -> MachineUpdateRequest {
        MachineUpdateRequest {
            operation_id: self.operation_id,
            machine_id: self.machine_id,
            target_version: self.target_version,
        }
    }
}

impl MachineInspectCommand {
    #[must_use]
    pub fn into_request(self) -> MachineInspectRequest {
        MachineInspectRequest {
            machine_id: self.machine_id,
        }
    }
}

pub(crate) fn machine_inspect_command(
    parsed: MachineInspectCli,
) -> Result<MachineInspectCommand, PloyzctlCliError> {
    let machine_id = MachineId::try_new(parsed.machine_id)
        .map_err(|error| invalid_value("<machine_id>", error))?;

    Ok(MachineInspectCommand { machine_id })
}

pub(crate) fn machine_update_command(
    parsed: MachineUpdateCli,
) -> Result<MachineUpdateCommand, PloyzctlCliError> {
    let machine_id = MachineId::try_new(parsed.machine_id)
        .map_err(|error| invalid_value("<machine_id>", error))?;
    let target_version = InstallArtifactVersion::try_new(parsed.version)
        .map_err(|error| invalid_value("--version", error))?;
    let operation_id = generate_client_machine_update_id(&machine_id)
        .map_err(|error| PloyzctlCliError::InvalidValue {
            flag: "<machine_id>",
            message: error.to_string(),
        })?
        .operation_id;

    Ok(MachineUpdateCommand {
        operation_id,
        machine_id,
        target_version,
        detach: parsed.detach,
    })
}

#[derive(Debug, Args)]
pub(crate) struct MachineInspectCli {
    machine_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct MachineUpdateCli {
    machine_id: String,
    #[arg(long)]
    version: String,
    #[arg(long)]
    detach: bool,
}

#[derive(Debug, Args)]
pub(crate) struct EmptyCli {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineListOutput {
    pub machines: Vec<MachineSnapshot>,
}

impl MachineListOutput {
    #[must_use]
    pub fn from_result(result: MachineListResult) -> Self {
        Self {
            machines: result.machines,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let rendered = self
            .machines
            .iter()
            .map(render_machine_summary)
            .collect::<Vec<_>>()
            .join("\n");

        if rendered.is_empty() {
            rendered
        } else {
            rendered + "\n"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineInspectOutput {
    pub machine: MachineSnapshot,
}

impl MachineInspectOutput {
    #[must_use]
    pub fn new(machine: MachineSnapshot) -> Self {
        Self { machine }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "machine {}\nname {}\nactivated-by {}\npublic-ip {}\ngateway {}\ncontainers {}\nsubstrate {}\n",
            self.machine.active.machine_id.as_str(),
            self.machine.active.name.as_str(),
            self.machine.active.activated_by.as_str(),
            render_public_ip(&self.machine),
            render_gateway(&self.machine),
            self.machine.observed_container_count,
            render_substrate_versions(&self.machine),
        )
    }
}

fn render_machine_summary(machine: &MachineSnapshot) -> String {
    format!(
        "{} {} public-ip {} gateway {} containers {} substrate {}",
        machine.active.machine_id.as_str(),
        machine.active.name.as_str(),
        render_public_ip(machine),
        render_gateway(machine),
        machine.observed_container_count,
        render_substrate_versions(machine),
    )
}

fn render_substrate_versions(machine: &MachineSnapshot) -> String {
    let Some(versions) = &machine.active.substrate_versions else {
        return "ployzd unknown keeper unknown".to_owned();
    };
    format!(
        "ployzd {} keeper {}",
        versions
            .ployzd
            .as_ref()
            .map_or("unknown", |version| version.as_str()),
        versions
            .keeper
            .as_ref()
            .map_or("unknown", |version| version.as_str())
    )
}

fn render_public_ip(machine: &MachineSnapshot) -> String {
    match &machine.public_ip {
        Some(observation) => observation.public_ip.to_string(),
        None => "unknown".to_owned(),
    }
}

fn render_gateway(machine: &MachineSnapshot) -> String {
    match &machine.gateway {
        Some(observation) => format!(
            "{} {} routes {}",
            render_gateway_serving(observation.serving),
            observation.listen_addr,
            observation.route_count
        ),
        None => "none".to_owned(),
    }
}

const fn render_gateway_serving(serving: GatewayServingStatus) -> &'static str {
    match serving {
        GatewayServingStatus::Current => "current",
        GatewayServingStatus::LastKnownGood => "last-known-good",
        GatewayServingStatus::Unavailable => "unavailable",
    }
}
