#![allow(dead_code)]

use ployz_core::install::{WrappedCaKey, WrappedCoreSeeds};
use std::path::PathBuf;
use std::sync::OnceLock;

use ployz_core::ids::MachineId;
use ployz_core::install::NatsMachineMaterialPaths;
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, InstallRolePolicy};
use ployz_host_runner::execution::{ArtifactKind, ArtifactTarget, DataplaneArtifactTargets};
use ployz_host_runner::execution::{PloyzdRoleEnvironmentFile, SupervisorUnitTarget};
use ployz_host_runner::lifecycle::{
    HostRunnerJoinRedeemer, HostRunnerJoinReporter, HostRunnerJoinTokenConsumer,
    RedeemedHostRunnerJoin,
};
use ployz_host_runner::plan::{
    FirstMachineInstallTarget, HostRunnerJoinMaterial, HostRunnerJoinTarget, HostRunnerStep,
    HostRunnerStepEffectError, HostRunnerStepLabel, JoinToken, NonEmptyRoleSet,
    PloyzdRoleEnvironmentTarget, RoleNatsCredentials, first_machine_install_plan,
};
use ployz_host_runner::plan::{HostRunnerStepEffects, HostRunnerStepEvent, HostRunnerStepRecorder};
use ployz_host_runner::recovery::{
    ClusterNatsIdentity, ServerCertificateSans, generate_cluster_nats_identity,
};
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::server_config::NatsListener;
use ployz_sdk_types::MachineJoinReportFailure;
use ployz_test_support::host_runner::{
    artifact_source as source, artifact_version as version, nats_server_artifact, ployzd_artifact,
    sha256_digest as digest,
};
use ployz_test_support::ids::{failure_message, machine_id, operation_id};

pub struct RecordingEffects {
    pub calls: Vec<HostRunnerStepLabel>,
    pub fail_on: Option<HostRunnerStepLabel>,
    pub fail_message: &'static str,
}

impl RecordingEffects {
    fn record(&mut self, label: HostRunnerStepLabel) -> Result<(), HostRunnerStepEffectError> {
        self.calls.push(label.clone());
        if self.fail_on.as_ref() == Some(&label) {
            return Err(failure_message(self.fail_message).into());
        }
        Ok(())
    }
}

impl HostRunnerStepEffects for RecordingEffects {
    fn apply_step(&mut self, step: &HostRunnerStep) -> Result<(), HostRunnerStepEffectError> {
        self.record(HostRunnerStepLabel::from_step(step))
    }
}

#[derive(Default)]
pub struct RecordingJoinRedeemer {
    pub redeemed_tokens: Vec<JoinToken>,
    pub fail_message: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinReport {
    Completed,
    Failed { failure: MachineJoinReportFailure },
}

#[derive(Default)]
pub struct RecordingJoinReporter {
    pub reports: Vec<JoinReport>,
    pub fail_message: Option<&'static str>,
}

#[derive(Default)]
pub struct RecordingTokenConsumer {
    pub consumed: usize,
    pub fail_message: Option<&'static str>,
}

impl HostRunnerJoinTokenConsumer for RecordingTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        self.consumed += 1;
        if let Some(message) = self.fail_message {
            return Err(failure_message(message));
        }

        Ok(())
    }
}

impl HostRunnerJoinRedeemer for RecordingJoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedHostRunnerJoin, FailureMessage> {
        self.redeemed_tokens.push(token.clone());
        if let Some(message) = self.fail_message {
            return Err(failure_message(message));
        }

        Ok(RedeemedHostRunnerJoin::new(
            operation_id("op_machine"),
            machine_id("machine_7"),
            HostRunnerJoinTarget::new(
                host_runner_join_material(),
                ployzd_artifact(),
                dataplane_artifacts(),
                NonEmptyRoleSet::try_new(vec![DaemonProcessRole::Machine(machine_id("machine_7"))])
                    .expect("non-empty role set"),
                role_environment(),
                ployz_core::install::HostPortAssurance::Keeper,
            ),
        ))
    }
}

pub fn host_runner_join_material() -> HostRunnerJoinMaterial {
    HostRunnerJoinMaterial::new(
        machine_id("machine_7"),
        "prod",
        NatsUserSeed::try_new("SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM")
            .expect("valid nats credentials"),
        test_ca_pem(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    )
    .expect("valid join material")
}

pub fn test_ca_pem() -> NatsCaCertificatePem {
    NatsCaCertificatePem::try_new(
        "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
    )
    .expect("valid test CA pem")
}

pub fn test_identity() -> &'static ClusterNatsIdentity {
    static IDENTITY: OnceLock<ClusterNatsIdentity> = OnceLock::new();
    IDENTITY.get_or_init(|| {
        generate_cluster_nats_identity(
            &ServerCertificateSans::try_new(None, None).expect("valid SAN inputs"),
        )
        .expect("test identity generates")
    })
}

impl HostRunnerJoinReporter for RecordingJoinReporter {
    fn report_join_completed(&mut self) -> Result<(), FailureMessage> {
        self.reports.push(JoinReport::Completed);
        if let Some(message) = self.fail_message {
            return Err(failure_message(message));
        }

        Ok(())
    }

    fn report_join_failed(
        &mut self,
        failure: MachineJoinReportFailure,
    ) -> Result<(), FailureMessage> {
        self.reports.push(JoinReport::Failed { failure });
        if let Some(message) = self.fail_message {
            return Err(failure_message(message));
        }

        Ok(())
    }
}

#[derive(Default)]
pub struct RecordingRecorder {
    pub events: Vec<HostRunnerStepEvent>,
    pub fail_on: Option<HostRunnerStepEvent>,
    pub fail_message: &'static str,
}

impl HostRunnerStepRecorder for RecordingRecorder {
    fn record_step_event(&mut self, event: &HostRunnerStepEvent) -> Result<(), FailureMessage> {
        if self.fail_on.as_ref() == Some(event) {
            return Err(failure_message(self.fail_message));
        }
        self.events.push(event.clone());
        Ok(())
    }
}

impl Default for RecordingEffects {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            fail_on: None,
            fail_message: "simulated Host Runner step failure",
        }
    }
}

pub fn ebpf_bytecode_artifact() -> ArtifactTarget {
    ArtifactTarget::new(
        ArtifactKind::EbpfBytecode,
        version("0.1.0"),
        source("/tmp/ployz-ebpf-tc"),
        digest(HOST_RUNNER_DIGEST),
        PathBuf::from("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"),
    )
    .expect("valid eBPF bytecode artifact")
}

pub fn ebpf_ctl_artifact() -> ArtifactTarget {
    ArtifactTarget::new(
        ArtifactKind::EbpfCtl,
        version("0.1.0"),
        source("/tmp/ployz-ebpf-ctl"),
        digest(HOST_RUNNER_DIGEST),
        PathBuf::from("/usr/local/bin/ployz-ebpf-ctl"),
    )
    .expect("valid eBPF ctl artifact")
}

pub fn dataplane_artifacts() -> DataplaneArtifactTargets {
    DataplaneArtifactTargets::new(ebpf_bytecode_artifact(), ebpf_ctl_artifact())
}

pub fn role_environment() -> PloyzdRoleEnvironmentTarget {
    PloyzdRoleEnvironmentTarget::new(
        PloyzdRoleEnvironmentFile::new(PathBuf::from("/etc/ployz/ployzd.env"))
            .expect("valid role environment path"),
        machine_id("machine_1"),
        NatsClientUrl::try_new("nats://127.0.0.1:4222").expect("valid NATS URL"),
        RoleNatsCredentials::joined(std::path::Path::new("/var/lib/ployz/join-material.d")),
    )
    .with_ebpf_bytecode_path(PathBuf::from("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"))
    .with_ebpf_ctl_path(PathBuf::from("/usr/local/bin/ployz-ebpf-ctl"))
}

pub fn edge_role_environment() -> PloyzdRoleEnvironmentTarget {
    PloyzdRoleEnvironmentTarget::new(
        PloyzdRoleEnvironmentFile::new(PathBuf::from("/etc/ployz/ployzd.env"))
            .expect("valid role environment path"),
        machine_id("machine_7"),
        NatsClientUrl::try_new("nats://127.0.0.1:7422").expect("valid NATS URL"),
        RoleNatsCredentials::joined(std::path::Path::new("/var/lib/ployz/join-material.d")),
    )
    .with_ebpf_bytecode_path(PathBuf::from("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"))
    .with_ebpf_ctl_path(PathBuf::from("/usr/local/bin/ployz-ebpf-ctl"))
}

pub fn first_machine_plan() -> ployz_host_runner::plan::HostRunnerStepPlan {
    first_machine_install_plan(FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all().without_gateway(),
        ployz_core::install::HostPortAssurance::Keeper,
        test_identity().clone(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    ))
}

pub fn installs_artifact_kind(
    plan: &ployz_host_runner::plan::HostRunnerStepPlan,
    kind: ArtifactKind,
) -> bool {
    plan.steps().iter().any(
        |step| matches!(step, HostRunnerStep::InstallArtifact(artifact) if artifact.kind == kind),
    )
}

pub fn writes_ployzd_role_units(plan: &ployz_host_runner::plan::HostRunnerStepPlan) -> bool {
    plan.steps().iter().any(|step| {
        matches!(
            step,
            HostRunnerStep::WriteSupervisorUnit(spec)
                if matches!(spec.target(), SupervisorUnitTarget::PloyzdRole(_))
        )
    })
}

pub fn writes_nats_server_unit(plan: &ployz_host_runner::plan::HostRunnerStepPlan) -> bool {
    plan_writes_unit(plan, &SupervisorUnitTarget::NatsServer)
}

pub fn plan_writes_unit(
    plan: &ployz_host_runner::plan::HostRunnerStepPlan,
    target: &SupervisorUnitTarget,
) -> bool {
    plan.steps().iter().any(
        |step| matches!(step, HostRunnerStep::WriteSupervisorUnit(spec) if spec.target() == *target),
    )
}

pub fn first_machine_nats_target(
    machine_id: MachineId,
) -> ployz_host_runner::plan::NatsServerConfigTarget {
    ployz_host_runner::plan::NatsServerConfigTarget::for_first_machine(
        machine_id,
        &ployz_host_runner::execution::NatsServerUnitTarget::default_paths(),
        &NatsMachineMaterialPaths::in_default_state_dir(),
        NatsListener::Loopback,
    )
}

pub fn unique_temp_path(prefix: &str) -> PathBuf {
    let unique = format!(
        "{}-{}",
        prefix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

pub const HOST_RUNNER_DIGEST: &str =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const PLOYZD_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const NATS_CA_DIGEST: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
