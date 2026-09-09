use ployz_core::{BUILD_CAPABILITY, MachineTarget, RoutingRequest};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

#[path = "connect/support.rs"]
#[allow(dead_code)]
mod support;

#[tokio::test]
async fn standalone_remote_build_never_invokes_local_docker_and_keeps_the_service_positional() {
    let root = std::env::temp_dir().join(format!("ployz-remote-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("docker"),
        format!(
            "#!/bin/sh\nprintf called > '{}'\nexit 99\n",
            root.join("docker-called").display()
        ),
    )
    .unwrap();
    fs::set_permissions(root.join("docker"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("compose.yaml"), "name: demo\nservices:\n  api:\n    image: example.test/api:built\n    build: .\n  other:\n    build: .\n").unwrap();
    fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    let mut description = support::test_description();
    description.machine_id = support::machine_id('a');
    description
        .capabilities
        .insert(BUILD_CAPABILITY.parse().unwrap());
    let mut service = support::DiscoveryService::new(description);
    service.machines = vec![support::machine('a', "tower")];
    let recorder = Arc::new(support::BuildRecorder::default());
    service.builds = Some(recorder.clone());
    let (address, server) = support::serve_discovery(service).await;
    let run = |args: &[&str]| {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .current_dir(&root)
            .env("PATH", &root)
            .env("HOME", &root)
            .env("PLOYZ_CONFIG", root.join("config.yaml"))
            .env("DOCKER_HOST", "unix:///missing-local-docker.sock")
            .args(["--connect", &format!("tcp://{address}"), "build"])
            .args(args);
        command
    };
    for args in [
        vec!["--remote=tower", "api"],
        vec!["--remote=tower", "--check", "api"],
    ] {
        let output = tokio::time::timeout(Duration::from_secs(20), run(&args).output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(support::machine_id('a').as_str()),
            "{stdout}"
        );
    }
    assert_eq!(recorder.uploads.load(Ordering::SeqCst), 2);
    assert!(
        recorder
            .targets
            .lock()
            .unwrap()
            .iter()
            .all(|targets| targets == &["api"])
    );
    assert!(
        recorder
            .routes
            .lock()
            .unwrap()
            .iter()
            .all(|route| route
                == &RoutingRequest::One(MachineTarget::from(&support::machine_id('a'))))
    );
    for args in [
        vec!["--remote", "api"],
        vec!["--remote=tower", "--local", "api"],
        vec!["--remote=missing", "api"],
    ] {
        let output = run(&args).output().await.unwrap();
        assert!(!output.status.success());
    }
    assert_eq!(
        recorder.uploads.load(Ordering::SeqCst),
        2,
        "refusals must not resubmit source"
    );
    assert!(
        !root.join("docker-called").exists(),
        "remote Build accessed local Docker"
    );
    server.abort();
    fs::remove_dir_all(root).unwrap();
}
