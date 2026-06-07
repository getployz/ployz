use std::fmt;

use ployz_core::ids::{NodeId, OperationId};
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
    MachineJoinClusterName,
};
use ployz_core::ops::OperationIdempotencyKey;
use ployz_sdk_types::{
    AcceptedOperation, MachineAddAccepted, MachineAddGateway, MachineAddRequest, MachineJoinBundle,
    MachineJoinPloyzdArtifact,
};

pub use ployz_sdk_types::MachineName;
pub use ployz_sdk_types::{BootstrapCommandError, MachineBootstrapUrl, MachineJoinToken};

use crate::commands::{ArgCursor, PloyzctlCliError, invalid_value, required, set_once};
use crate::shell::shell_quote;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddCommand {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub node_id: NodeId,
    pub name: MachineName,
    pub gateway: MachineAddGateway,
    pub join_bundle: MachineJoinBundle,
}

impl MachineAddCommand {
    #[must_use]
    pub fn into_request(self) -> MachineAddRequest {
        MachineAddRequest {
            operation_id: self.operation_id,
            idempotency_key: self.idempotency_key,
            node_id: self.node_id,
            name: self.name,
            gateway: self.gateway,
            join_bundle: self.join_bundle,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MachineAddOutput {
    pub node_id: NodeId,
    pub accepted: AcceptedOperation,
    pub bootstrap_url: MachineBootstrapUrl,
    pub join_token: MachineJoinToken,
    pub nats_url: Option<String>,
}

impl MachineAddOutput {
    #[must_use]
    pub fn from_accepted(accepted: MachineAddAccepted) -> Self {
        Self {
            node_id: accepted.node_id,
            accepted: accepted.accepted,
            bootstrap_url: accepted.bootstrap_url,
            join_token: accepted.join_token,
            nats_url: None,
        }
    }

    #[must_use]
    pub fn with_nats_url(mut self, nats_url: Option<String>) -> Self {
        self.nats_url = nats_url;
        self
    }

    #[must_use]
    pub fn render(&self) -> String {
        let shell = match &self.nats_url {
            Some(nats_url) => format!("PLOYZ_NATS_URL={} sh", shell_quote(nats_url)),
            None => "sh".to_owned(),
        };

        format!(
            "operation {}\nnode {}\ninstall curl -fsSL -- {} | {} -s -- --join-token {}\n",
            self.accepted.operation_id.as_str(),
            self.node_id.as_str(),
            shell_quote(self.bootstrap_url.as_str()),
            shell,
            shell_quote(self.join_token.as_str())
        )
    }
}

impl fmt::Debug for MachineAddOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachineAddOutput")
            .field("node_id", &self.node_id)
            .field("accepted", &self.accepted)
            .field("bootstrap_url", &self.bootstrap_url)
            .field("join_token", &self.join_token)
            .field("nats_url", &self.nats_url.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

pub fn parse_machine_add_command(args: &[String]) -> Result<MachineAddCommand, PloyzctlCliError> {
    let mut node_id = None;
    let mut name = None;
    let mut gateway = MachineAddGateway::Skip;
    let mut operation_id = None;
    let mut idempotency_key = None;
    let mut cluster_name = None;
    let mut ployzd_version = None;
    let mut ployzd_source = None;
    let mut ployzd_sha256 = None;
    let mut ployzd_install_path = None;
    let mut args = ArgCursor::new(args);

    while !args.is_empty() {
        if args.take_flag("--gateway") {
            if gateway == MachineAddGateway::Install {
                return Err(PloyzctlCliError::DuplicateArgument { flag: "--gateway" });
            }
            gateway = MachineAddGateway::Install;
            continue;
        }
        if let Some(value) = args.take_value("--node")? {
            let parsed = NodeId::try_new(value).map_err(|error| invalid_value("--node", error))?;
            set_once(&mut node_id, parsed, "--node")?;
            continue;
        }
        if let Some(value) = args.take_value("--name")? {
            let parsed =
                MachineName::try_new(value).map_err(|error| invalid_value("--name", error))?;
            set_once(&mut name, parsed, "--name")?;
            continue;
        }
        if let Some(value) = args.take_value("--operation")? {
            let parsed =
                OperationId::try_new(value).map_err(|error| invalid_value("--operation", error))?;
            set_once(&mut operation_id, parsed, "--operation")?;
            continue;
        }
        if let Some(value) = args.take_value("--idempotency-key")? {
            let parsed = OperationIdempotencyKey::try_new(value)
                .map_err(|error| invalid_value("--idempotency-key", error))?;
            set_once(&mut idempotency_key, parsed, "--idempotency-key")?;
            continue;
        }
        if let Some(value) = args.take_value("--cluster")? {
            set_once(&mut cluster_name, value, "--cluster")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-version")? {
            set_once(&mut ployzd_version, value, "--ployzd-version")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-source")? {
            set_once(&mut ployzd_source, value, "--ployzd-source")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-sha256")? {
            set_once(&mut ployzd_sha256, value, "--ployzd-sha256")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-install-path")? {
            set_once(&mut ployzd_install_path, value, "--ployzd-install-path")?;
            continue;
        }
        return Err(args.unexpected());
    }

    let join_bundle = MachineJoinBundle {
        cluster_name: MachineJoinClusterName::try_new(required(cluster_name, "--cluster")?)
            .map_err(|error| invalid_value("--cluster", error))?,
        ployzd: MachineJoinPloyzdArtifact {
            version: InstallArtifactVersion::try_new(required(ployzd_version, "--ployzd-version")?)
                .map_err(|error| invalid_value("--ployzd-version", error))?,
            source: InstallArtifactSource::try_new(required(ployzd_source, "--ployzd-source")?)
                .map_err(|error| invalid_value("--ployzd-source", error))?,
            sha256: InstallSha256Digest::try_new(required(ployzd_sha256, "--ployzd-sha256")?)
                .map_err(|error| invalid_value("--ployzd-sha256", error))?,
            install_path: AbsoluteInstallPath::try_new(required(
                ployzd_install_path,
                "--ployzd-install-path",
            )?)
            .map_err(|error| invalid_value("--ployzd-install-path", error))?,
        },
    };

    Ok(MachineAddCommand {
        operation_id: required(operation_id, "--operation")?,
        idempotency_key: required(idempotency_key, "--idempotency-key")?,
        node_id: required(node_id, "--node")?,
        name: required(name, "--name")?,
        gateway,
        join_bundle,
    })
}
