//! Typed keeper step plans.

use std::fmt;

use ployz_core::ids::{NodeId, OperationId};
use ployz_core::roles::{DaemonProcessRole, FirstNodeGateway, first_node_process_set};

use crate::artifacts::{ArtifactKind, ArtifactTarget, KeeperArtifactTarget, PloyzdArtifactTarget};
use crate::health::{KeeperStepFailureReason, RoleHealthGate};
use crate::systemd::SupervisorUnitTarget;

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
            matches!(
                step,
                KeeperStep::WriteSupervisorUnit(SupervisorUnitTarget::PloyzdRole(_))
            )
        })
    }

    #[must_use]
    pub fn writes_nats_server_unit(&self) -> bool {
        self.steps.iter().any(|step| {
            matches!(
                step,
                KeeperStep::WriteSupervisorUnit(SupervisorUnitTarget::NatsServer)
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperStep {
    VerifyHost(HostPrerequisite),
    VerifyArtifact(ArtifactTarget),
    InstallArtifact(ArtifactTarget),
    WriteSupervisorUnit(SupervisorUnitTarget),
    StartSupervisorUnit(SupervisorUnitTarget),
    RestartSupervisorUnit(SupervisorUnitTarget),
    HealthCheck(RoleHealthGate),
    RedeemJoinToken(JoinToken),
    StoreJoinMaterial(RedactedJoinMaterial),
    ReportProgress(OperationId),
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstrateRolloutTarget {
    pub operation_id: OperationId,
    pub ployzd_artifact: PloyzdArtifactTarget,
    pub roles: NonEmptyRoleSet,
}

impl SubstrateRolloutTarget {
    #[must_use]
    pub fn new(
        operation_id: OperationId,
        ployzd_artifact: PloyzdArtifactTarget,
        roles: NonEmptyRoleSet,
    ) -> Self {
        Self {
            operation_id,
            ployzd_artifact,
            roles,
        }
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
        KeeperStep::InstallArtifact(target.keeper_artifact.into()),
        KeeperStep::WriteSupervisorUnit(SupervisorUnitTarget::Keeper),
        KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::Keeper),
    ])
}

#[must_use]
pub fn keeper_join_plan(target: KeeperJoinTarget) -> KeeperStepPlan {
    let mut steps = vec![
        KeeperStep::RedeemJoinToken(target.token),
        KeeperStep::StoreJoinMaterial(target.material),
        KeeperStep::VerifyArtifact(target.ployzd_artifact.clone().into()),
        KeeperStep::InstallArtifact(target.ployzd_artifact.into()),
    ];

    for role in target.roles.roles {
        let unit = SupervisorUnitTarget::PloyzdRole(role);
        steps.push(KeeperStep::WriteSupervisorUnit(unit.clone()));
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
        KeeperStep::InstallArtifact(target.ployzd_artifact.into()),
        KeeperStep::WriteSupervisorUnit(SupervisorUnitTarget::NatsServer),
        KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::NatsServer),
    ];

    for role in process_set.roles() {
        let unit = SupervisorUnitTarget::PloyzdRole(role.clone());
        steps.push(KeeperStep::WriteSupervisorUnit(unit.clone()));
        steps.push(KeeperStep::StartSupervisorUnit(unit));
    }

    KeeperStepPlan::new(steps)
}

#[must_use]
pub fn substrate_rollout_plan(target: SubstrateRolloutTarget) -> KeeperStepPlan {
    let mut steps = vec![
        KeeperStep::ReportProgress(target.operation_id.clone()),
        KeeperStep::VerifyArtifact(target.ployzd_artifact.clone().into()),
        KeeperStep::InstallArtifact(target.ployzd_artifact.into()),
    ];

    for role in target.roles.roles {
        steps.push(KeeperStep::RestartSupervisorUnit(
            SupervisorUnitTarget::PloyzdRole(role.clone()),
        ));
        steps.push(KeeperStep::HealthCheck(RoleHealthGate::new(role, 60)));
        steps.push(KeeperStep::ReportProgress(target.operation_id.clone()));
    }

    KeeperStepPlan::new(steps)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperStepFailure {
    pub step: KeeperStep,
    pub reason: KeeperStepFailureReason,
}

impl KeeperStepFailure {
    #[must_use]
    pub const fn new(step: KeeperStep, reason: KeeperStepFailureReason) -> Self {
        Self { step, reason }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperRolloutStatus {
    Running,
    Paused { failure: KeeperStepFailure },
    Completed,
}

#[must_use]
pub const fn pause_rollout_on_failure(failure: KeeperStepFailure) -> KeeperRolloutStatus {
    KeeperRolloutStatus::Paused { failure }
}
