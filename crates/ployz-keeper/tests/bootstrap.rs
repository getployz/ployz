use std::path::PathBuf;
use std::process::{Command, Output};
use std::{env, fs};

use ployz_core::ids::{NodeId, OperationId};
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, FirstNodeGateway, TunnelSide};
use ployz_keeper::artifacts::{
    ArtifactKind, ArtifactSource, ArtifactTarget, ArtifactTargetError, ArtifactVersion,
    KeeperArtifactTarget, PloyzdArtifactTarget, Sha256Digest,
};
use ployz_keeper::cli::load_startup;
use ployz_keeper::executor::{
    KeeperPlanFailure, KeeperPlanTerminal, KeeperRecordFailure, KeeperStepEffects, KeeperStepEvent,
    KeeperStepRecorder, execute_keeper_plan,
};
use ployz_keeper::join_executor::{
    KeeperJoinRedeemer, KeeperJoinReporter, KeeperJoinTokenConsumer, RedeemedKeeperJoin,
    execute_keeper_join,
};
use ployz_keeper::steps::{
    BootstrapScriptTarget, FirstNodeInstallTarget, HostPrerequisite, JoinMaterialError, JoinToken,
    KeeperJoinTarget, KeeperStep, KeeperStepEffectError, KeeperStepFailure,
    KeeperStepFailureReason, KeeperStepLabel, NonEmptyRoleSet, RedactedJoinMaterial, RoleSetError,
    bootstrap_script_plan, first_node_install_plan, keeper_join_local_install_plan,
};
use ployz_keeper::systemd::{SupervisorUnitSpec, SupervisorUnitTarget};
use ployz_sdk_types::MachineJoinReportFailure;

#[test]
fn bootstrap_script_installs_keeper_only() {
    let plan = bootstrap_script_plan(BootstrapScriptTarget::new(keeper_artifact()));

    assert!(plan.installs_artifact_kind(ArtifactKind::Keeper));
    assert!(!plan.installs_artifact_kind(ArtifactKind::Ployzd));
    assert!(!plan.writes_ployzd_role_units());
    assert_eq!(
        plan.steps(),
        &[
            KeeperStep::VerifyHost(HostPrerequisite::LinuxRootSystemd),
            KeeperStep::InstallArtifact(ArtifactTarget::Keeper(keeper_artifact())),
            KeeperStep::WriteSupervisorUnit(SupervisorUnitSpec::Keeper {
                artifact: keeper_artifact(),
            }),
            KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::Keeper),
        ]
    );
}

#[test]
fn bootstrap_script_file_installs_only_keeper() {
    let script = fs::read_to_string(bootstrap_script_path()).expect("script is readable");

    assert_eq!(
        shell_keeper_unit_template(&script),
        "[Unit]\nDescription=Ployz Keeper\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nEnvironmentFile=-${keeper_env_file}\nExecStart=${keeper_bin}${keeper_args}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    assert!(script.contains("PLOYZ_KEEPER_URL"));
    assert!(script.contains("PLOYZ_NATS_URL"));
    assert!(script.contains("/etc/ployz"));
    assert!(script.contains("PLOYZ_JOIN_TOKEN"));
    assert!(script.contains("join-token"));
    assert!(script.contains("not both"));
    assert!(script.contains("--join-token <token>"));
    assert!(script.contains("unknown ployz installer argument"));
    assert!(script.contains("install -d -m 0700"));
    assert!(script.contains("umask 077"));
    assert!(script.contains("uname -s"));
    assert!(script.contains("id -u"));
    assert!(!script.contains("ployzd"));
    assert!(!script.contains("NATS_CREDS"));
}

#[test]
fn bootstrap_script_rejects_positional_join_token() {
    let output = run_bootstrap_script(&["join_once"], None);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "--join-token <token>");
}

#[test]
fn bootstrap_script_rejects_unknown_join_token_flag() {
    let output = run_bootstrap_script(&["--token", "join_once"], None);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "unknown ployz installer argument: --token");
}

#[test]
fn bootstrap_script_rejects_join_token_from_flag_and_env() {
    let output = run_bootstrap_script(&["--join-token", "join_once"], Some("join_env"));

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "set join token as either --join-token or PLOYZ_JOIN_TOKEN, not both",
    );
}

#[test]
fn keeper_startup_reads_join_token_file_without_consuming_it() {
    let token_file = unique_temp_path("ployz-keeper-join-token");
    fs::write(&token_file, "join_once\n").expect("join token file can be written");

    let startup = load_startup(vec![
        "--join-token-file".into(),
        token_file.as_os_str().to_os_string(),
    ])
    .expect("startup reads join token");
    let join = startup.join.as_ref().expect("join token is loaded");

    assert_eq!(
        &join.token,
        &JoinToken::try_new("join_once").expect("expected token is valid")
    );
    assert_eq!(join.file, token_file);
    assert_eq!(format!("{:?}", join.token), "JoinToken(\"[redacted]\")");
    assert!(token_file.exists());
    fs::remove_file(token_file).expect("test token file can be removed");
}

#[test]
fn keeper_join_installs_ployzd_and_only_assigned_role_units() {
    let roles = vec![
        DaemonProcessRole::Node(node_id("node_7")),
        DaemonProcessRole::Gateway,
        DaemonProcessRole::Tunnel(TunnelSide::Edge),
    ];
    let plan = keeper_join_local_install_plan(KeeperJoinTarget::new(
        RedactedJoinMaterial::new(node_id("node_7"), "prod").expect("valid join material"),
        ployzd_artifact(),
        NonEmptyRoleSet::try_new(roles.clone()).expect("non-empty unique roles"),
    ));

    assert!(plan.installs_artifact_kind(ArtifactKind::Ployzd));
    assert!(plan.writes_ployzd_role_units());
    assert!(plan.steps().contains(&KeeperStep::StoreJoinMaterial(
        RedactedJoinMaterial::new(node_id("node_7"), "prod").expect("valid join material")
    )));

    for role in roles {
        let unit = SupervisorUnitTarget::PloyzdRole(role);
        assert!(plan_writes_unit(&plan, &unit));
        assert!(
            plan.steps()
                .contains(&KeeperStep::StartSupervisorUnit(unit))
        );
    }

    assert!(!plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Control)
    ));
    assert!(!plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Dns)
    ));
}

#[test]
fn first_node_install_starts_nats_and_core_roles_without_join_token() {
    let node_id = node_id("node_1");
    let plan = first_node_install_plan(FirstNodeInstallTarget::new(
        node_id.clone(),
        ployzd_artifact(),
        FirstNodeGateway::Skip,
    ));

    assert!(plan.installs_artifact_kind(ArtifactKind::Ployzd));
    assert!(plan.writes_nats_server_unit());
    assert!(plan.writes_ployzd_role_units());
    assert!(plan_writes_unit(&plan, &SupervisorUnitTarget::NatsServer));
    assert!(plan.steps().contains(&KeeperStep::StartSupervisorUnit(
        SupervisorUnitTarget::NatsServer
    )));

    for role in [
        DaemonProcessRole::Tunnel(TunnelSide::Core),
        DaemonProcessRole::Control,
        DaemonProcessRole::Node(node_id),
    ] {
        let unit = SupervisorUnitTarget::PloyzdRole(role);
        assert!(plan_writes_unit(&plan, &unit));
        assert!(
            plan.steps()
                .contains(&KeeperStep::StartSupervisorUnit(unit))
        );
    }

    assert!(!plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Gateway)
    ));
    assert!(
        !plan
            .steps()
            .iter()
            .any(|step| matches!(step, KeeperStep::StoreJoinMaterial(_)))
    );
}

#[test]
fn first_node_install_can_include_gateway_role() {
    let plan = first_node_install_plan(FirstNodeInstallTarget::new(
        node_id("node_1"),
        ployzd_artifact(),
        FirstNodeGateway::Install,
    ));

    assert!(plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Gateway)
    ));
    assert!(plan.steps().contains(&KeeperStep::StartSupervisorUnit(
        SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Gateway)
    )));
}

#[test]
fn role_sets_reject_empty_and_duplicate_assignments() {
    assert_eq!(NonEmptyRoleSet::try_new(vec![]), Err(RoleSetError::Empty));
    assert_eq!(
        NonEmptyRoleSet::try_new(vec![DaemonProcessRole::Gateway, DaemonProcessRole::Gateway]),
        Err(RoleSetError::Duplicate {
            role: DaemonProcessRole::Gateway,
        })
    );
}

#[test]
fn join_material_cluster_name_rejects_persisted_format_breakers() {
    for value in ["prod\nnext", "prod\rnext", "prod=next"] {
        assert_eq!(
            RedactedJoinMaterial::new(node_id("node_7"), value),
            Err(JoinMaterialError::InvalidClusterName {
                value: value.to_owned(),
            })
        );
    }
}

#[test]
fn keeper_step_failure_is_bootstrap_scoped_and_typed() {
    let step = KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::Keeper);
    assert_eq!(
        KeeperStepFailure::from_step(&step, failure_message("simulated supervisor start failure")),
        KeeperStepFailure {
            step: KeeperStepLabel::StartSupervisorUnit(SupervisorUnitTarget::Keeper),
            reason: KeeperStepFailureReason::SupervisorStartFailed,
            message: failure_message("simulated supervisor start failure"),
        },
    );
}

#[test]
fn keeper_plan_executor_runs_steps_in_order_and_records_progress() {
    let plan = first_node_install_plan(FirstNodeInstallTarget::new(
        node_id("node_1"),
        ployzd_artifact(),
        FirstNodeGateway::Skip,
    ));
    let mut effects = RecordingEffects::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);
    let rendered = format!("{execution:?}");

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    assert!(rendered.contains("/usr/local/bin/ployzd"));
    assert!(rendered.contains(PLOYZD_DIGEST));
    assert_eq!(effects.calls.len(), plan.steps().len());
    assert_eq!(recorder.events, execution.events);
    let [first, second, ..] = effects.calls.as_slice() else {
        panic!("plan records at least two calls");
    };
    assert_eq!(
        *first,
        KeeperStepLabel::VerifyHost(HostPrerequisite::LinuxRootSystemd)
    );
    assert_eq!(
        *second,
        KeeperStepLabel::InstallArtifact(ArtifactTarget::Ployzd(ployzd_artifact()))
    );
    assert_eq!(
        execution.events,
        plan.steps()
            .iter()
            .flat_map(|step| {
                let label = KeeperStepLabel::from_step(step);
                [
                    KeeperStepEvent::Started {
                        step: label.clone(),
                    },
                    KeeperStepEvent::Succeeded { step: label },
                ]
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn keeper_plan_executor_stops_on_first_failed_step() {
    let plan = bootstrap_script_plan(BootstrapScriptTarget::new(keeper_artifact()));
    let keeper_target = ArtifactTarget::Keeper(keeper_artifact());
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::InstallArtifact(keeper_target.clone())),
        fail_message: "simulated artifact install failure",
        ..RecordingEffects::default()
    };
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);
    let failure = KeeperStepFailure {
        step: KeeperStepLabel::InstallArtifact(keeper_target.clone()),
        reason: KeeperStepFailureReason::ArtifactInstallFailed,
        message: failure_message("simulated artifact install failure"),
    };

    assert_eq!(
        execution.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(failure.clone()))
    );
    assert_eq!(
        effects.calls,
        vec![
            KeeperStepLabel::VerifyHost(HostPrerequisite::LinuxRootSystemd),
            KeeperStepLabel::InstallArtifact(keeper_target),
        ]
    );
    assert_eq!(
        execution.events.last(),
        Some(&KeeperStepEvent::Failed(failure))
    );
    assert_eq!(recorder.events, execution.events);
}

#[test]
fn keeper_join_executor_redacts_join_token_from_progress() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer {
        fail_message: Some("simulated join token redeem failure"),
        ..RecordingJoinRedeemer::default()
    };
    let mut effects = RecordingEffects {
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );
    let rendered = format!("{execution:?}");

    assert!(matches!(
        execution.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::RedeemJoinToken,
            reason: KeeperStepFailureReason::JoinTokenRedeemFailed,
            message,
        }))
            if message.as_str() == "simulated join token redeem failure"
    ));
    assert!(!rendered.contains("join_secret"));
    assert_eq!(
        redeemer.redeemed_tokens,
        vec![JoinToken::try_new("join_secret").expect("valid join token")]
    );
    assert_eq!(token_consumer.consumed, 0);
    assert!(reporter.reports.is_empty());
}

#[test]
fn keeper_join_keeps_token_when_material_store_fails() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let material =
        RedactedJoinMaterial::new(node_id("node_7"), "prod").expect("valid join material");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::StoreJoinMaterial(material.clone())),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::StoreJoinMaterial(failed_material),
            reason: KeeperStepFailureReason::JoinMaterialStoreFailed,
            ..
        })) if failed_material == material
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated keeper step failure"),
            },
        }]
    );
}

#[test]
fn keeper_join_keeps_token_when_install_fails_after_redemption() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::InstallArtifact(ArtifactTarget::Ployzd(
            ployzd_artifact(),
        ))),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::InstallArtifact(ArtifactTarget::Ployzd(_)),
            reason: KeeperStepFailureReason::ArtifactInstallFailed,
            ..
        }))
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated keeper step failure"),
            },
        }]
    );
}

#[test]
fn keeper_join_does_not_report_completed_when_token_consume_fails() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut reporter = RecordingJoinReporter::default();
    let mut effects = RecordingEffects {
        ..RecordingEffects::default()
    };
    let mut token_consumer = RecordingTokenConsumer {
        fail_message: Some("simulated token cleanup failure"),
        ..RecordingTokenConsumer::default()
    };
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::ConsumeJoinTokenFile,
            reason: KeeperStepFailureReason::JoinTokenConsumeFailed,
            ..
        }))
    ));
    assert_eq!(token_consumer.consumed, 1);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated token cleanup failure"),
            },
        }]
    );
}

#[test]
fn keeper_plan_executor_records_started_before_applying_step() {
    let plan = bootstrap_script_plan(BootstrapScriptTarget::new(keeper_artifact()));
    let failed_event = KeeperStepEvent::Started {
        step: KeeperStepLabel::InstallArtifact(ArtifactTarget::Keeper(keeper_artifact())),
    };
    let mut effects = RecordingEffects::default();
    let mut recorder = RecordingRecorder {
        fail_on: Some(failed_event.clone()),
        fail_message: "simulated event recorder failure",
        ..RecordingRecorder::default()
    };

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(
        execution.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Record(KeeperRecordFailure {
            event: failed_event,
            message: failure_message("simulated event recorder failure"),
        }))
    );
    assert_eq!(
        effects.calls,
        vec![KeeperStepLabel::VerifyHost(
            HostPrerequisite::LinuxRootSystemd
        )]
    );
    assert_eq!(execution.events, recorder.events);
}

#[test]
fn artifact_digest_must_be_sha256_hex() {
    assert_eq!(
        Sha256Digest::try_new("sha256:keeper"),
        Err(ArtifactTargetError::InvalidSha256Digest {
            value: "sha256:keeper".to_owned()
        })
    );
    assert!(Sha256Digest::try_new(KEEPER_DIGEST).is_ok());
}

#[test]
fn artifact_install_paths_must_be_absolute() {
    assert_eq!(
        KeeperArtifactTarget::new(
            version("0.1.0"),
            source("https://example.invalid/ployz-keeper"),
            digest(KEEPER_DIGEST),
            PathBuf::new(),
        ),
        Err(ArtifactTargetError::EmptyInstallPath)
    );
    assert_eq!(
        PloyzdArtifactTarget::new(
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::from("bin/ployzd"),
        ),
        Err(ArtifactTargetError::RelativeInstallPath {
            value: PathBuf::from("bin/ployzd"),
        })
    );
    assert_eq!(
        PloyzdArtifactTarget::new(
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::from("/"),
        ),
        Err(ArtifactTargetError::MissingInstallParent {
            value: PathBuf::from("/"),
        })
    );
    assert_eq!(
        PloyzdArtifactTarget::new(
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::from("/usr/local/bin/"),
        ),
        Err(ArtifactTargetError::MissingInstallFileName {
            value: PathBuf::from("/usr/local/bin/"),
        })
    );
}

struct RecordingEffects {
    calls: Vec<KeeperStepLabel>,
    fail_on: Option<KeeperStepLabel>,
    fail_message: &'static str,
}

impl RecordingEffects {
    fn record(&mut self, label: KeeperStepLabel) -> Result<(), KeeperStepEffectError> {
        self.calls.push(label.clone());
        if self.fail_on.as_ref() == Some(&label) {
            return Err(failure_message(self.fail_message).into());
        }
        Ok(())
    }
}

impl KeeperStepEffects for RecordingEffects {
    fn apply_step(&mut self, step: &KeeperStep) -> Result<(), KeeperStepEffectError> {
        self.record(KeeperStepLabel::from_step(step))
    }
}

#[derive(Default)]
struct RecordingJoinRedeemer {
    redeemed_tokens: Vec<JoinToken>,
    fail_message: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JoinReport {
    Completed,
    Failed { failure: MachineJoinReportFailure },
}

#[derive(Default)]
struct RecordingJoinReporter {
    reports: Vec<JoinReport>,
    fail_message: Option<&'static str>,
}

#[derive(Default)]
struct RecordingTokenConsumer {
    consumed: usize,
    fail_message: Option<&'static str>,
}

impl KeeperJoinTokenConsumer for RecordingTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        self.consumed += 1;
        if let Some(message) = self.fail_message {
            return Err(failure_message(message));
        }

        Ok(())
    }
}

impl KeeperJoinRedeemer for RecordingJoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedKeeperJoin, FailureMessage> {
        self.redeemed_tokens.push(token.clone());
        if let Some(message) = self.fail_message {
            return Err(failure_message(message));
        }

        Ok(RedeemedKeeperJoin::new(
            operation_id("op_machine"),
            node_id("node_7"),
            KeeperJoinTarget::new(
                RedactedJoinMaterial::new(node_id("node_7"), "prod").expect("valid join material"),
                ployzd_artifact(),
                NonEmptyRoleSet::try_new(vec![DaemonProcessRole::Node(node_id("node_7"))])
                    .expect("non-empty role set"),
            ),
        ))
    }
}

impl KeeperJoinReporter for RecordingJoinReporter {
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
struct RecordingRecorder {
    events: Vec<KeeperStepEvent>,
    fail_on: Option<KeeperStepEvent>,
    fail_message: &'static str,
}

impl KeeperStepRecorder for RecordingRecorder {
    fn record_step_event(&mut self, event: &KeeperStepEvent) -> Result<(), FailureMessage> {
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
            fail_message: "simulated keeper step failure",
        }
    }
}

fn keeper_artifact() -> KeeperArtifactTarget {
    KeeperArtifactTarget::new(
        version("0.1.0"),
        source("https://example.invalid/ployz-keeper"),
        digest(KEEPER_DIGEST),
        PathBuf::from("/usr/local/bin/ployz-keeper"),
    )
    .expect("valid keeper artifact")
}

fn ployzd_artifact() -> PloyzdArtifactTarget {
    PloyzdArtifactTarget::new(
        version("0.1.0"),
        source("https://example.invalid/ployzd"),
        digest(PLOYZD_DIGEST),
        PathBuf::from("/usr/local/bin/ployzd"),
    )
    .expect("valid ployzd artifact")
}

fn version(value: &str) -> ArtifactVersion {
    ArtifactVersion::try_new(value).expect("valid artifact version")
}

fn source(value: &str) -> ArtifactSource {
    ArtifactSource::try_new(value).expect("valid artifact source")
}

fn digest(value: &str) -> Sha256Digest {
    Sha256Digest::try_new(value).expect("valid artifact digest")
}

fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}

fn plan_writes_unit(
    plan: &ployz_keeper::steps::KeeperStepPlan,
    target: &SupervisorUnitTarget,
) -> bool {
    plan.steps().iter().any(
        |step| matches!(step, KeeperStep::WriteSupervisorUnit(spec) if spec.target() == *target),
    )
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn unique_temp_path(prefix: &str) -> PathBuf {
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

fn bootstrap_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("scripts")
        .join("ployz.sh")
}

fn run_bootstrap_script(args: &[&str], join_token_env: Option<&str>) -> Output {
    let mut command = Command::new("sh");
    command
        .arg(bootstrap_script_path())
        .args(args)
        .env("PLOYZ_KEEPER_URL", "https://example.invalid/ployz-keeper")
        .env("PLOYZ_KEEPER_SHA256", KEEPER_DIGEST);

    match join_token_env {
        Some(token) => {
            command.env("PLOYZ_JOIN_TOKEN", token);
        }
        None => {
            command.env_remove("PLOYZ_JOIN_TOKEN");
        }
    }

    command.output().expect("bootstrap script can run")
}

fn assert_stderr_contains(output: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "stderr should contain {expected:?}, got {stderr:?}"
    );
}

const KEEPER_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PLOYZD_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn shell_keeper_unit_template(script: &str) -> &str {
    let start = script
        .find("cat > \"$keeper_unit\" <<UNIT\n")
        .expect("keeper unit heredoc starts")
        + "cat > \"$keeper_unit\" <<UNIT\n".len();
    let end = script[start..]
        .find("\nUNIT\n")
        .expect("keeper unit heredoc ends")
        + start
        + 1;
    &script[start..end]
}
