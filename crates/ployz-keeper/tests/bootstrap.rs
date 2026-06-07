use std::path::PathBuf;
use std::{env, fs};

use ployz_core::ids::NodeId;
use ployz_core::roles::{DaemonProcessRole, FirstNodeGateway, TunnelSide};
use ployz_keeper::artifacts::{
    ArtifactKind, ArtifactSource, ArtifactTarget, ArtifactTargetError, ArtifactVersion,
    KeeperArtifactTarget, PloyzdArtifactTarget, Sha256Digest,
};
use ployz_keeper::cli::load_startup;
use ployz_keeper::steps::{
    BootstrapScriptTarget, FirstNodeInstallTarget, HostPrerequisite, JoinToken, KeeperJoinTarget,
    KeeperStep, NonEmptyRoleSet, RedactedJoinMaterial, RoleSetError, bootstrap_script_plan,
    first_node_install_plan, keeper_join_plan,
};
use ployz_keeper::systemd::SupervisorUnitTarget;

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
            KeeperStep::VerifyArtifact(ArtifactTarget::Keeper(keeper_artifact())),
            KeeperStep::InstallArtifact(ArtifactTarget::Keeper(keeper_artifact())),
            KeeperStep::WriteSupervisorUnit(SupervisorUnitTarget::Keeper),
            KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::Keeper),
        ]
    );
}

#[test]
fn bootstrap_script_file_installs_only_keeper() {
    let script_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("scripts")
        .join("ployz.sh");
    let script = fs::read_to_string(script_path).expect("script is readable");

    assert_eq!(
        shell_keeper_unit_template(&script),
        "[Unit]\nDescription=Ployz Keeper\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart=${keeper_bin}${keeper_args}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    assert!(script.contains("PLOYZ_KEEPER_URL"));
    assert!(script.contains("PLOYZ_JOIN_TOKEN"));
    assert!(script.contains("join-token"));
    assert!(script.contains("not both"));
    assert!(script.contains("[join-token]"));
    assert!(script.contains("install -d -m 0700"));
    assert!(script.contains("umask 077"));
    assert!(script.contains("uname -s"));
    assert!(script.contains("id -u"));
    assert!(!script.contains("ployzd"));
    assert!(!script.contains("NATS"));
}

#[test]
fn keeper_startup_consumes_join_token_file_without_leaking_secret() {
    let token_file = unique_temp_path("ployz-keeper-join-token");
    fs::write(&token_file, "join_once\n").expect("join token file can be written");

    let startup = load_startup(vec![
        "--join-token-file".into(),
        token_file.as_os_str().to_os_string(),
    ])
    .expect("startup reads join token");
    let token = startup.join_token.expect("join token is loaded");

    assert_eq!(
        token,
        JoinToken::try_new("join_once").expect("expected token is valid")
    );
    assert_eq!(format!("{token:?}"), "JoinToken(\"[redacted]\")");
    assert!(!token_file.exists());
    assert!(
        load_startup(vec![
            "--join-token-file".into(),
            token_file.as_os_str().to_os_string(),
        ])
        .is_err()
    );
}

#[test]
fn keeper_join_installs_ployzd_and_only_assigned_role_units() {
    let roles = vec![
        DaemonProcessRole::Node(node_id("node_7")),
        DaemonProcessRole::Gateway,
        DaemonProcessRole::Tunnel(TunnelSide::Edge),
    ];
    let plan = keeper_join_plan(KeeperJoinTarget::new(
        JoinToken::try_new("join_once").expect("valid join token"),
        RedactedJoinMaterial::new(node_id("node_7"), "prod").expect("valid join material"),
        ployzd_artifact(),
        NonEmptyRoleSet::try_new(roles.clone()).expect("non-empty unique roles"),
    ));

    assert!(plan.installs_artifact_kind(ArtifactKind::Ployzd));
    assert!(plan.writes_ployzd_role_units());
    assert!(plan.steps().contains(&KeeperStep::RedeemJoinToken(
        JoinToken::try_new("join_once").expect("valid join token")
    )));
    assert!(plan.steps().contains(&KeeperStep::StoreJoinMaterial(
        RedactedJoinMaterial::new(node_id("node_7"), "prod").expect("valid join material")
    )));

    for role in roles {
        let unit = SupervisorUnitTarget::PloyzdRole(role);
        assert!(
            plan.steps()
                .contains(&KeeperStep::WriteSupervisorUnit(unit.clone()))
        );
        assert!(
            plan.steps()
                .contains(&KeeperStep::StartSupervisorUnit(unit))
        );
    }

    assert!(!plan.steps().contains(&KeeperStep::WriteSupervisorUnit(
        SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Control)
    )));
    assert!(!plan.steps().contains(&KeeperStep::WriteSupervisorUnit(
        SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Dns)
    )));
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
    assert!(plan.steps().contains(&KeeperStep::WriteSupervisorUnit(
        SupervisorUnitTarget::NatsServer
    )));
    assert!(plan.steps().contains(&KeeperStep::StartSupervisorUnit(
        SupervisorUnitTarget::NatsServer
    )));

    for role in [
        DaemonProcessRole::Tunnel(TunnelSide::Core),
        DaemonProcessRole::Control,
        DaemonProcessRole::Node(node_id),
    ] {
        let unit = SupervisorUnitTarget::PloyzdRole(role);
        assert!(
            plan.steps()
                .contains(&KeeperStep::WriteSupervisorUnit(unit.clone()))
        );
        assert!(
            plan.steps()
                .contains(&KeeperStep::StartSupervisorUnit(unit))
        );
    }

    assert!(!plan.steps().contains(&KeeperStep::WriteSupervisorUnit(
        SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Gateway)
    )));
    assert!(
        !plan
            .steps()
            .iter()
            .any(|step| matches!(step, KeeperStep::RedeemJoinToken(_)))
    );
}

#[test]
fn first_node_install_can_include_gateway_role() {
    let plan = first_node_install_plan(FirstNodeInstallTarget::new(
        node_id("node_1"),
        ployzd_artifact(),
        FirstNodeGateway::Install,
    ));

    assert!(plan.steps().contains(&KeeperStep::WriteSupervisorUnit(
        SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Gateway)
    )));
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

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
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
