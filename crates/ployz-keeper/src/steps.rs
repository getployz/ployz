//! Typed keeper step plans.

use std::fmt;

use ployz_core::ids::NodeId;
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, FirstNodeGateway, first_node_process_set};

use crate::artifacts::{ArtifactKind, ArtifactTarget, KeeperArtifactTarget, PloyzdArtifactTarget};
use crate::systemd::{NatsServerUnitTarget, SupervisorUnitSpec, SupervisorUnitTarget};

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
    VerifyArtifact(ArtifactTarget),
    InstallArtifact(ArtifactTarget),
    WriteSupervisorUnit(SupervisorUnitSpec),
    StartSupervisorUnit(SupervisorUnitTarget),
    RestartSupervisorUnit(SupervisorUnitTarget),
    RedeemJoinToken(JoinToken),
    StoreJoinMaterial(RedactedJoinMaterial),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperStepLabel {
    VerifyHost(HostPrerequisite),
    VerifyArtifact(ArtifactTarget),
    InstallArtifact(ArtifactTarget),
    WriteSupervisorUnit(SupervisorUnitTarget),
    StartSupervisorUnit(SupervisorUnitTarget),
    RestartSupervisorUnit(SupervisorUnitTarget),
    RedeemJoinToken,
    StoreJoinMaterial(RedactedJoinMaterial),
}

impl KeeperStepLabel {
    #[must_use]
    pub fn from_step(step: &KeeperStep) -> Self {
        match step {
            KeeperStep::VerifyHost(prerequisite) => Self::VerifyHost(*prerequisite),
            KeeperStep::VerifyArtifact(target) => Self::VerifyArtifact(target.clone()),
            KeeperStep::InstallArtifact(target) => Self::InstallArtifact(target.clone()),
            KeeperStep::WriteSupervisorUnit(spec) => Self::WriteSupervisorUnit(spec.target()),
            KeeperStep::StartSupervisorUnit(target) => Self::StartSupervisorUnit(target.clone()),
            KeeperStep::RestartSupervisorUnit(target) => {
                Self::RestartSupervisorUnit(target.clone())
            }
            KeeperStep::RedeemJoinToken(_) => Self::RedeemJoinToken,
            KeeperStep::StoreJoinMaterial(material) => Self::StoreJoinMaterial(material.clone()),
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
pub struct RedactedJoinMaterial {
    pub node_id: NodeId,
    pub cluster_name: String,
}

impl RedactedJoinMaterial {
    pub fn new(
        node_id: NodeId,
        cluster_name: impl Into<String>,
    ) -> Result<Self, JoinMaterialError> {
        let cluster_name = cluster_name.into();
        if cluster_name.is_empty() {
            return Err(JoinMaterialError::EmptyClusterName);
        }

        Ok(Self {
            node_id,
            cluster_name,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinMaterialError {
    EmptyJoinToken,
    EmptyClusterName,
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
    pub token: JoinToken,
    pub material: RedactedJoinMaterial,
    pub ployzd_artifact: PloyzdArtifactTarget,
    pub roles: NonEmptyRoleSet,
}

impl KeeperJoinTarget {
    #[must_use]
    pub fn new(
        token: JoinToken,
        material: RedactedJoinMaterial,
        ployzd_artifact: PloyzdArtifactTarget,
        roles: NonEmptyRoleSet,
    ) -> Self {
        Self {
            token,
            material,
            ployzd_artifact,
            roles,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstNodeInstallTarget {
    pub node_id: NodeId,
    pub ployzd_artifact: PloyzdArtifactTarget,
    pub gateway: FirstNodeGateway,
    pub nats_server_unit: NatsServerUnitTarget,
}

impl FirstNodeInstallTarget {
    #[must_use]
    pub fn new(
        node_id: NodeId,
        ployzd_artifact: PloyzdArtifactTarget,
        gateway: FirstNodeGateway,
    ) -> Self {
        Self {
            node_id,
            ployzd_artifact,
            gateway,
            nats_server_unit: NatsServerUnitTarget::default_paths(),
        }
    }

    #[must_use]
    pub fn with_nats_server_unit(mut self, nats_server_unit: NatsServerUnitTarget) -> Self {
        self.nats_server_unit = nats_server_unit;
        self
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
        KeeperStep::VerifyArtifact(target.keeper_artifact.clone().into()),
        KeeperStep::InstallArtifact(target.keeper_artifact.clone().into()),
        KeeperStep::WriteSupervisorUnit(SupervisorUnitSpec::Keeper {
            artifact: target.keeper_artifact,
        }),
        KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::Keeper),
    ])
}

#[must_use]
pub fn keeper_join_plan(target: KeeperJoinTarget) -> KeeperStepPlan {
    let mut steps = vec![
        KeeperStep::RedeemJoinToken(target.token),
        KeeperStep::StoreJoinMaterial(target.material),
        KeeperStep::VerifyArtifact(target.ployzd_artifact.clone().into()),
        KeeperStep::InstallArtifact(target.ployzd_artifact.clone().into()),
    ];

    for role in target.roles.roles {
        let unit = SupervisorUnitTarget::PloyzdRole(role.clone());
        steps.push(KeeperStep::WriteSupervisorUnit(
            SupervisorUnitSpec::PloyzdRole {
                role,
                artifact: target.ployzd_artifact.clone(),
            },
        ));
        steps.push(KeeperStep::StartSupervisorUnit(unit));
    }

    KeeperStepPlan::new(steps)
}

#[must_use]
pub fn first_node_install_plan(target: FirstNodeInstallTarget) -> KeeperStepPlan {
    let process_set = first_node_process_set(&target.node_id, target.gateway);
    let mut steps = vec![
        KeeperStep::VerifyHost(HostPrerequisite::LinuxRootSystemd),
        KeeperStep::VerifyArtifact(target.ployzd_artifact.clone().into()),
        KeeperStep::InstallArtifact(target.ployzd_artifact.clone().into()),
        KeeperStep::WriteSupervisorUnit(SupervisorUnitSpec::NatsServer(target.nats_server_unit)),
        KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::NatsServer),
    ];

    for role in process_set.roles() {
        let unit = SupervisorUnitTarget::PloyzdRole(role.clone());
        steps.push(KeeperStep::WriteSupervisorUnit(
            SupervisorUnitSpec::PloyzdRole {
                role: role.clone(),
                artifact: target.ployzd_artifact.clone(),
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

impl KeeperStepFailure {
    #[must_use]
    pub fn from_step(step: &KeeperStep, message: FailureMessage) -> Self {
        Self {
            step: KeeperStepLabel::from_step(step),
            reason: KeeperStepFailureReason::from_step(step),
            message,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeeperStepFailureReason {
    HostPrerequisiteFailed,
    ArtifactVerificationFailed,
    ArtifactInstallFailed,
    SupervisorWriteFailed,
    SupervisorStartFailed,
    SupervisorRestartFailed,
    JoinTokenRedeemFailed,
    JoinMaterialStoreFailed,
}

impl KeeperStepFailureReason {
    #[must_use]
    pub const fn from_step(step: &KeeperStep) -> Self {
        match step {
            KeeperStep::VerifyHost(_) => Self::HostPrerequisiteFailed,
            KeeperStep::VerifyArtifact(_) => Self::ArtifactVerificationFailed,
            KeeperStep::InstallArtifact(_) => Self::ArtifactInstallFailed,
            KeeperStep::WriteSupervisorUnit(_) => Self::SupervisorWriteFailed,
            KeeperStep::StartSupervisorUnit(_) => Self::SupervisorStartFailed,
            KeeperStep::RestartSupervisorUnit(_) => Self::SupervisorRestartFailed,
            KeeperStep::RedeemJoinToken(_) => Self::JoinTokenRedeemFailed,
            KeeperStep::StoreJoinMaterial(_) => Self::JoinMaterialStoreFailed,
        }
    }
}
