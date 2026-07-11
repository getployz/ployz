//! Typed Host Runner step plans.

mod nats_material;

use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;

use ployz_core::dataplane::MachineEndpointSupernet;
use ployz_core::ids::MachineId;
use ployz_core::install::{
    AbsoluteInstallPath, MachineBootstrapUrl, MachineJoinBundle, MachineJoinClusterName,
    MachineJoinMaterial, MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTemplate,
    MachineJoinTrustedNats, NatsMachineMaterialPaths, WrappedCaKey, WrappedCoreSeeds,
};
use ployz_core::nats_config::{
    CredentialGrant, CredentialName, NatsCaCertificatePem, NatsUserSeed,
};
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, InstallRolePolicy, plan_first_machine_process_set};
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
use nats_material::{DEFAULT_NATS_PORT, first_machine_listener, tls_loopback_nats_url};

const PLOYZ_MACHINE_ID_ENV: &str = "PLOYZ_MACHINE_ID";
const PLOYZ_GATEWAY_LISTEN_ADDR_ENV: &str = "PLOYZ_GATEWAY_LISTEN_ADDR";
const PLOYZ_JOIN_NKEY_SEED_FILE_ENV: &str = "PLOYZ_JOIN_NKEY_SEED_FILE";
const PLOYZ_SEED_FROM_MIRROR_ENV: &str = "PLOYZ_SEED_FROM_MIRROR";
const PLOYZ_DATAPLANE_ENDPOINT_SUPERNET_ENV: &str = "PLOYZ_DATAPLANE_ENDPOINT_SUPERNET";
const DEFAULT_GATEWAY_LISTEN_ADDR: &str = "0.0.0.0:80";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerStepPlan {
    steps: Vec<HostRunnerStep>,
}

impl HostRunnerStepPlan {
    #[must_use]
    fn new(steps: Vec<HostRunnerStep>) -> Self {
        Self { steps }
    }

    #[must_use]
    pub fn from_steps(steps: Vec<HostRunnerStep>) -> Self {
        Self::new(steps)
    }

    #[must_use]
    pub fn steps(&self) -> &[HostRunnerStep] {
        &self.steps
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRunnerStep {
    VerifyHost(HostPrerequisite),
    PrepareDataplaneHost,
    PrepareContainerRuntime(ContainerRuntime, MachineEndpointSupernet),
    VerifyContainerRuntime(ContainerRuntime),
    InstallArtifact(ArtifactTarget),
    WritePloyzdRoleEnvironment(PloyzdRoleEnvironmentStep),
    WriteNatsTlsMaterial(NatsTlsMaterialTarget),
    WriteNatsAuthorizedUsers(NatsAuthorizedUsersTarget),
    WriteNatsClientCredentials(NatsClientCredentialsTarget),
    WriteNatsServerConfig(NatsServerConfigTarget),
    WriteMachineJoinTemplate(MachineJoinTemplateTarget),
    WriteSupervisorUnit(SupervisorUnitSpec),
    StartSupervisorUnit(SupervisorUnitTarget),
    RestartSupervisorUnit(SupervisorUnitTarget),
    StoreJoinMaterial(HostRunnerJoinMaterial),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRunnerStepLabel {
    VerifyHost(HostPrerequisite),
    PrepareDataplaneHost,
    PrepareContainerRuntime(ContainerRuntime),
    VerifyContainerRuntime(ContainerRuntime),
    InstallArtifact(ArtifactTarget),
    WritePloyzdRoleEnvironment(PloyzdRoleEnvironmentStep),
    WriteNatsTlsMaterial { state_dir: PathBuf },
    WriteNatsAuthorizedUsers { path: PathBuf },
    WriteNatsClientCredentials { state_dir: PathBuf },
    WriteNatsServerConfig(NatsServerConfigTarget),
    WriteMachineJoinTemplate { path: PathBuf },
    WriteSupervisorUnit(SupervisorUnitTarget),
    StartSupervisorUnit(SupervisorUnitTarget),
    RestartSupervisorUnit(SupervisorUnitTarget),
    RedeemJoinToken,
    ReportJoinResult,
    ConsumeJoinTokenFile,
    StoreJoinMaterial(RedactedJoinMaterial),
}

impl HostRunnerStepLabel {
    #[must_use]
    pub fn from_step(step: &HostRunnerStep) -> Self {
        match step {
            HostRunnerStep::VerifyHost(prerequisite) => Self::VerifyHost(*prerequisite),
            HostRunnerStep::PrepareDataplaneHost => Self::PrepareDataplaneHost,
            HostRunnerStep::PrepareContainerRuntime(runtime, _) => {
                Self::PrepareContainerRuntime(*runtime)
            }
            HostRunnerStep::VerifyContainerRuntime(runtime) => {
                Self::VerifyContainerRuntime(*runtime)
            }
            HostRunnerStep::InstallArtifact(target) => Self::InstallArtifact(target.clone()),
            HostRunnerStep::WritePloyzdRoleEnvironment(step) => {
                Self::WritePloyzdRoleEnvironment(step.clone())
            }
            HostRunnerStep::WriteNatsTlsMaterial(target) => Self::WriteNatsTlsMaterial {
                state_dir: target.state_dir().to_path_buf(),
            },
            HostRunnerStep::WriteNatsAuthorizedUsers(target) => Self::WriteNatsAuthorizedUsers {
                path: target.display_path(),
            },
            HostRunnerStep::WriteNatsClientCredentials(target) => {
                Self::WriteNatsClientCredentials {
                    state_dir: target.state_dir().to_path_buf(),
                }
            }
            HostRunnerStep::WriteNatsServerConfig(target) => {
                Self::WriteNatsServerConfig(target.clone())
            }
            HostRunnerStep::WriteMachineJoinTemplate(target) => Self::WriteMachineJoinTemplate {
                path: target.path(),
            },
            HostRunnerStep::WriteSupervisorUnit(spec) => Self::WriteSupervisorUnit(spec.target()),
            HostRunnerStep::StartSupervisorUnit(target) => {
                Self::StartSupervisorUnit(target.clone())
            }
            HostRunnerStep::RestartSupervisorUnit(target) => {
                Self::RestartSupervisorUnit(target.clone())
            }
            HostRunnerStep::StoreJoinMaterial(material) => {
                Self::StoreJoinMaterial(material.redacted())
            }
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
pub struct HostRunnerJoinMaterial {
    redacted: RedactedJoinMaterial,
    nats_credentials: NatsUserSeed,
    trusted_ca_pem: NatsCaCertificatePem,
    recovery_key_wrapped: WrappedCaKey,
    core_seeds_wrapped: WrappedCoreSeeds,
    dataplane_endpoint_supernet: MachineEndpointSupernet,
}

impl HostRunnerJoinMaterial {
    pub fn from_join_payload(
        machine_id: MachineId,
        join_bundle: &MachineJoinBundle,
        secret_delivery: &MachineJoinSecretDelivery,
    ) -> Result<Self, JoinMaterialError> {
        Self::new_with_supernet(
            machine_id,
            join_bundle.material.cluster_name.as_str(),
            secret_delivery.nats_credentials.clone(),
            join_bundle.material.trusted_nats.ca_pem.clone(),
            join_bundle.material.recovery_key_wrapped.clone(),
            join_bundle.material.core_seeds_wrapped.clone(),
            join_bundle.material.dataplane_endpoint_supernet.clone(),
        )
    }

    pub fn new(
        machine_id: MachineId,
        cluster_name: impl Into<String>,
        nats_credentials: NatsUserSeed,
        trusted_ca_pem: NatsCaCertificatePem,
        recovery_key_wrapped: WrappedCaKey,
        core_seeds_wrapped: WrappedCoreSeeds,
    ) -> Result<Self, JoinMaterialError> {
        Self::new_with_supernet(
            machine_id,
            cluster_name,
            nats_credentials,
            trusted_ca_pem,
            recovery_key_wrapped,
            core_seeds_wrapped,
            MachineEndpointSupernet::default_v1(),
        )
    }

    pub fn new_with_supernet(
        machine_id: MachineId,
        cluster_name: impl Into<String>,
        nats_credentials: NatsUserSeed,
        trusted_ca_pem: NatsCaCertificatePem,
        recovery_key_wrapped: WrappedCaKey,
        core_seeds_wrapped: WrappedCoreSeeds,
        dataplane_endpoint_supernet: MachineEndpointSupernet,
    ) -> Result<Self, JoinMaterialError> {
        let redacted = RedactedJoinMaterial::new_with_supernet(
            machine_id,
            cluster_name,
            ca_pem_sha256(trusted_ca_pem.as_str()),
            dataplane_endpoint_supernet.clone(),
        )?;
        Ok(Self {
            redacted,
            nats_credentials,
            trusted_ca_pem,
            recovery_key_wrapped,
            core_seeds_wrapped,
            dataplane_endpoint_supernet,
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

    /// The wrapped cluster CA signing key (ADR 0031). Written 0600 so this
    /// candidate can be core-promoted.
    #[must_use]
    pub fn recovery_key_wrapped(&self) -> &WrappedCaKey {
        &self.recovery_key_wrapped
    }

    /// The wrapped core principal seeds (ADR 0031). Written 0600 so a promoted
    /// core reuses the old core's principals verbatim.
    #[must_use]
    pub fn core_seeds_wrapped(&self) -> &WrappedCoreSeeds {
        &self.core_seeds_wrapped
    }

    #[must_use]
    pub const fn dataplane_endpoint_supernet(&self) -> &MachineEndpointSupernet {
        &self.dataplane_endpoint_supernet
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
    pub machine_id: MachineId,
    pub cluster_name: String,
    pub trusted_nats_ca_sha256: String,
    pub dataplane_endpoint_supernet: MachineEndpointSupernet,
}

impl RedactedJoinMaterial {
    pub fn new(
        machine_id: MachineId,
        cluster_name: impl Into<String>,
        trusted_nats_ca_sha256: impl Into<String>,
    ) -> Result<Self, JoinMaterialError> {
        Self::new_with_supernet(
            machine_id,
            cluster_name,
            trusted_nats_ca_sha256,
            MachineEndpointSupernet::default_v1(),
        )
    }

    pub fn new_with_supernet(
        machine_id: MachineId,
        cluster_name: impl Into<String>,
        trusted_nats_ca_sha256: impl Into<String>,
        dataplane_endpoint_supernet: MachineEndpointSupernet,
    ) -> Result<Self, JoinMaterialError> {
        let cluster_name = line_value(JoinMaterialField::ClusterName, cluster_name.into())?;
        let trusted_nats_ca_sha256 = line_value(
            JoinMaterialField::TrustedNatsCaDigest,
            trusted_nats_ca_sha256.into(),
        )?;

        Ok(Self {
            machine_id,
            cluster_name,
            trusted_nats_ca_sha256,
            dataplane_endpoint_supernet,
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
pub struct HostRunnerJoinTarget {
    pub material: HostRunnerJoinMaterial,
    pub ployzd_artifact: ArtifactTarget,
    pub dataplane_artifacts: DataplaneArtifactTargets,
    pub roles: NonEmptyRoleSet,
    pub role_environment: PloyzdRoleEnvironmentTarget,
}

impl HostRunnerJoinTarget {
    #[must_use]
    pub fn new(
        material: HostRunnerJoinMaterial,
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
pub struct FirstMachineInstallTarget {
    pub machine_id: MachineId,
    pub dataplane_endpoint_supernet: MachineEndpointSupernet,
    pub ployzd_artifact: ArtifactTarget,
    pub dataplane_artifacts: DataplaneArtifactTargets,
    pub nats_server_artifact: ArtifactTarget,
    pub roles: InstallRolePolicy,
    pub nats_identity: ClusterNatsIdentity,
    /// The CA signing key wrapped with the recovery secret (ADR 0031), persisted
    /// beside the TLS material so a promotion can decrypt and self-issue.
    pub recovery_key_wrapped: WrappedCaKey,
    /// The core principal seeds wrapped with the recovery secret (ADR 0031),
    /// pre-positioned so a promotion reuses them instead of rotating.
    pub core_seeds_wrapped: WrappedCoreSeeds,
    pub nats_material: NatsMachineMaterialPaths,
    pub founder_credential_name: CredentialName,
    pub additional_credentials: Vec<CredentialGrant>,
    pub machine_public_ip: Option<IpAddr>,
    pub nats_server_unit: NatsServerUnitTarget,
    pub role_environment: PloyzdRoleEnvironmentTarget,
    machine_join_template_file: Option<AbsoluteInstallPath>,
    machine_join_cluster_name: MachineJoinClusterName,
    machine_join_runtime_nats_url: MachineJoinRuntimeNatsUrl,
}

impl FirstMachineInstallTarget {
    // ponytail: the distinct first-machine install inputs — ids, artifacts, identity,
    // and the two wrapped recovery blobs. Bundling them would be a sparse option bag.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        machine_id: MachineId,
        ployzd_artifact: ArtifactTarget,
        dataplane_artifacts: DataplaneArtifactTargets,
        nats_server_artifact: ArtifactTarget,
        roles: InstallRolePolicy,
        nats_identity: ClusterNatsIdentity,
        recovery_key_wrapped: WrappedCaKey,
        core_seeds_wrapped: WrappedCoreSeeds,
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
            machine_id.clone(),
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
        let dataplane_endpoint_supernet = MachineEndpointSupernet::default_v1();
        let role_environment =
            role_environment.with_dataplane_endpoint_supernet(dataplane_endpoint_supernet.clone());
        let founder_credential_name =
            CredentialName::try_new(format!("Founder operator ({})", machine_id.as_str()))
                .expect("machine id forms a non-empty founder credential name");
        Self {
            machine_id,
            dataplane_endpoint_supernet,
            ployzd_artifact,
            dataplane_artifacts,
            nats_server_artifact,
            roles,
            nats_identity,
            recovery_key_wrapped,
            core_seeds_wrapped,
            nats_material,
            founder_credential_name,
            additional_credentials: Vec::new(),
            machine_public_ip: None,
            nats_server_unit,
            role_environment,
            machine_join_template_file: None,
            machine_join_cluster_name: MachineJoinClusterName::try_new("ployz")
                .expect("default cluster name is valid"),
            machine_join_runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(format!(
                "tls://127.0.0.1:{DEFAULT_NATS_PORT}"
            ))
            .expect("default runtime NATS URL is valid"),
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
    pub fn with_founder_hostname(mut self, hostname: impl AsRef<str>) -> Self {
        self.founder_credential_name =
            CredentialName::try_new(format!("Founder operator ({})", hostname.as_ref()))
                .expect("hostname forms a non-empty founder credential name");
        self
    }

    #[must_use]
    pub fn with_additional_credential(mut self, grant: CredentialGrant) -> Self {
        self.additional_credentials.push(grant);
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
        self.machine_join_template_file = Some(path.clone());
        self.role_environment = self.role_environment.with_machine_join_template_file(path);
        self
    }

    #[must_use]
    pub fn with_machine_join_cluster_name(mut self, cluster_name: MachineJoinClusterName) -> Self {
        self.machine_join_cluster_name = cluster_name;
        self
    }

    #[must_use]
    pub fn with_machine_join_runtime_nats_url(
        mut self,
        runtime_nats_url: MachineJoinRuntimeNatsUrl,
    ) -> Self {
        self.machine_join_runtime_nats_url = runtime_nats_url;
        self
    }

    #[must_use]
    pub const fn machine_join_runtime_nats_url(&self) -> &MachineJoinRuntimeNatsUrl {
        &self.machine_join_runtime_nats_url
    }

    #[must_use]
    pub fn with_machine_public_ip(mut self, public_ip: IpAddr) -> Self {
        self.machine_public_ip = Some(public_ip);
        self
    }

    #[must_use]
    pub fn with_dataplane_endpoint_supernet(
        mut self,
        dataplane_endpoint_supernet: MachineEndpointSupernet,
    ) -> Self {
        self.role_environment = self
            .role_environment
            .with_dataplane_endpoint_supernet(dataplane_endpoint_supernet.clone());
        self.dataplane_endpoint_supernet = dataplane_endpoint_supernet;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineJoinTemplateTarget {
    path: AbsoluteInstallPath,
    template: MachineJoinTemplate,
}

impl MachineJoinTemplateTarget {
    #[must_use]
    pub const fn new(path: AbsoluteInstallPath, template: MachineJoinTemplate) -> Self {
        Self { path, template }
    }

    #[must_use]
    pub fn path(&self) -> PathBuf {
        PathBuf::from(self.path.as_str())
    }

    #[must_use]
    pub fn render(&self) -> String {
        serde_json::to_string_pretty(&self.template).expect("machine join template serializes")
    }
}

/// One per-role environment file write within a Host Runner plan.
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
    machine_id: MachineId,
    nats_url: NatsClientUrl,
    nats_credentials: RoleNatsCredentials,
    machine_bootstrap_url: Option<MachineBootstrapUrl>,
    machine_join_template_file: Option<AbsoluteInstallPath>,
    ebpf_bytecode_path: Option<PathBuf>,
    ebpf_ctl_path: Option<PathBuf>,
    seed_from_mirror: Option<PathBuf>,
    dataplane_endpoint_supernet: Option<MachineEndpointSupernet>,
}

impl PloyzdRoleEnvironmentTarget {
    #[must_use]
    pub fn new(
        file: PloyzdRoleEnvironmentFile,
        machine_id: MachineId,
        nats_url: NatsClientUrl,
        nats_credentials: RoleNatsCredentials,
    ) -> Self {
        Self {
            file,
            machine_id,
            nats_url,
            nats_credentials,
            machine_bootstrap_url: None,
            machine_join_template_file: None,
            ebpf_bytecode_path: None,
            ebpf_ctl_path: None,
            seed_from_mirror: None,
            dataplane_endpoint_supernet: None,
        }
    }

    #[must_use]
    pub fn default_path(
        machine_id: MachineId,
        nats_url: NatsClientUrl,
        nats_credentials: RoleNatsCredentials,
    ) -> Self {
        Self::new(
            PloyzdRoleEnvironmentFile::default_path(),
            machine_id,
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

    /// Point the Control process at a promotion mirror to seed a fresh core store
    /// from at startup (ADR 0031); rendered as `PLOYZ_SEED_FROM_MIRROR` for the
    /// Control role only.
    #[must_use]
    pub fn with_seed_from_mirror(mut self, path: PathBuf) -> Self {
        self.seed_from_mirror = Some(path);
        self
    }

    #[must_use]
    pub fn with_dataplane_endpoint_supernet(
        mut self,
        dataplane_endpoint_supernet: MachineEndpointSupernet,
    ) -> Self {
        self.dataplane_endpoint_supernet = Some(dataplane_endpoint_supernet);
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
        output.push_str(PLOYZ_MACHINE_ID_ENV);
        output.push('=');
        output.push_str(self.machine_id.as_str());
        output.push('\n');
        if matches!(role, DaemonProcessRole::Control)
            && let Some(path) = self.nats_credentials.join_seed_file()
        {
            output.push_str(PLOYZ_JOIN_NKEY_SEED_FILE_ENV);
            output.push('=');
            output.push_str(&path.display().to_string());
            output.push('\n');
        }
        if matches!(role, DaemonProcessRole::Control)
            && let Some(path) = &self.seed_from_mirror
        {
            output.push_str(PLOYZ_SEED_FROM_MIRROR_ENV);
            output.push('=');
            output.push_str(&path.display().to_string());
            output.push('\n');
        }
        if matches!(role, DaemonProcessRole::Control)
            && let Some(supernet) = &self.dataplane_endpoint_supernet
        {
            output.push_str(PLOYZ_DATAPLANE_ENDPOINT_SUPERNET_ENV);
            output.push('=');
            output.push_str(&supernet.as_string());
            output.push('\n');
        }
        if matches!(role, DaemonProcessRole::Gateway) {
            output.push_str(PLOYZ_GATEWAY_LISTEN_ADDR_ENV);
            output.push('=');
            output.push_str(DEFAULT_GATEWAY_LISTEN_ADDR);
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
pub fn host_runner_join_local_install_plan(target: HostRunnerJoinTarget) -> HostRunnerStepPlan {
    let mut steps = host_runner_join_material_steps(&target);
    steps.extend(host_runner_join_install_steps(target));
    HostRunnerStepPlan::new(steps)
}

#[must_use]
pub(crate) fn host_runner_join_material_plan(target: &HostRunnerJoinTarget) -> HostRunnerStepPlan {
    HostRunnerStepPlan::new(host_runner_join_material_steps(target))
}

#[must_use]
pub(crate) fn host_runner_join_install_plan(target: HostRunnerJoinTarget) -> HostRunnerStepPlan {
    HostRunnerStepPlan::new(host_runner_join_install_steps(target))
}

fn host_runner_join_material_steps(target: &HostRunnerJoinTarget) -> Vec<HostRunnerStep> {
    vec![HostRunnerStep::StoreJoinMaterial(target.material.clone())]
}

fn host_runner_join_install_steps(target: HostRunnerJoinTarget) -> Vec<HostRunnerStep> {
    let mut steps = vec![
        HostRunnerStep::PrepareDataplaneHost,
        HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            target.material.dataplane_endpoint_supernet().clone(),
        ),
        HostRunnerStep::VerifyContainerRuntime(ContainerRuntime::Docker),
        HostRunnerStep::InstallArtifact(target.ployzd_artifact.clone()),
    ];
    steps.push(HostRunnerStep::InstallArtifact(
        target.dataplane_artifacts.ebpf_bytecode.clone(),
    ));
    steps.push(HostRunnerStep::InstallArtifact(
        target.dataplane_artifacts.ebpf_ctl.clone(),
    ));

    for role in target.roles.roles {
        let unit = SupervisorUnitTarget::PloyzdRole(role.clone());
        steps.push(HostRunnerStep::WritePloyzdRoleEnvironment(
            PloyzdRoleEnvironmentStep {
                role: role.clone(),
                target: target.role_environment.clone(),
            },
        ));
        steps.push(HostRunnerStep::WriteSupervisorUnit(
            SupervisorUnitSpec::PloyzdRole {
                role: role.clone(),
                artifact: target.ployzd_artifact.clone(),
                environment_file: target.role_environment.file_for_role(&role),
            },
        ));
        steps.push(HostRunnerStep::StartSupervisorUnit(unit));
    }

    steps
}

/// Everything `core-promote` needs to render + start a replacement core on an
/// already-joined machine (ADR 0031). The identity keeps the fleet's CA and reuses
/// the old core's principals from their pre-positioned seeds; the full grant set is
/// seeded into the store from the local mirror by control at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorePromoteTarget {
    pub machine_id: MachineId,
    pub dataplane_endpoint_supernet: MachineEndpointSupernet,
    pub nats_server_artifact: ArtifactTarget,
    pub ployzd_artifact: ArtifactTarget,
    pub dataplane_artifacts: DataplaneArtifactTargets,
    pub nats_identity: ClusterNatsIdentity,
    pub recovery_key_wrapped: WrappedCaKey,
    pub core_seeds_wrapped: WrappedCoreSeeds,
    pub nats_material: NatsMachineMaterialPaths,
    pub machine_public_ip: Option<IpAddr>,
    pub nats_server_unit: NatsServerUnitTarget,
    pub role_environment: PloyzdRoleEnvironmentTarget,
    pub machine_join_template_file: AbsoluteInstallPath,
    pub machine_join_cluster_name: MachineJoinClusterName,
    pub machine_join_runtime_nats_url: MachineJoinRuntimeNatsUrl,
}

impl CorePromoteTarget {
    /// Assemble a promote target from the resolved inputs, constructing the core
    /// material paths, the `nats-server` unit, and the Control role environment
    /// (pointed at `seed_from_mirror`) the same way first-machine install does.
    // ponytail: the distinct resolved promotion inputs; bundling into an intermediate
    // struct just for the lint would be a sparse option bag.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        machine_id: MachineId,
        nats_server_artifact: ArtifactTarget,
        ployzd_artifact: ArtifactTarget,
        dataplane_artifacts: DataplaneArtifactTargets,
        nats_identity: ClusterNatsIdentity,
        recovery_key_wrapped: WrappedCaKey,
        core_seeds_wrapped: WrappedCoreSeeds,
        dataplane_endpoint_supernet: MachineEndpointSupernet,
        machine_public_ip: Option<IpAddr>,
        seed_from_mirror: PathBuf,
        machine_join_template_file: AbsoluteInstallPath,
        machine_join_cluster_name: MachineJoinClusterName,
        machine_join_runtime_nats_url: MachineJoinRuntimeNatsUrl,
    ) -> Self {
        let nats_material = NatsMachineMaterialPaths::in_default_state_dir();
        let nats_server_unit = NatsServerUnitTarget::new(
            nats_server_artifact.install_path().to_path_buf(),
            NatsServerUnitTarget::default_paths()
                .config_path()
                .to_path_buf(),
        )
        .expect("validated nats-server artifact install path is a valid unit path");
        let role_environment = PloyzdRoleEnvironmentTarget::default_path(
            machine_id.clone(),
            tls_loopback_nats_url(DEFAULT_NATS_PORT),
            RoleNatsCredentials::cluster(&nats_material),
        )
        .with_seed_from_mirror(seed_from_mirror)
        .with_dataplane_endpoint_supernet(dataplane_endpoint_supernet.clone())
        .with_machine_join_template_file(machine_join_template_file.clone());
        Self {
            machine_id,
            dataplane_endpoint_supernet,
            nats_server_artifact,
            ployzd_artifact,
            dataplane_artifacts,
            nats_identity,
            recovery_key_wrapped,
            core_seeds_wrapped,
            nats_material,
            machine_public_ip,
            nats_server_unit,
            role_environment,
            machine_join_template_file,
            machine_join_cluster_name,
            machine_join_runtime_nats_url,
        }
    }
}

/// The promotion plan: install `nats-server`, write the TLS material (self-issued
/// cert under the kept CA) + a bootstrap authorized-users of just the reused core
/// principals (enough for nats-server to start and control to connect) + the reused
/// core credentials + the server config, start the core `nats-server`, then add the
/// `Control` process (its env points `PLOYZ_SEED_FROM_MIRROR` at the local mirror,
/// so control seeds the full grant set — machines, operator, Cloud — into the store
/// and renders it). The machine keeps its existing Machine/dataplane units — this
/// plan only adds the core, so it does not re-install ployzd/ebpf.
#[must_use]
pub fn core_promote_plan(target: CorePromoteTarget) -> HostRunnerStepPlan {
    let nats_server_config = NatsServerConfigTarget::for_first_machine(
        target.machine_id.clone(),
        &target.nats_server_unit,
        &target.nats_material,
        first_machine_listener(target.machine_public_ip),
    );
    let control = DaemonProcessRole::Control;
    HostRunnerStepPlan::new(vec![
        HostRunnerStep::VerifyHost(HostPrerequisite::LinuxRootSystemd),
        HostRunnerStep::InstallArtifact(target.nats_server_artifact.clone()),
        HostRunnerStep::WriteNatsTlsMaterial(NatsTlsMaterialTarget::new(
            target.nats_material.clone(),
            &target.nats_identity,
            target.recovery_key_wrapped.clone(),
            target.core_seeds_wrapped.clone(),
        )),
        HostRunnerStep::WriteNatsAuthorizedUsers(
            NatsAuthorizedUsersTarget::initial_for_first_machine(
                nats_server_config.config_dir().to_path_buf(),
                &target.nats_identity,
                &CredentialName::try_new(format!(
                    "Founder operator ({})",
                    target.machine_id.as_str()
                ))
                .expect("machine id forms a non-empty founder credential name"),
                &[],
            ),
        ),
        HostRunnerStep::WriteNatsClientCredentials(NatsClientCredentialsTarget::new(
            target.nats_material.clone(),
            &target.nats_identity,
        )),
        HostRunnerStep::WriteNatsServerConfig(nats_server_config),
        HostRunnerStep::WriteSupervisorUnit(SupervisorUnitSpec::NatsServer(
            target.nats_server_unit,
        )),
        HostRunnerStep::StartSupervisorUnit(SupervisorUnitTarget::NatsServer),
        HostRunnerStep::WriteMachineJoinTemplate(MachineJoinTemplateTarget::new(
            target.machine_join_template_file,
            MachineJoinTemplate {
                join_bundle: MachineJoinBundle {
                    material: MachineJoinMaterial {
                        cluster_name: target.machine_join_cluster_name,
                        dataplane_endpoint_supernet: target.dataplane_endpoint_supernet.clone(),
                        runtime_nats_url: target.machine_join_runtime_nats_url,
                        trusted_nats: MachineJoinTrustedNats {
                            ca_pem: target.nats_identity.ca.clone(),
                        },
                        recovery_key_wrapped: target.recovery_key_wrapped.clone(),
                        core_seeds_wrapped: target.core_seeds_wrapped.clone(),
                        ployzd: target.ployzd_artifact.install_spec(),
                        ebpf_bytecode: target.dataplane_artifacts.ebpf_bytecode.install_spec(),
                        ebpf_ctl: target.dataplane_artifacts.ebpf_ctl.install_spec(),
                    },
                },
            },
        )),
        HostRunnerStep::WritePloyzdRoleEnvironment(PloyzdRoleEnvironmentStep {
            role: control.clone(),
            target: target.role_environment.clone(),
        }),
        HostRunnerStep::WriteSupervisorUnit(SupervisorUnitSpec::PloyzdRole {
            role: control.clone(),
            artifact: target.ployzd_artifact.clone(),
            environment_file: target.role_environment.file_for_role(&control),
        }),
        HostRunnerStep::StartSupervisorUnit(SupervisorUnitTarget::PloyzdRole(control)),
    ])
}

#[must_use]
pub fn first_machine_install_plan(target: FirstMachineInstallTarget) -> HostRunnerStepPlan {
    let process_set = plan_first_machine_process_set(&target.machine_id, target.roles);
    let role_environment = target
        .role_environment
        .clone()
        .with_dataplane_endpoint_supernet(target.dataplane_endpoint_supernet.clone());
    let nats_server_config = NatsServerConfigTarget::for_first_machine(
        target.machine_id.clone(),
        &target.nats_server_unit,
        &target.nats_material,
        first_machine_listener(target.machine_public_ip),
    );
    let mut steps = vec![
        HostRunnerStep::VerifyHost(HostPrerequisite::LinuxRootSystemd),
        HostRunnerStep::PrepareDataplaneHost,
        HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            target.dataplane_endpoint_supernet.clone(),
        ),
        HostRunnerStep::VerifyContainerRuntime(ContainerRuntime::Docker),
        HostRunnerStep::InstallArtifact(target.ployzd_artifact.clone()),
        HostRunnerStep::InstallArtifact(target.dataplane_artifacts.ebpf_bytecode.clone()),
        HostRunnerStep::InstallArtifact(target.dataplane_artifacts.ebpf_ctl.clone()),
        HostRunnerStep::InstallArtifact(target.nats_server_artifact.clone()),
        HostRunnerStep::WriteNatsTlsMaterial(NatsTlsMaterialTarget::new(
            target.nats_material.clone(),
            &target.nats_identity,
            target.recovery_key_wrapped.clone(),
            target.core_seeds_wrapped.clone(),
        )),
        HostRunnerStep::WriteNatsAuthorizedUsers(
            NatsAuthorizedUsersTarget::initial_for_first_machine(
                nats_server_config.config_dir().to_path_buf(),
                &target.nats_identity,
                &target.founder_credential_name,
                &target.additional_credentials,
            ),
        ),
        HostRunnerStep::WriteNatsClientCredentials(NatsClientCredentialsTarget::new(
            target.nats_material.clone(),
            &target.nats_identity,
        )),
        HostRunnerStep::WriteNatsServerConfig(nats_server_config),
        HostRunnerStep::WriteSupervisorUnit(SupervisorUnitSpec::NatsServer(
            target.nats_server_unit,
        )),
        HostRunnerStep::StartSupervisorUnit(SupervisorUnitTarget::NatsServer),
    ];

    if let Some(template_file) = target.machine_join_template_file.clone() {
        let template = MachineJoinTemplate {
            join_bundle: MachineJoinBundle {
                material: MachineJoinMaterial {
                    cluster_name: target.machine_join_cluster_name.clone(),
                    dataplane_endpoint_supernet: target.dataplane_endpoint_supernet.clone(),
                    runtime_nats_url: target.machine_join_runtime_nats_url.clone(),
                    trusted_nats: MachineJoinTrustedNats {
                        ca_pem: target.nats_identity.ca.clone(),
                    },
                    recovery_key_wrapped: target.recovery_key_wrapped.clone(),
                    core_seeds_wrapped: target.core_seeds_wrapped.clone(),
                    ployzd: target.ployzd_artifact.install_spec(),
                    ebpf_bytecode: target.dataplane_artifacts.ebpf_bytecode.install_spec(),
                    ebpf_ctl: target.dataplane_artifacts.ebpf_ctl.install_spec(),
                },
            },
        };
        steps.push(HostRunnerStep::WriteMachineJoinTemplate(
            MachineJoinTemplateTarget::new(template_file, template),
        ));
    }

    for role in process_set.roles() {
        let unit = SupervisorUnitTarget::PloyzdRole(role.clone());
        steps.push(HostRunnerStep::WritePloyzdRoleEnvironment(
            PloyzdRoleEnvironmentStep {
                role: role.clone(),
                target: role_environment.clone(),
            },
        ));
        steps.push(HostRunnerStep::WriteSupervisorUnit(
            SupervisorUnitSpec::PloyzdRole {
                role: role.clone(),
                artifact: target.ployzd_artifact.clone(),
                environment_file: role_environment.file_for_role(role),
            },
        ));
        steps.push(HostRunnerStep::StartSupervisorUnit(unit));
    }

    HostRunnerStepPlan::new(steps)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerStepFailure {
    pub step: HostRunnerStepLabel,
    pub reason: HostRunnerStepFailureReason,
    pub message: FailureMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRunnerStepEffectError {
    StepDefault(FailureMessage),
    Explicit {
        reason: HostRunnerStepFailureReason,
        message: FailureMessage,
    },
}

impl HostRunnerStepEffectError {
    #[must_use]
    pub const fn new(reason: HostRunnerStepFailureReason, message: FailureMessage) -> Self {
        Self::Explicit { reason, message }
    }

    #[must_use]
    pub const fn reason(&self) -> Option<HostRunnerStepFailureReason> {
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

impl From<FailureMessage> for HostRunnerStepEffectError {
    fn from(message: FailureMessage) -> Self {
        Self::StepDefault(message)
    }
}

impl HostRunnerStepFailure {
    #[must_use]
    pub fn from_step(step: &HostRunnerStep, message: FailureMessage) -> Self {
        Self {
            step: HostRunnerStepLabel::from_step(step),
            reason: HostRunnerStepFailureReason::from_step(step),
            message,
        }
    }

    #[must_use]
    pub fn from_effect_error(step: &HostRunnerStep, error: HostRunnerStepEffectError) -> Self {
        Self {
            step: HostRunnerStepLabel::from_step(step),
            reason: error
                .reason()
                .unwrap_or_else(|| HostRunnerStepFailureReason::from_step(step)),
            message: error.message().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRunnerStepFailureReason {
    HostPrerequisiteFailed,
    ArtifactDownloadFailed,
    ArtifactVerificationFailed,
    ArtifactInstallFailed,
    NatsConfigWriteFailed,
    NatsTlsMaterialWriteFailed,
    NatsAuthorizedUsersWriteFailed,
    NatsClientCredentialsWriteFailed,
    MachineJoinTemplateWriteFailed,
    RoleEnvironmentWriteFailed,
    SupervisorWriteFailed,
    SupervisorStartFailed,
    SupervisorRestartFailed,
    JoinTokenRedeemFailed,
    JoinReportFailed,
    JoinTokenConsumeFailed,
    JoinMaterialStoreFailed,
    DataplaneHostPrepareFailed,
    ContainerRuntimePrepareFailed,
    ContainerRuntimeVerifyFailed,
    ContainerRuntimeClassicStoreUnsupported,
}

impl HostRunnerStepFailureReason {
    #[must_use]
    pub const fn from_step(step: &HostRunnerStep) -> Self {
        match step {
            HostRunnerStep::VerifyHost(_) => Self::HostPrerequisiteFailed,
            HostRunnerStep::PrepareDataplaneHost => Self::DataplaneHostPrepareFailed,
            HostRunnerStep::PrepareContainerRuntime(_, _) => Self::ContainerRuntimePrepareFailed,
            HostRunnerStep::VerifyContainerRuntime(_) => Self::ContainerRuntimeVerifyFailed,
            HostRunnerStep::InstallArtifact(_) => Self::ArtifactInstallFailed,
            HostRunnerStep::WritePloyzdRoleEnvironment(_) => Self::RoleEnvironmentWriteFailed,
            HostRunnerStep::WriteNatsTlsMaterial(_) => Self::NatsTlsMaterialWriteFailed,
            HostRunnerStep::WriteNatsAuthorizedUsers(_) => Self::NatsAuthorizedUsersWriteFailed,
            HostRunnerStep::WriteNatsClientCredentials(_) => Self::NatsClientCredentialsWriteFailed,
            HostRunnerStep::WriteNatsServerConfig(_) => Self::NatsConfigWriteFailed,
            HostRunnerStep::WriteMachineJoinTemplate(_) => Self::MachineJoinTemplateWriteFailed,
            HostRunnerStep::WriteSupervisorUnit(_) => Self::SupervisorWriteFailed,
            HostRunnerStep::StartSupervisorUnit(_) => Self::SupervisorStartFailed,
            HostRunnerStep::RestartSupervisorUnit(_) => Self::SupervisorRestartFailed,
            HostRunnerStep::StoreJoinMaterial(_) => Self::JoinMaterialStoreFailed,
        }
    }
}
