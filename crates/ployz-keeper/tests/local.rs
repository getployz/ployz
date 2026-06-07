use std::fs;
use std::path::{Path, PathBuf};

use ployz_core::ids::NodeId;
use ployz_core::ops::FailureMessage;
use ployz_core::roles::FirstNodeGateway;
use ployz_keeper::artifacts::{
    ArtifactSource, ArtifactVersion, KeeperArtifactTarget, PloyzdArtifactTarget, Sha256Digest,
};
use ployz_keeper::executor::{
    KeeperPlanFailure, KeeperPlanTerminal, KeeperStepEffects, KeeperStepEvent, KeeperStepRecorder,
    execute_keeper_plan,
};
use ployz_keeper::join::JOIN_MATERIAL_FILE;
use ployz_keeper::join_executor::{
    KeeperJoinRedeemer, KeeperJoinTokenConsumer, execute_keeper_join,
};
use ployz_keeper::local::{KeeperCommandRunner, KeeperLocalConfig, KeeperLocalEffects};
use ployz_keeper::steps::{
    BootstrapScriptTarget, FirstNodeInstallTarget, JoinToken, KeeperJoinTarget, KeeperStep,
    KeeperStepFailure, KeeperStepFailureReason, KeeperStepLabel, NonEmptyRoleSet,
    RedactedJoinMaterial, bootstrap_script_plan, first_node_install_plan,
};
use ployz_keeper::systemd::{NatsServerUnitTarget, SupervisorUnitTarget};

#[test]
fn local_effects_install_keeper_and_start_its_unit() {
    let root = temp_dir("ployz-keeper-local-bootstrap");
    let source = root.join("ployz-keeper-source");
    let install_path = root.join("bin/ployz-keeper");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");

    let keeper_artifact = keeper_artifact(&source, &install_path);
    let plan = bootstrap_script_plan(BootstrapScriptTarget::new(keeper_artifact));
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    assert_eq!(fs::read_to_string(&install_path).unwrap(), "ployz\n");
    let keeper_unit = fs::read_to_string(systemd_dir.join("ployz-keeper.service")).unwrap();
    assert!(keeper_unit.contains("Description=Ployz Keeper"));
    assert!(keeper_unit.contains("Type=exec"));
    assert!(keeper_unit.contains(install_path.to_str().expect("path is utf-8")));
    assert_eq!(
        effects.runner().systemctl_calls,
        vec![
            vec!["daemon-reload".to_owned()],
            vec![
                "enable".to_owned(),
                "--now".to_owned(),
                "ployz-keeper.service".to_owned(),
            ],
        ]
    );
    assert_eq!(recorder.events, execution.events);
}

#[test]
fn local_effects_install_first_node_process_units() {
    let root = temp_dir("ployz-keeper-local-first-node");
    let source = root.join("ployzd-source");
    let install_path = root.join("bin/ployzd");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    write_nats_config(&root);

    let ployzd_artifact = ployzd_artifact(&source, &install_path);
    let plan = first_node_install_plan(
        FirstNodeInstallTarget::new(node_id("node_1"), ployzd_artifact, FirstNodeGateway::Skip)
            .with_nats_server_unit(nats_unit(&root)),
    );
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    assert_eq!(fs::read_to_string(&install_path).unwrap(), "ployz\n");
    assert!(
        fs::read_to_string(systemd_dir.join("nats-server.service"))
            .unwrap()
            .contains("--config")
    );
    assert!(
        fs::read_to_string(systemd_dir.join("ployzd-control.service"))
            .unwrap()
            .contains("control")
    );
    assert!(systemd_dir.join("ployzd-tunnel-core.service").exists());
    assert!(systemd_dir.join("ployzd-node-node_1.service").exists());
    assert!(!systemd_dir.join("ployzd-gateway.service").exists());
}

#[test]
fn local_effects_fail_before_work_when_host_is_not_root() {
    let root = temp_dir("ployz-keeper-local-not-root");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let plan = bootstrap_script_plan(BootstrapScriptTarget::new(keeper_artifact(
        &root.join("source"),
        &root.join("bin/ployz-keeper"),
    )));
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            uid: 501,
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::VerifyHost(_),
            reason: KeeperStepFailureReason::HostPrerequisiteFailed,
            message,
        })) if message.as_str() == "keeper must run as root"
    ));
    assert_eq!(effects.runner().systemctl_calls, Vec::<Vec<String>>::new());
}

#[test]
fn local_effects_download_remote_artifact_sources() {
    let root = temp_dir("ployz-keeper-local-remote-source");
    let systemd_dir = root.join("systemd");
    let install_path = root.join("bin/ployz-keeper");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let plan = bootstrap_script_plan(BootstrapScriptTarget::new(
        KeeperArtifactTarget::new(
            version("0.1.0"),
            ArtifactSource::try_new("https://example.invalid/ployz-keeper")
                .expect("valid remote source"),
            digest(PLOYZ_NEWLINE_SHA256),
            install_path.clone(),
        )
        .expect("valid keeper artifact"),
    ));
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            download_body: Some(b"ployz\n".to_vec()),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    assert_eq!(fs::read_to_string(install_path).unwrap(), "ployz\n");
    let downloads = effects.runner().downloads.clone();
    assert_eq!(downloads.len(), 1);
    assert!(
        downloads
            .iter()
            .all(|download| download.url == "https://example.invalid/ployz-keeper")
    );
    drop(effects);
    assert!(downloads.iter().all(RecordedDownload::is_cleaned_up));
}

#[test]
fn local_effects_remove_partial_remote_download_after_failure() {
    let root = temp_dir("ployz-keeper-local-remote-source-fail");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let url = "https://example.invalid/ployz-keeper";
    let plan = bootstrap_script_plan(BootstrapScriptTarget::new(
        KeeperArtifactTarget::new(
            version("0.1.0"),
            ArtifactSource::try_new(url).expect("valid remote source"),
            digest(PLOYZ_NEWLINE_SHA256),
            root.join("bin/ployz-keeper"),
        )
        .expect("valid keeper artifact"),
    ));
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            fail_download: Some(url.to_owned()),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::InstallArtifact(_),
            reason: KeeperStepFailureReason::ArtifactDownloadFailed,
            message,
        })) if message.as_str() == "simulated download failure"
    ));
    let downloads = effects.runner().downloads.clone();
    assert_eq!(downloads.len(), 1);
    assert!(downloads.iter().all(RecordedDownload::is_cleaned_up));
}

#[test]
fn local_effects_report_remote_artifact_digest_mismatch_as_verification_failure() {
    let root = temp_dir("ployz-keeper-local-remote-digest-fail");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let plan = bootstrap_script_plan(BootstrapScriptTarget::new(
        KeeperArtifactTarget::new(
            version("0.1.0"),
            ArtifactSource::try_new("https://example.invalid/ployz-keeper")
                .expect("valid remote source"),
            digest(PLOYZ_NEWLINE_SHA256),
            root.join("bin/ployz-keeper"),
        )
        .expect("valid keeper artifact"),
    ));
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            download_body: Some(b"wrong\n".to_vec()),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let first = execute_keeper_plan(&plan, &mut effects, &mut recorder);
    let second = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        first.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::InstallArtifact(_),
            reason: KeeperStepFailureReason::ArtifactVerificationFailed,
            ..
        }))
    ));
    assert!(matches!(
        second.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::InstallArtifact(_),
            reason: KeeperStepFailureReason::ArtifactVerificationFailed,
            ..
        }))
    ));
    assert_eq!(effects.runner().downloads.len(), 2);
    assert!(
        effects
            .runner()
            .downloads
            .iter()
            .all(RecordedDownload::is_cleaned_up)
    );
}

#[test]
fn local_effects_reject_nats_unit_without_existing_config() {
    let root = temp_dir("ployz-keeper-local-missing-nats-config");
    let source = root.join("ployzd-source");
    let install_path = root.join("bin/ployzd");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");

    let ployzd_artifact = ployzd_artifact(&source, &install_path);
    let plan = first_node_install_plan(
        FirstNodeInstallTarget::new(node_id("node_1"), ployzd_artifact, FirstNodeGateway::Skip)
            .with_nats_server_unit(nats_unit(&root)),
    );
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::WriteSupervisorUnit(SupervisorUnitTarget::NatsServer),
            reason: KeeperStepFailureReason::SupervisorWriteFailed,
            message,
        })) if message.as_str().contains("nats-server config")
    ));
    assert!(fs::read_to_string(&install_path).is_ok());
    assert!(!systemd_dir.join("nats-server.service").exists());
}

#[test]
fn local_effects_preserve_supervisor_start_failure_as_step_failure() {
    let root = temp_dir("ployz-keeper-local-systemctl-fail");
    let source = root.join("ployz-keeper-source");
    let install_path = root.join("bin/ployz-keeper");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan = bootstrap_script_plan(BootstrapScriptTarget::new(keeper_artifact(
        &source,
        &install_path,
    )));
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            fail_systemctl: Some(vec![
                "enable".to_owned(),
                "--now".to_owned(),
                "ployz-keeper.service".to_owned(),
            ]),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal,
        KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::StartSupervisorUnit(SupervisorUnitTarget::Keeper),
            reason: KeeperStepFailureReason::SupervisorStartFailed,
            message,
        })) if message.as_str() == "simulated systemctl failure"
    ));
    assert_eq!(fs::read_to_string(&install_path).unwrap(), "ployz\n");
    assert!(systemd_dir.join("ployz-keeper.service").exists());
}

#[test]
fn local_effects_render_role_units_from_the_artifact_installed_by_the_plan() {
    let root = temp_dir("ployz-keeper-local-plan-artifact-source");
    let source = root.join("ployzd-source");
    let install_path = root.join("plan/bin/ployzd");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    write_nats_config(&root);

    let plan = first_node_install_plan(
        FirstNodeInstallTarget::new(
            node_id("node_1"),
            ployzd_artifact(&source, &install_path),
            FirstNodeGateway::Skip,
        )
        .with_nats_server_unit(nats_unit(&root)),
    );
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    let control_unit = fs::read_to_string(systemd_dir.join("ployzd-control.service")).unwrap();
    assert!(control_unit.contains(install_path.to_str().expect("path is utf-8")));
    assert!(!control_unit.contains("ployzd-config-source"));
}

#[test]
fn local_join_redeems_token_then_installs_assigned_roles() {
    let root = temp_dir("ployz-keeper-local-join");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("ployzd-source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let target = KeeperJoinTarget::new(
        RedactedJoinMaterial::new(node_id("node_2"), "prod").expect("valid join material"),
        ployzd_artifact(&source, &root.join("join/bin/ployzd")),
        NonEmptyRoleSet::try_new(vec![ployz_core::roles::DaemonProcessRole::Node(node_id(
            "node_2",
        ))])
        .expect("non-empty role set"),
    );
    let mut redeemer = StaticJoinRedeemer {
        expected_token: JoinToken::try_new("join_once").expect("valid join token"),
        target,
    };
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &JoinToken::try_new("join_once").expect("valid join token"),
        &mut redeemer,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    assert_eq!(
        fs::read_to_string(root.join("state").join(JOIN_MATERIAL_FILE))
            .expect("join material is stored"),
        "node_id=node_2\ncluster_name=prod\n"
    );
    assert!(root.join("join/bin/ployzd").exists());
    assert!(systemd_dir.join("ployzd-node-node_2.service").exists());
    assert_eq!(token_consumer.consumed, 1);
    assert_eq!(
        effects.runner().systemctl_calls,
        vec![
            vec!["daemon-reload".to_owned()],
            vec![
                "enable".to_owned(),
                "--now".to_owned(),
                "ployzd-node-node_2.service".to_owned(),
            ],
        ]
    );
}

#[test]
fn local_effects_store_redacted_join_material() {
    let root = temp_dir("ployz-keeper-local-join-material");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let material =
        RedactedJoinMaterial::new(node_id("node_2"), "prod").expect("valid join material");
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );

    effects
        .apply_step(&KeeperStep::StoreJoinMaterial(material.clone()))
        .expect("join material stores");
    effects
        .apply_step(&KeeperStep::StoreJoinMaterial(material))
        .expect("join material stores idempotently");

    assert_eq!(
        fs::read_to_string(root.join("state").join(JOIN_MATERIAL_FILE))
            .expect("join material is stored"),
        "node_id=node_2\ncluster_name=prod\n"
    );
}

#[derive(Debug, Default)]
struct RecordingTokenConsumer {
    consumed: usize,
}

impl KeeperJoinTokenConsumer for RecordingTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        self.consumed += 1;
        Ok(())
    }
}

#[derive(Debug)]
struct StaticJoinRedeemer {
    expected_token: JoinToken,
    target: KeeperJoinTarget,
}

impl KeeperJoinRedeemer for StaticJoinRedeemer {
    fn redeem_join_token(&mut self, token: &JoinToken) -> Result<KeeperJoinTarget, FailureMessage> {
        if *token != self.expected_token {
            return Err(failure_message("unexpected join token"));
        }

        Ok(self.target.clone())
    }
}

#[derive(Debug)]
struct RecordingRunner {
    linux: bool,
    uid: u32,
    systemctl_calls: Vec<Vec<String>>,
    fail_systemctl: Option<Vec<String>>,
    downloads: Vec<RecordedDownload>,
    download_body: Option<Vec<u8>>,
    fail_download: Option<String>,
}

impl RecordingRunner {
    fn root_linux() -> Self {
        Self {
            linux: true,
            uid: 0,
            systemctl_calls: Vec::new(),
            fail_systemctl: None,
            downloads: Vec::new(),
            download_body: None,
            fail_download: None,
        }
    }
}

impl KeeperCommandRunner for RecordingRunner {
    fn is_linux(&mut self) -> bool {
        self.linux
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(self.uid)
    }

    fn systemctl(&mut self, args: &[&str]) -> Result<(), FailureMessage> {
        let call = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        self.systemctl_calls.push(call.clone());
        if self.fail_systemctl.as_ref() == Some(&call) {
            return Err(failure_message("simulated systemctl failure"));
        }
        Ok(())
    }

    fn download(&mut self, url: &str, destination: &Path) -> Result<(), FailureMessage> {
        if self.fail_download.as_deref() == Some(url) {
            fs::write(destination, b"partial")
                .map_err(|error| failure_message(&format!("failed fake partial write: {error}")))?;
            self.downloads.push(RecordedDownload {
                url: url.to_owned(),
                destination: destination.to_path_buf(),
            });
            return Err(failure_message("simulated download failure"));
        }
        let body = self
            .download_body
            .as_deref()
            .ok_or_else(|| failure_message("missing fake artifact body"))?;
        fs::write(destination, body)
            .map_err(|error| failure_message(&format!("failed fake artifact write: {error}")))?;
        self.downloads.push(RecordedDownload {
            url: url.to_owned(),
            destination: destination.to_path_buf(),
        });
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct RecordedDownload {
    url: String,
    destination: PathBuf,
}

impl RecordedDownload {
    fn is_cleaned_up(&self) -> bool {
        !self.destination.exists()
            && self
                .destination
                .parent()
                .is_none_or(|parent| !parent.exists())
    }
}

#[derive(Default)]
struct RecordingRecorder {
    events: Vec<KeeperStepEvent>,
}

impl KeeperStepRecorder for RecordingRecorder {
    fn record_step_event(&mut self, event: &KeeperStepEvent) -> Result<(), FailureMessage> {
        self.events.push(event.clone());
        Ok(())
    }
}

fn local_config(root: &Path, systemd_dir: &Path) -> KeeperLocalConfig {
    KeeperLocalConfig {
        systemd_dir: systemd_dir.to_path_buf(),
        state_dir: root.join("state"),
    }
}

fn write_nats_config(root: &Path) {
    let config_path = root.join("etc/nats-server.conf");
    fs::create_dir_all(config_path.parent().expect("config path has parent"))
        .expect("nats config parent can be created");
    fs::write(config_path, "jetstream: true\n").expect("nats config can be written");
}

fn nats_unit(root: &Path) -> NatsServerUnitTarget {
    NatsServerUnitTarget::new(
        root.join("bin/nats-server"),
        root.join("etc/nats-server.conf"),
    )
    .expect("valid nats-server unit target")
}

fn keeper_artifact(source: &Path, install_path: &Path) -> KeeperArtifactTarget {
    KeeperArtifactTarget::new(
        version("0.1.0"),
        artifact_source(source),
        digest(PLOYZ_NEWLINE_SHA256),
        install_path.to_path_buf(),
    )
    .expect("valid keeper artifact")
}

fn ployzd_artifact(source: &Path, install_path: &Path) -> PloyzdArtifactTarget {
    PloyzdArtifactTarget::new(
        version("0.1.0"),
        artifact_source(source),
        digest(PLOYZ_NEWLINE_SHA256),
        install_path.to_path_buf(),
    )
    .expect("valid ployzd artifact")
}

fn artifact_source(path: &Path) -> ArtifactSource {
    ArtifactSource::try_new(path.to_str().expect("temp path is utf-8")).expect("valid source")
}

fn version(value: &str) -> ArtifactVersion {
    ArtifactVersion::try_new(value).expect("valid version")
}

fn digest(value: &str) -> Sha256Digest {
    Sha256Digest::try_new(value).expect("valid digest")
}

fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn temp_dir(prefix: &str) -> PathBuf {
    let unique = format!(
        "{}-{}",
        prefix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    fs::create_dir_all(&path).expect("temp dir can be created");
    path
}

const PLOYZ_NEWLINE_SHA256: &str =
    "2dcc3bb1142455239d3b3391d9569a8ce0fbdfb906cd0434329e5dd736592138";
