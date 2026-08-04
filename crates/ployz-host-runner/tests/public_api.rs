use ployz_host_runner::lifecycle::machine_join::JoinToken;
use ployz_host_runner::{
    ArtifactKind, ReleaseManifest, SupervisorBackend, SystemHostRunnerCommandRunner,
    detect_host_platform,
};

#[test]
fn transport_free_host_mechanics_are_public() {
    let _runner = SystemHostRunnerCommandRunner::default();
    let _artifact = ArtifactKind::Ployzd;
    let _supervisor = SupervisorBackend::Systemd;
    let _manifest_parser = ReleaseManifest::parse;

    assert!(detect_host_platform("ID=ubuntu\n").is_ok());
    assert_eq!(
        JoinToken::try_new("join_secret_123")
            .expect("join token")
            .as_str(),
        "join_secret_123"
    );
}
