//! Typed keeper step plans.

mod nats_material;

use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;

use ployz_core::ids::NodeId;
use ployz_core::install::{
    AbsoluteInstallPath, MachineBootstrapUrl, MachineJoinBundle, MachineJoinSecretDelivery,
    NatsMachineMaterialPaths,
};
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, FirstNodeGateway, first_node_process_set};
use ployz_nats::connect::NatsClientUrl;
use sha2::{Digest, Sha256};

use crate::artifacts::{ArtifactTarget, DataplaneArtifactTargets};
use crate::nats_identity::ClusterNatsIdentity;
use crate::systemd::{
    NatsServerUnitTarget, PloyzdRoleEnvironmentFile, SupervisorUnitSpec, SupervisorUnitTarget,
};

pub use nats_material::{
    AUTHORIZED_USERS_FILE_NAME, NatsAuthorizedUsersTarget, NatsClientCredentialsTarget,
    NatsServerConfigTarget, NatsTlsMaterialTarget, RoleNatsCredentials, RoleNatsSeedSource,
};
use nats_material::{DEFAULT_NATS_PORT, first_node_listener, tls_loopback_nats_url};

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperStep {
    VerifyHost(HostPrerequisite),
    PrepareContainerRuntime(ContainerRuntime),
    VerifyContainerRuntime(ContainerRuntime),
    InstallArtifact(ArtifactTarget),
    WritePloyzdRoleEnvironment(PloyzdRoleEnvironmentStep),
    WriteNatsTlsMaterial(NatsTlsMaterialTarget),
    WriteNatsAuthorizedUsers(NatsAuthorizedUsersTarget),
    WriteNatsClientCredentials(NatsClientCredentialsTarget),
    WriteNatsServerConfig(NatsServerConfigTarget),
    WriteSupervisorUnit(SupervisorUnitSpec),
    StartSupervisorUnit(SupervisorUnitTarget),
    RestartSupervisorUnit(SupervisorUnitTarget),
    StoreJoinMaterial(KeeperJoinMaterial),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperStepLabel {
    VerifyHost(HostPrerequisite),
    PrepareContainerRuntime(ContainerRuntime),
    VerifyContainerRuntime(ContainerRuntime),
    InstallArtifact(ArtifactTarget),
    WritePloyzdRoleEnvironment(PloyzdRoleEnvironmentStep),
    WriteNatsTlsMaterial { state_dir: PathBuf },
    WriteNatsAuthorizedUsers { path: PathBuf },
    WriteNatsClientCredentials { state_dir: PathBuf },
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
            KeeperStep::PrepareContainerRuntime(runtime) => Self::PrepareContainerRuntime(*runtime),
            KeeperStep::VerifyContainerRuntime(runtime) => Self::VerifyContainerRuntime(*runtime),
            KeeperStep::InstallArtifact(target) => Self::InstallArtifact(target.clone()),
            KeeperStep::WritePloyzdRoleEnvironment(step) => {
                Self::WritePloyzdRoleEnvironment(step.clone())
            }
            KeeperStep::WriteNatsTlsMaterial(target) => Self::WriteNatsTlsMaterial {
                state_dir: target.state_dir().to_path_buf(),
            },
            KeeperStep::WriteNatsAuthorizedUsers(target) => Self::WriteNatsAuthorizedUsers {
                path: target.display_path(),
            },
            KeeperStep::WriteNatsClientCredentials(target) => Self::WriteNatsClientCredentials {
                state_dir: target.state_dir().to_path_buf(),
            },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRuntime {
    Docker,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperJoinMaterial {
    redacted: RedactedJoinMaterial,
    nats_credentials: NatsUserSeed,
    trusted_ca_pem: NatsCaCertificatePem,
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
            secret_delivery.nats_credentials.clone(),
            join_bundle.material.trusted_nats.ca_pem.clone(),
        )
    }

    pub fn new(
        node_id: NodeId,
        cluster_name: impl Into<String>,
        nats_credentials: NatsUserSeed,
        trusted_ca_pem: NatsCaCertificatePem,
    ) -> Result<Self, JoinMaterialError> {
        let redacted = RedactedJoinMaterial::new(
            node_id,
            cluster_name,
            ca_pem_sha256(trusted_ca_pem.as_str()),
        )?;
        Ok(Self {
            redacted,
            nats_credentials,
            trusted_ca_pem,
        })
    }

    #[must_use]
    pub fn redacted(&self) -> RedactedJoinMaterial {
        self.redacted.clone()
    }

    /// The redeemed per-machine secret. `Debug` output stays redacted via
    /// the typed credential.
    #[must_use]
    pub fn nats_credentials(&self) -> &NatsUserSeed {
        &self.nats_credentials
    }

    /// The cluster CA the joined machine's roles verify TLS against.
    /// Public material; stored next to the redeemed seed.
    #[must_use]
    pub fn trusted_ca_pem(&self) -> &NatsCaCertificatePem {
        &self.trusted_ca_pem
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
    pub trusted_nats_ca_sha256: String,
}

impl RedactedJoinMaterial {
    pub fn new(
        node_id: NodeId,
        cluster_name: impl Into<String>,
        trusted_nats_ca_sha256: impl Into<String>,
    ) -> Result<Self, JoinMaterialError> {
        let cluster_name = line_value(JoinMaterialField::ClusterName, cluster_name.into())?;
        let trusted_nats_ca_sha256 = line_value(
            JoinMaterialField::TrustedNatsCaDigest,
            trusted_nats_ca_sha256.into(),
        )?;

        Ok(Self {
            node_id,
            cluster_name,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinMaterialField {
    ClusterName,
    TrustedNatsCaDigest,
}

impl JoinMaterialField {
    const fn label(self) -> &'static str {
        match self {
            Self::ClusterName => "cluster name",
            Self::TrustedNatsCaDigest => "trusted NATS CA digest",
        }
    }

    const fn empty_error(self) -> JoinMaterialError {
        match self {
            Self::ClusterName => JoinMaterialError::EmptyClusterName,
            Self::TrustedNatsCaDigest => JoinMaterialError::EmptyJoinMaterialValue {
                label: Self::TrustedNatsCaDigest.label(),
            },
        }
    }
}

fn line_value(field: JoinMaterialField, value: String) -> Result<String, JoinMaterialError> {
    if value.is_empty() {
        return Err(field.empty_error());
    }
    if value.contains(['\n', '\r', '=']) {
        return Err(JoinMaterialError::InvalidJoinMaterialValue {
            label: field.label(),
            value,
        });
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperJoinTarget {
    pub material: KeeperJoinMaterial,
    pub ployzd_artifact: ArtifactTarget,
    pub dataplane_artifacts: DataplaneArtifactTargets,
    pub roles: NonEmptyRoleSet,
    pub role_environment: PloyzdRoleEnvironmentTarget,
}

impl KeeperJoinTarget {
    #[must_use]
    pub fn new(
        material: KeeperJoinMaterial,
        ployzd_artifact: ArtifactTarget,
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
    pub ployzd_artifact: ArtifactTarget,
    pub dataplane_artifacts: DataplaneArtifactTargets,
    pub nats_server_artifact: ArtifactTarget,
    pub gateway: FirstNodeGateway,
    pub nats_identity: ClusterNatsIdentity,
    pub nats_material: NatsMachineMaterialPaths,
    pub node_public_ip: Option<IpAddr>,
    pub nats_server_unit: NatsServerUnitTarget,
    pub role_environment: PloyzdRoleEnvironmentTarget,
}

impl FirstNodeInstallTarget {
    #[must_use]
    pub fn new(
        node_id: NodeId,
        ployzd_artifact: ArtifactTarget,
        dataplane_artifacts: DataplaneArtifactTargets,
        nats_server_artifact: ArtifactTarget,
        gateway: FirstNodeGateway,
        nats_identity: ClusterNatsIdentity,
    ) -> Self {
        let nats_server_unit = NatsServerUnitTarget::new(
            nats_server_artifact.install_path().to_path_buf(),
            NatsServerUnitTarget::default_paths()
                .config_path()
                .to_path_buf(),
        )
        .expect("validated nats-server artifact install path is a valid unit path");
        let nats_material = NatsMachineMaterialPaths::in_default_state_dir();
        let role_environment = PloyzdRoleEnvironmentTarget::default_path(
            node_id.clone(),
            tls_loopback_nats_url(DEFAULT_NATS_PORT),
            RoleNatsCredentials::cluster(&nats_material),
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
            nats_identity,
            nats_material,
            node_public_ip: None,
            nats_server_unit,
            role_environment,
        }
    }

    #[must_use]
    pub fn with_nats_server_unit(mut self, nats_server_unit: NatsServerUnitTarget) -> Self {
        self.nats_server_unit = nats_server_unit;
        self
    }

    /// Relocates the NATS material state dir (tests use temp roots) and
    /// re-derives the role environment's credential paths from it.
    #[must_use]
    pub fn with_nats_material_paths(mut self, nats_material: NatsMachineMaterialPaths) -> Self {
        self.role_environment = self
            .role_environment
            .with_nats_credentials(RoleNatsCredentials::cluster(&nats_material));
        self.nats_material = nats_material;
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
        self.node_public_ip = Some(public_ip);
        self.role_environment = self.role_environment.with_node_public_ip(public_ip);
        self
    }
}

/// One per-role environment file write within a keeper plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzdRoleEnvironmentStep {
    pub role: DaemonProcessRole,
    pub target: PloyzdRoleEnvironmentTarget,
}

impl PloyzdRoleEnvironmentStep {
    #[must_use]
    pub fn file(&self) -> PloyzdRoleEnvironmentFile {
        self.target.file_for_role(&self.role)
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.target.render_for_role(&self.role)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzdRoleEnvironmentTarget {
    file: PloyzdRoleEnvironmentFile,
    node_id: NodeId,
    nats_url: NatsClientUrl,
    nats_credentials: RoleNatsCredentials,
    node_public_ip: Option<IpAddr>,
    machine_bootstrap_url: Option<MachineBootstrapUrl>,
    machine_join_template_file: Option<AbsoluteInstallPath>,
    ebpf_bytecode_path: Option<PathBuf>,
    ebpf_ctl_path: Option<PathBuf>,
}

impl PloyzdRoleEnvironmentTarget {
    #[must_use]
    pub fn new(
        file: PloyzdRoleEnvironmentFile,
        node_id: NodeId,
        nats_url: NatsClientUrl,
        nats_credentials: RoleNatsCredentials,
    ) -> Self {
        Self {
            file,
            node_id,
            nats_url,
            nats_credentials,
            node_public_ip: None,
            machine_bootstrap_url: None,
            machine_join_template_file: None,
            ebpf_bytecode_path: None,
            ebpf_ctl_path: None,
        }
    }

    #[must_use]
    pub fn default_path(
        node_id: NodeId,
        nats_url: NatsClientUrl,
        nats_credentials: RoleNatsCredentials,
    ) -> Self {
        Self::new(
            PloyzdRoleEnvironmentFile::default_path(),
            node_id,
            nats_url,
            nats_credentials,
        )
    }

    #[must_use]
    pub const fn file(&self) -> &PloyzdRoleEnvironmentFile {
        &self.file
    }

    /// The per-role environment file derived from the base path:
    /// `/etc/ployz/ployzd.env` becomes `/etc/ployz/ployzd-control.env`.
    #[must_use]
    pub fn file_for_role(&self, role: &DaemonProcessRole) -> PloyzdRoleEnvironmentFile {
        let base = self.file.path();
        let parent = base
            .parent()
            .expect("validated ployzd role env path has a directory");
        let stem = base
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("validated ployzd role env path has a UTF-8 file stem");
        let file_name = match base.extension().and_then(|extension| extension.to_str()) {
            Some(extension) => format!("{stem}-{}.{extension}", role.process_name()),
            None => format!("{stem}-{}", role.process_name()),
        };
        PloyzdRoleEnvironmentFile::new(parent.join(file_name))
            .expect("per-role env path derived from a validated base path is valid")
    }

    #[must_use]
    pub fn with_nats_credentials(mut self, nats_credentials: RoleNatsCredentials) -> Self {
        self.nats_credentials = nats_credentials;
        self
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
    pub fn render_for_role(&self, role: &DaemonProcessRole) -> String {
        let mut output = format!("PLOYZ_NATS_URL={}\n", self.nats_url.as_str());
        output.push_str("PLOYZ_NATS_CA_FILE=");
        output.push_str(&self.nats_credentials.ca_file().display().to_string());
        output.push('\n');
        output.push_str("PLOYZ_NATS_NKEY_SEED_FILE=");
        output.push_str(
            &self
                .nats_credentials
                .seed_file_for_role(role)
                .display()
                .to_string(),
        );
        output.push('\n');
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
    let mut steps = vec![
        KeeperStep::PrepareContainerRuntime(ContainerRuntime::Docker),
        KeeperStep::VerifyContainerRuntime(ContainerRuntime::Docker),
        KeeperStep::InstallArtifact(target.ployzd_artifact.clone()),
    ];
    steps.push(KeeperStep::InstallArtifact(
        target.dataplane_artifacts.ebpf_bytecode.clone(),
    ));
    steps.push(KeeperStep::InstallArtifact(
        target.dataplane_artifacts.ebpf_ctl.clone(),
    ));

    for role in target.roles.roles {
        let unit = SupervisorUnitTarget::PloyzdRole(role.clone());
        steps.push(KeeperStep::WritePloyzdRoleEnvironment(
            PloyzdRoleEnvironmentStep {
                role: role.clone(),
                target: target.role_environment.clone(),
            },
        ));
        steps.push(KeeperStep::WriteSupervisorUnit(
            SupervisorUnitSpec::PloyzdRole {
                role: role.clone(),
                artifact: target.ployzd_artifact.clone(),
                environment_file: target.role_environment.file_for_role(&role),
            },
        ));
        steps.push(KeeperStep::StartSupervisorUnit(unit));
    }

    steps
}

#[must_use]
pub fn first_node_install_plan(target: FirstNodeInstallTarget) -> KeeperStepPlan {
    let process_set = first_node_process_set(&target.node_id, target.gateway);
    let nats_server_config = NatsServerConfigTarget::for_first_node(
        target.node_id.clone(),
        &target.nats_server_unit,
        &target.nats_material,
        first_node_listener(target.node_public_ip),
    );
    let mut steps = vec![
        KeeperStep::VerifyHost(HostPrerequisite::LinuxRootSystemd),
        KeeperStep::PrepareContainerRuntime(ContainerRuntime::Docker),
        KeeperStep::VerifyContainerRuntime(ContainerRuntime::Docker),
        KeeperStep::InstallArtifact(target.ployzd_artifact.clone()),
        KeeperStep::InstallArtifact(target.dataplane_artifacts.ebpf_bytecode.clone()),
        KeeperStep::InstallArtifact(target.dataplane_artifacts.ebpf_ctl.clone()),
        KeeperStep::InstallArtifact(target.nats_server_artifact.clone()),
        KeeperStep::WriteNatsTlsMaterial(NatsTlsMaterialTarget::new(
            target.nats_material.clone(),
            &target.nats_identity,
        )),
        KeeperStep::WriteNatsAuthorizedUsers(NatsAuthorizedUsersTarget::initial_for_first_node(
            nats_server_config.config_dir().to_path_buf(),
            &target.nats_identity,
        )),
        KeeperStep::WriteNatsClientCredentials(NatsClientCredentialsTarget::new(
            target.nats_material.clone(),
            &target.nats_identity,
        )),
        KeeperStep::WriteNatsServerConfig(nats_server_config),
        KeeperStep::WriteSupervisorUnit(SupervisorUnitSpec::NatsServer(target.nats_server_unit)),
        KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::NatsServer),
    ];

    for role in process_set.roles() {
        let unit = SupervisorUnitTarget::PloyzdRole(role.clone());
        steps.push(KeeperStep::WritePloyzdRoleEnvironment(
            PloyzdRoleEnvironmentStep {
                role: role.clone(),
                target: target.role_environment.clone(),
            },
        ));
        steps.push(KeeperStep::WriteSupervisorUnit(
            SupervisorUnitSpec::PloyzdRole {
                role: role.clone(),
                artifact: target.ployzd_artifact.clone(),
                environment_file: target.role_environment.file_for_role(role),
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
    NatsTlsMaterialWriteFailed,
    NatsAuthorizedUsersWriteFailed,
    NatsClientCredentialsWriteFailed,
    RoleEnvironmentWriteFailed,
    SupervisorWriteFailed,
    SupervisorStartFailed,
    SupervisorRestartFailed,
    JoinTokenRedeemFailed,
    JoinReportFailed,
    JoinTokenConsumeFailed,
    JoinMaterialStoreFailed,
    ContainerRuntimePrepareFailed,
    ContainerRuntimeVerifyFailed,
}

impl KeeperStepFailureReason {
    #[must_use]
    pub const fn from_step(step: &KeeperStep) -> Self {
        match step {
            KeeperStep::VerifyHost(_) => Self::HostPrerequisiteFailed,
            KeeperStep::PrepareContainerRuntime(_) => Self::ContainerRuntimePrepareFailed,
            KeeperStep::VerifyContainerRuntime(_) => Self::ContainerRuntimeVerifyFailed,
            KeeperStep::InstallArtifact(_) => Self::ArtifactInstallFailed,
            KeeperStep::WritePloyzdRoleEnvironment(_) => Self::RoleEnvironmentWriteFailed,
            KeeperStep::WriteNatsTlsMaterial(_) => Self::NatsTlsMaterialWriteFailed,
            KeeperStep::WriteNatsAuthorizedUsers(_) => Self::NatsAuthorizedUsersWriteFailed,
            KeeperStep::WriteNatsClientCredentials(_) => Self::NatsClientCredentialsWriteFailed,
            KeeperStep::WriteNatsServerConfig(_) => Self::NatsConfigWriteFailed,
            KeeperStep::WriteSupervisorUnit(_) => Self::SupervisorWriteFailed,
            KeeperStep::StartSupervisorUnit(_) => Self::SupervisorStartFailed,
            KeeperStep::RestartSupervisorUnit(_) => Self::SupervisorRestartFailed,
            KeeperStep::StoreJoinMaterial(_) => Self::JoinMaterialStoreFailed,
        }
    }
}
