//! Typed keeper step plans.

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use ployz_core::ids::NodeId;
use ployz_core::install::{
    AbsoluteInstallPath, MachineBootstrapUrl, MachineJoinBundle, MachineJoinSecretDelivery,
};
use ployz_core::nats_config::{NatsListener, NatsServerConfig, NatsServerTlsFiles};
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, FirstNodeGateway, first_node_process_set};
use ployz_nats::connect::NatsClientUrl;
use sha2::{Digest, Sha256};

use crate::artifacts::{
    ArtifactKind, ArtifactTarget, DataplaneArtifactTargets, KeeperArtifactTarget,
    NatsServerArtifactTarget, PloyzdArtifactTarget,
};
use crate::systemd::{
    NatsServerUnitTarget, PloyzdRoleEnvironmentFile, SupervisorUnitSpec, SupervisorUnitTarget,
};

const DEFAULT_NATS_PORT: u16 = 4222;
const PLOYZ_NODE_ID_ENV: &str = "PLOYZ_NODE_ID";
const PLOYZ_NODE_PUBLIC_IP_ENV: &str = "PLOYZ_NODE_PUBLIC_IP";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperStepPlan {
    steps: Vec<KeeperStep>,
}

impl KeeperStepPlan {
    #[must_use]
    fn new(steps: Vec<KeeperStep>) -> Self {
        Self { steps }
    }

    #[must_use]
    pub fn steps(&self) -> &[KeeperStep] {
        &self.steps
    }

    #[must_use]
    pub fn installs_artifact_kind(&self, kind: ArtifactKind) -> bool {
        self.steps.iter().any(|step| {
            matches!(
                step,
                KeeperStep::InstallArtifact(artifact) if artifact.kind() == kind
            )
        })
    }

    #[must_use]
    pub fn writes_ployzd_role_units(&self) -> bool {
        self.steps.iter().any(|step| {
            matches!(step, KeeperStep::WriteSupervisorUnit(spec) if matches!(spec.target(), SupervisorUnitTarget::PloyzdRole(_)))
        })
    }

    #[must_use]
    pub fn writes_nats_server_unit(&self) -> bool {
        self.steps.iter().any(|step| {
            matches!(step, KeeperStep::WriteSupervisorUnit(spec) if spec.target() == SupervisorUnitTarget::NatsServer)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperStep {
    VerifyHost(HostPrerequisite),
    InstallArtifact(ArtifactTarget),
    WritePloyzdRoleEnvironment(PloyzdRoleEnvironmentTarget),
    WriteNatsServerConfig(NatsServerConfigTarget),
    WriteSupervisorUnit(SupervisorUnitSpec),
    StartSupervisorUnit(SupervisorUnitTarget),
    RestartSupervisorUnit(SupervisorUnitTarget),
    StoreJoinMaterial(KeeperJoinMaterial),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperStepLabel {
    VerifyHost(HostPrerequisite),
    InstallArtifact(ArtifactTarget),
    WritePloyzdRoleEnvironment(PloyzdRoleEnvironmentTarget),
    WriteNatsServerConfig(NatsServerConfigTarget),
    WriteSupervisorUnit(SupervisorUnitTarget),
    StartSupervisorUnit(SupervisorUnitTarget),
    RestartSupervisorUnit(SupervisorUnitTarget),
    RedeemJoinToken,
    ReportJoinResult,
    ConsumeJoinTokenFile,
    StoreJoinMaterial(RedactedJoinMaterial),
}

impl KeeperStepLabel {
    #[must_use]
    pub fn from_step(step: &KeeperStep) -> Self {
        match step {
            KeeperStep::VerifyHost(prerequisite) => Self::VerifyHost(*prerequisite),
            KeeperStep::InstallArtifact(target) => Self::InstallArtifact(target.clone()),
            KeeperStep::WritePloyzdRoleEnvironment(target) => {
                Self::WritePloyzdRoleEnvironment(target.clone())
            }
            KeeperStep::WriteNatsServerConfig(target) => {
                Self::WriteNatsServerConfig(target.clone())
            }
            KeeperStep::WriteSupervisorUnit(spec) => Self::WriteSupervisorUnit(spec.target()),
            KeeperStep::StartSupervisorUnit(target) => Self::StartSupervisorUnit(target.clone()),
            KeeperStep::RestartSupervisorUnit(target) => {
                Self::RestartSupervisorUnit(target.clone())
            }
            KeeperStep::StoreJoinMaterial(material) => Self::StoreJoinMaterial(material.redacted()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPrerequisite {
    LinuxRootSystemd,
}

#[derive(Clone, PartialEq, Eq)]
pub struct JoinToken(String);

impl JoinToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, JoinMaterialError> {
        let value = value.into();
        if value.is_empty() {
            return Err(JoinMaterialError::EmptyJoinToken);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn redacted(&self) -> &'static str {
        "[redacted]"
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for JoinToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("JoinToken")
            .field(&self.redacted())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct KeeperJoinMaterial {
    redacted: RedactedJoinMaterial,
    nats_credentials: String,
}

impl KeeperJoinMaterial {
    pub fn from_join_payload(
        node_id: NodeId,
        join_bundle: &MachineJoinBundle,
        secret_delivery: &MachineJoinSecretDelivery,
    ) -> Result<Self, JoinMaterialError> {
        Self::new(
            node_id,
            join_bundle.material.cluster_name.as_str(),
            secret_delivery.nats_credentials.secret(),
            join_bundle.material.trusted_nats.server_name.as_str(),
            ca_pem_sha256(join_bundle.material.trusted_nats.ca_pem.as_str()),
        )
    }

    pub fn new(
        node_id: NodeId,
        cluster_name: impl Into<String>,
        nats_credentials: impl Into<String>,
        trusted_nats_server: impl Into<String>,
        trusted_nats_ca_sha256: impl Into<String>,
    ) -> Result<Self, JoinMaterialError> {
        let redacted = RedactedJoinMaterial::new(
            node_id,
            cluster_name,
            trusted_nats_server,
            trusted_nats_ca_sha256,
        )?;
        let nats_credentials = secret_file_content(nats_credentials.into())?;
        Ok(Self {
            redacted,
            nats_credentials,
        })
    }

    #[must_use]
    pub fn redacted(&self) -> RedactedJoinMaterial {
        self.redacted.clone()
    }

    #[must_use]
    pub fn nats_credentials(&self) -> &str {
        &self.nats_credentials
    }
}

impl fmt::Debug for KeeperJoinMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KeeperJoinMaterial")
            .field("redacted", &self.redacted)
            .field("nats_credentials", &"[redacted]")
            .finish()
    }
}

/// Human-diffable digest of the trusted cluster CA certificate.
#[must_use]
pub fn ca_pem_sha256(ca_pem: &str) -> String {
    let digest = Sha256::digest(ca_pem.as_bytes());
    format!("{digest:x}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedJoinMaterial {
    pub node_id: NodeId,
    pub cluster_name: String,
    pub trusted_nats_server: String,
    pub trusted_nats_ca_sha256: String,
}

impl RedactedJoinMaterial {
    pub fn new(
        node_id: NodeId,
        cluster_name: impl Into<String>,
        trusted_nats_server: impl Into<String>,
        trusted_nats_ca_sha256: impl Into<String>,
    ) -> Result<Self, JoinMaterialError> {
        let cluster_name = line_value("cluster name", cluster_name.into())?;
        let trusted_nats_server = line_value("trusted NATS server", trusted_nats_server.into())?;
        let trusted_nats_ca_sha256 =
            line_value("trusted NATS CA digest", trusted_nats_ca_sha256.into())?;

        Ok(Self {
            node_id,
            cluster_name,
            trusted_nats_server,
            trusted_nats_ca_sha256,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinMaterialError {
    EmptyJoinToken,
    EmptyClusterName,
    EmptyJoinMaterialValue { label: &'static str },
    InvalidJoinMaterialValue { label: &'static str, value: String },
    EmptySecret,
    InvalidSecret,
}

fn line_value(label: &'static str, value: String) -> Result<String, JoinMaterialError> {
    if value.is_empty() {
        return if label == "cluster name" {
            Err(JoinMaterialError::EmptyClusterName)
        } else {
            Err(JoinMaterialError::EmptyJoinMaterialValue { label })
        };
    }
    if value.contains(['\n', '\r', '=']) {
        return Err(JoinMaterialError::InvalidJoinMaterialValue { label, value });
    }
    Ok(value)
}

fn secret_file_content(value: String) -> Result<String, JoinMaterialError> {
    if value.is_empty() {
        return Err(JoinMaterialError::EmptySecret);
    }
    if value.contains('\0') {
        return Err(JoinMaterialError::InvalidSecret);
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapScriptTarget {
    pub keeper_artifact: KeeperArtifactTarget,
}

impl BootstrapScriptTarget {
    #[must_use]
    pub const fn new(keeper_artifact: KeeperArtifactTarget) -> Self {
        Self { keeper_artifact }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperJoinTarget {
    pub material: KeeperJoinMaterial,
    pub ployzd_artifact: PloyzdArtifactTarget,
    pub dataplane_artifacts: DataplaneArtifactTargets,
    pub roles: NonEmptyRoleSet,
    pub role_environment: PloyzdRoleEnvironmentTarget,
}

impl KeeperJoinTarget {
    #[must_use]
    pub fn new(
        material: KeeperJoinMaterial,
        ployzd_artifact: PloyzdArtifactTarget,
        dataplane_artifacts: DataplaneArtifactTargets,
        roles: NonEmptyRoleSet,
        role_environment: PloyzdRoleEnvironmentTarget,
    ) -> Self {
        let role_environment = role_environment
            .with_ebpf_bytecode_path(
                dataplane_artifacts
                    .ebpf_bytecode
                    .install_path()
                    .to_path_buf(),
            )
            .with_ebpf_ctl_path(dataplane_artifacts.ebpf_ctl.install_path().to_path_buf());
        Self {
            material,
            ployzd_artifact,
            dataplane_artifacts,
            roles,
            role_environment,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstNodeInstallTarget {
    pub node_id: NodeId,
    pub ployzd_artifact: PloyzdArtifactTarget,
    pub dataplane_artifacts: DataplaneArtifactTargets,
    pub nats_server_artifact: NatsServerArtifactTarget,
    pub gateway: FirstNodeGateway,
    pub nats_server_unit: NatsServerUnitTarget,
    pub role_environment: PloyzdRoleEnvironmentTarget,
}

impl FirstNodeInstallTarget {
    #[must_use]
    pub fn new(
        node_id: NodeId,
        ployzd_artifact: PloyzdArtifactTarget,
        dataplane_artifacts: DataplaneArtifactTargets,
        nats_server_artifact: NatsServerArtifactTarget,
        gateway: FirstNodeGateway,
    ) -> Self {
        let nats_server_unit = NatsServerUnitTarget::new(
            nats_server_artifact.install_path().to_path_buf(),
            NatsServerUnitTarget::default_paths()
                .config_path()
                .to_path_buf(),
        )
        .expect("validated nats-server artifact install path is a valid unit path");
        let role_environment = PloyzdRoleEnvironmentTarget::default_path(
            node_id.clone(),
            NatsClientUrl::loopback(DEFAULT_NATS_PORT),
        )
        .with_ebpf_bytecode_path(
            dataplane_artifacts
                .ebpf_bytecode
                .install_path()
                .to_path_buf(),
        )
        .with_ebpf_ctl_path(dataplane_artifacts.ebpf_ctl.install_path().to_path_buf());
        Self {
            node_id,
            ployzd_artifact,
            dataplane_artifacts,
            nats_server_artifact,
            gateway,
            nats_server_unit,
            role_environment,
        }
    }

    #[must_use]
    pub fn with_nats_server_unit(mut self, nats_server_unit: NatsServerUnitTarget) -> Self {
        self.nats_server_unit = nats_server_unit;
        self
    }

    #[must_use]
    pub fn with_role_environment(mut self, role_environment: PloyzdRoleEnvironmentTarget) -> Self {
        self.role_environment = role_environment
            .with_ebpf_bytecode_path(
                self.dataplane_artifacts
                    .ebpf_bytecode
                    .install_path()
                    .to_path_buf(),
            )
            .with_ebpf_ctl_path(
                self.dataplane_artifacts
                    .ebpf_ctl
                    .install_path()
                    .to_path_buf(),
            );
        self
    }

    #[must_use]
    pub fn with_machine_bootstrap_url(mut self, url: MachineBootstrapUrl) -> Self {
        self.role_environment = self.role_environment.with_machine_bootstrap_url(url);
        self
    }

    #[must_use]
    pub fn with_machine_join_template_file(mut self, path: AbsoluteInstallPath) -> Self {
        self.role_environment = self.role_environment.with_machine_join_template_file(path);
        self
    }

    #[must_use]
    pub fn with_node_public_ip(mut self, public_ip: IpAddr) -> Self {
        self.role_environment = self.role_environment.with_node_public_ip(public_ip);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzdRoleEnvironmentTarget {
    file: PloyzdRoleEnvironmentFile,
    node_id: NodeId,
    nats_url: NatsClientUrl,
    node_public_ip: Option<IpAddr>,
    machine_bootstrap_url: Option<MachineBootstrapUrl>,
    machine_join_template_file: Option<AbsoluteInstallPath>,
    ebpf_bytecode_path: Option<PathBuf>,
    ebpf_ctl_path: Option<PathBuf>,
}

impl PloyzdRoleEnvironmentTarget {
    #[must_use]
    pub fn new(file: PloyzdRoleEnvironmentFile, node_id: NodeId, nats_url: NatsClientUrl) -> Self {
        Self {
            file,
            node_id,
            nats_url,
            node_public_ip: None,
            machine_bootstrap_url: None,
            machine_join_template_file: None,
            ebpf_bytecode_path: None,
            ebpf_ctl_path: None,
        }
    }

    #[must_use]
    pub fn default_path(node_id: NodeId, nats_url: NatsClientUrl) -> Self {
        Self::new(PloyzdRoleEnvironmentFile::default_path(), node_id, nats_url)
    }

    #[must_use]
    pub const fn file(&self) -> &PloyzdRoleEnvironmentFile {
        &self.file
    }

    #[must_use]
    pub fn with_machine_bootstrap_url(mut self, url: MachineBootstrapUrl) -> Self {
        self.machine_bootstrap_url = Some(url);
        self
    }

    #[must_use]
    pub fn with_node_public_ip(mut self, public_ip: IpAddr) -> Self {
        self.node_public_ip = Some(public_ip);
        self
    }

    #[must_use]
    pub fn with_machine_join_template_file(mut self, path: AbsoluteInstallPath) -> Self {
        self.machine_join_template_file = Some(path);
        self
    }

    #[must_use]
    pub fn with_ebpf_bytecode_path(mut self, path: PathBuf) -> Self {
        self.ebpf_bytecode_path = Some(path);
        self
    }

    #[must_use]
    pub fn with_ebpf_ctl_path(mut self, path: PathBuf) -> Self {
        self.ebpf_ctl_path = Some(path);
        self
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        self.file
            .path()
            .parent()
            .expect("validated ployzd role env path has a directory")
    }

    #[must_use]
    pub fn file_name(&self) -> &str {
        self.file
            .path()
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .expect("validated ployzd role env path has a UTF-8 file name")
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut output = format!("PLOYZ_NATS_URL={}\n", self.nats_url.as_str());
        output.push_str(PLOYZ_NODE_ID_ENV);
        output.push('=');
        output.push_str(self.node_id.as_str());
        output.push('\n');
        if let Some(public_ip) = self.node_public_ip {
            output.push_str(PLOYZ_NODE_PUBLIC_IP_ENV);
            output.push('=');
            output.push_str(&public_ip.to_string());
            output.push('\n');
        }
        if let Some(url) = &self.machine_bootstrap_url {
            output.push_str("PLOYZ_MACHINE_BOOTSTRAP_URL=");
            output.push_str(url.as_str());
            output.push('\n');
        }
        if let Some(path) = &self.machine_join_template_file {
            output.push_str("PLOYZ_MACHINE_JOIN_TEMPLATE_FILE=");
            output.push_str(path.as_str());
            output.push('\n');
        }
        if let Some(path) = &self.ebpf_bytecode_path {
            output.push_str("PLOYZ_EBPF_BYTECODE=");
            output.push_str(&path.display().to_string());
            output.push('\n');
        }
        if let Some(path) = &self.ebpf_ctl_path {
            output.push_str("PLOYZ_EBPF_CTL=");
            output.push_str(&path.display().to_string());
            output.push('\n');
        }
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerConfigTarget {
    config_dir: PathBuf,
    config_file_name: String,
    rendered_config: String,
}

impl NatsServerConfigTarget {
    #[must_use]
    pub fn for_first_node(node_id: NodeId, unit: &NatsServerUnitTarget) -> Self {
        let config_path = unit.config_path().to_path_buf();
        let config_dir = config_path
            .parent()
            .expect("validated nats config path has a directory")
            .to_path_buf();
        let config_file_name = config_path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .expect("validated nats config path has a UTF-8 file name")
            .to_owned();

        Self {
            config_dir,
            config_file_name,
            rendered_config: NatsServerConfig::single_node(
                node_id,
                PathBuf::from("/var/lib/ployz/nats"),
                NatsListener::Loopback,
                NatsServerTlsFiles {
                    cert_file: PathBuf::from("/var/lib/ployz/nats/server.crt"),
                    key_file: PathBuf::from("/var/lib/ployz/nats/server.key"),
                },
                PathBuf::from("authorized-users.conf"),
            )
            .expect("first-node nats config is valid")
            .render(),
        }
    }

    #[must_use]
    pub fn display_path(&self) -> PathBuf {
        self.config_dir.join(&self.config_file_name)
    }

    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    #[must_use]
    pub fn config_file_name(&self) -> &str {
        &self.config_file_name
    }

    #[must_use]
    pub fn render_config(&self) -> String {
        self.rendered_config.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonEmptyRoleSet {
    roles: Vec<DaemonProcessRole>,
}

impl NonEmptyRoleSet {
    pub fn try_new(roles: Vec<DaemonProcessRole>) -> Result<Self, RoleSetError> {
        if roles.is_empty() {
            return Err(RoleSetError::Empty);
        }

        let mut unique = Vec::with_capacity(roles.len());
        for role in roles {
            if unique.contains(&role) {
                return Err(RoleSetError::Duplicate { role });
            }
            unique.push(role);
        }

        Ok(Self { roles: unique })
    }

    #[must_use]
    pub fn roles(&self) -> &[DaemonProcessRole] {
        &self.roles
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleSetError {
    Empty,
    Duplicate { role: DaemonProcessRole },
}

#[must_use]
pub fn bootstrap_script_plan(target: BootstrapScriptTarget) -> KeeperStepPlan {
    KeeperStepPlan::new(vec![
        KeeperStep::VerifyHost(HostPrerequisite::LinuxRootSystemd),
        KeeperStep::InstallArtifact(target.keeper_artifact.clone().into()),
    ])
}

#[must_use]
pub fn keeper_join_local_install_plan(target: KeeperJoinTarget) -> KeeperStepPlan {
    let mut steps = keeper_join_material_steps(&target);
    steps.extend(keeper_join_install_steps(target));
    KeeperStepPlan::new(steps)
}

#[must_use]
pub(crate) fn keeper_join_material_plan(target: &KeeperJoinTarget) -> KeeperStepPlan {
    KeeperStepPlan::new(keeper_join_material_steps(target))
}

#[must_use]
pub(crate) fn keeper_join_install_plan(target: KeeperJoinTarget) -> KeeperStepPlan {
    KeeperStepPlan::new(keeper_join_install_steps(target))
}

fn keeper_join_material_steps(target: &KeeperJoinTarget) -> Vec<KeeperStep> {
    vec![KeeperStep::StoreJoinMaterial(target.material.clone())]
}

fn keeper_join_install_steps(target: KeeperJoinTarget) -> Vec<KeeperStep> {
    let mut steps = vec![KeeperStep::InstallArtifact(
        target.ployzd_artifact.clone().into(),
    )];
    steps.push(KeeperStep::InstallArtifact(
        target.dataplane_artifacts.ebpf_bytecode.clone().into(),
    ));
    steps.push(KeeperStep::InstallArtifact(
        target.dataplane_artifacts.ebpf_ctl.clone().into(),
    ));
    steps.push(KeeperStep::WritePloyzdRoleEnvironment(
        target.role_environment.clone(),
    ));

    for role in target.roles.roles {
        let unit = SupervisorUnitTarget::PloyzdRole(role.clone());
        steps.push(KeeperStep::WriteSupervisorUnit(
            SupervisorUnitSpec::PloyzdRole {
                role,
                artifact: target.ployzd_artifact.clone(),
                environment_file: target.role_environment.file().clone(),
            },
        ));
        steps.push(KeeperStep::StartSupervisorUnit(unit));
    }

    steps
}

#[must_use]
pub fn first_node_install_plan(target: FirstNodeInstallTarget) -> KeeperStepPlan {
    let process_set = first_node_process_set(&target.node_id, target.gateway);
    let nats_server_config =
        NatsServerConfigTarget::for_first_node(target.node_id.clone(), &target.nats_server_unit);
    let mut steps = vec![
        KeeperStep::VerifyHost(HostPrerequisite::LinuxRootSystemd),
        KeeperStep::InstallArtifact(target.ployzd_artifact.clone().into()),
        KeeperStep::InstallArtifact(target.dataplane_artifacts.ebpf_bytecode.clone().into()),
        KeeperStep::InstallArtifact(target.dataplane_artifacts.ebpf_ctl.clone().into()),
        KeeperStep::InstallArtifact(target.nats_server_artifact.clone().into()),
        KeeperStep::WriteNatsServerConfig(nats_server_config),
        KeeperStep::WriteSupervisorUnit(SupervisorUnitSpec::NatsServer(target.nats_server_unit)),
        KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::NatsServer),
        KeeperStep::WritePloyzdRoleEnvironment(target.role_environment.clone()),
    ];

    for role in process_set.roles() {
        let unit = SupervisorUnitTarget::PloyzdRole(role.clone());
        steps.push(KeeperStep::WriteSupervisorUnit(
            SupervisorUnitSpec::PloyzdRole {
                role: role.clone(),
                artifact: target.ployzd_artifact.clone(),
                environment_file: target.role_environment.file().clone(),
            },
        ));
        steps.push(KeeperStep::StartSupervisorUnit(unit));
    }

    KeeperStepPlan::new(steps)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperStepFailure {
    pub step: KeeperStepLabel,
    pub reason: KeeperStepFailureReason,
    pub message: FailureMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperStepEffectError {
    StepDefault(FailureMessage),
    Explicit {
        reason: KeeperStepFailureReason,
        message: FailureMessage,
    },
}

impl KeeperStepEffectError {
    #[must_use]
    pub const fn new(reason: KeeperStepFailureReason, message: FailureMessage) -> Self {
        Self::Explicit { reason, message }
    }

    #[must_use]
    pub const fn reason(&self) -> Option<KeeperStepFailureReason> {
        match self {
            Self::StepDefault(_) => None,
            Self::Explicit { reason, .. } => Some(*reason),
        }
    }

    #[must_use]
    pub const fn message(&self) -> &FailureMessage {
        match self {
            Self::StepDefault(message) | Self::Explicit { message, .. } => message,
        }
    }
}

impl From<FailureMessage> for KeeperStepEffectError {
    fn from(message: FailureMessage) -> Self {
        Self::StepDefault(message)
    }
}

impl KeeperStepFailure {
    #[must_use]
    pub fn from_step(step: &KeeperStep, message: FailureMessage) -> Self {
        Self {
            step: KeeperStepLabel::from_step(step),
            reason: KeeperStepFailureReason::from_step(step),
            message,
        }
    }

    #[must_use]
    pub fn from_effect_error(step: &KeeperStep, error: KeeperStepEffectError) -> Self {
        Self {
            step: KeeperStepLabel::from_step(step),
            reason: error
                .reason()
                .unwrap_or_else(|| KeeperStepFailureReason::from_step(step)),
            message: error.message().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeeperStepFailureReason {
    HostPrerequisiteFailed,
    ArtifactDownloadFailed,
    ArtifactVerificationFailed,
    ArtifactInstallFailed,
    NatsConfigWriteFailed,
    RoleEnvironmentWriteFailed,
    SupervisorWriteFailed,
    SupervisorStartFailed,
    SupervisorRestartFailed,
    JoinTokenRedeemFailed,
    JoinReportFailed,
    JoinTokenConsumeFailed,
    JoinMaterialStoreFailed,
}

impl KeeperStepFailureReason {
    #[must_use]
    pub const fn from_step(step: &KeeperStep) -> Self {
        match step {
            KeeperStep::VerifyHost(_) => Self::HostPrerequisiteFailed,
            KeeperStep::InstallArtifact(_) => Self::ArtifactInstallFailed,
            KeeperStep::WritePloyzdRoleEnvironment(_) => Self::RoleEnvironmentWriteFailed,
            KeeperStep::WriteNatsServerConfig(_) => Self::NatsConfigWriteFailed,
            KeeperStep::WriteSupervisorUnit(_) => Self::SupervisorWriteFailed,
            KeeperStep::StartSupervisorUnit(_) => Self::SupervisorStartFailed,
            KeeperStep::RestartSupervisorUnit(_) => Self::SupervisorRestartFailed,
            KeeperStep::StoreJoinMaterial(_) => Self::JoinMaterialStoreFailed,
        }
    }
}
