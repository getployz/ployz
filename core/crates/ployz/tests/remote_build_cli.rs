//! Remote Build CLI admission, outcome rendering, and absence of local Docker I/O.

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
    let recorder = Arc::new(support::BuildRecorder {
        queued: true,
        ..Default::default()
    });
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
        // Plain --remote selects the only Machine that advertises Builds.
        vec!["--remote", "api"],
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Queued")
                && stderr.contains("queue wait:")
                && stderr.contains("execution:"),
            "{stderr}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(support::machine_id('a').as_str()),
            "{stdout}"
        );
    }
    assert_eq!(recorder.uploads.load(Ordering::SeqCst), 3);
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
        vec!["--remote=tower", "--local", "api"],
        vec!["--remote=missing", "api"],
        vec!["--remote", "--push", "api"],
    ] {
        let output = run(&args).output().await.unwrap();
        assert!(!output.status.success(), "{args:?}");
    }
    assert_eq!(
        recorder.uploads.load(Ordering::SeqCst),
        3,
        "refusals must not resubmit source"
    );
    assert!(
        !root.join("docker-called").exists(),
        "remote Build accessed local Docker"
    );
    let commit = "0123456789abcdef0123456789abcdef01234567";
    for build in [
        format!("context: ssh://git@example.test/private#{commit}"),
        format!("context: git@example.test:private#{commit}"),
        format!(
            "context: .\n      additional_contexts:\n        dependency: ssh://git@example.test/private#{commit}"
        ),
        format!(
            "context: .\n      additional_contexts:\n        - dependency=git@example.test:private#{commit}"
        ),
    ] {
        fs::write(root.join("compose.yaml"), format!("name: demo\nservices:\n  api:\n    image: example.test/api:built\n    build:\n      {build}\n")).unwrap();
        let output = run(&["--remote=tower", "api"])
            .env("SSH_AUTH_SOCK", root.join("client-agent.sock"))
            .output()
            .await
            .unwrap();
        assert!(
            !output.status.success(),
            "agent-dependent context was submitted"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("captured default SSH key"));
    }
    assert_eq!(
        recorder.uploads.load(Ordering::SeqCst),
        3,
        "SSH refusal must precede upload"
    );
    fs::write(root.join("key"), "captured-key").unwrap();
    fs::write(root.join("compose.yaml"), format!("name: demo\nservices:\n  api:\n    image: example.test/api:built\n    build:\n      context: ssh://git@example.test/private#{commit}\n      ssh:\n        - default=key\n")).unwrap();
    let output = run(&["--remote=tower", "api"]).output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(recorder.uploads.load(Ordering::SeqCst), 4);
    fs::write(
        root.join("compose.yaml"),
        "name: demo\nservices:\n  api:\n    build: {context: ., x-recipe: railpack}\n",
    )
    .unwrap();
    let output = run(&["--remote=tower", "api"]).output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(recorder.uploads.load(Ordering::SeqCst), 5);
    assert!(!root.join("docker-called").exists());
    // An explicit local Deploy reaches the CLI host even with a connected builder.
    fs::write(
        root.join("compose.yaml"),
        "name: demo\nservices: {api: {build: .}}\n",
    )
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .current_dir(&root)
        .env("PATH", &root)
        .env("HOME", &root)
        .env("PLOYZ_CONFIG", root.join("config.yaml"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "deploy",
            "--local",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        !output.status.success(),
        "the local Docker sentinel must fail"
    );
    assert!(
        root.join("docker-called").exists(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(recorder.uploads.load(Ordering::SeqCst), 5);
    server.abort();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn remote_queue_outcomes_name_machine_and_unattempted_work() {
    use ployz_build::{Stage, TargetEvidence, WorkEvidence, remote::Outcome};
    let root = std::env::temp_dir().join(format!("ployz-queue-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("compose.yaml"),
        "name: demo\nservices: {api: {build: .}}\n",
    )
    .unwrap();
    fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    for reason in [
        "queue full",
        "queue expired",
        "queue cancelled",
        "termination unknown",
        "Build acceptance revoked",
    ] {
        let mut description = support::test_description();
        description.machine_id = support::machine_id('a');
        description
            .capabilities
            .insert(BUILD_CAPABILITY.parse().unwrap());
        let mut service = support::DiscoveryService::new(description);
        service.machines = vec![support::machine('a', "tower")];
        let work = WorkEvidence(std::collections::BTreeMap::from([(
            "api".into(),
            TargetEvidence::Unattempted,
        )]));
        let outcome = if reason == "termination unknown" {
            Outcome::Unknown {
                stage: Stage::Admission,
                message: reason.into(),
                work,
            }
        } else {
            Outcome::Failed {
                stage: Stage::Queued,
                message: reason.into(),
                work,
            }
        };
        let recorder = Arc::new(support::BuildRecorder {
            queued: true,
            admission_outcome: Some(outcome),
            ..Default::default()
        });
        service.builds = Some(recorder.clone());
        let (address, server) = support::serve_discovery(service).await;
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .current_dir(&root)
            .env("PATH", &root)
            .env("HOME", &root)
            .env("PLOYZ_CONFIG", root.join("config.yaml"))
            .args([
                "--connect",
                &format!("tcp://{address}"),
                "build",
                "--remote",
                "api",
            ])
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(support::machine_id('a').as_str())
                && stderr.contains(reason)
                && stderr.contains("Unattempted"),
            "{stderr}"
        );
        assert!(
            stderr.contains("queue wait:") && stderr.contains("execution: 0.00s"),
            "{stderr}"
        );
        assert_eq!(recorder.uploads.load(Ordering::SeqCst), 0);
        assert_eq!(
            recorder.routes.lock().unwrap().len(),
            1,
            "rejected Builds must not be resubmitted"
        );
        server.abort();
    }
    fs::remove_dir_all(root).unwrap();
}

/// Automatic selection, an explicit pin, and the local override.
#[tokio::test]
async fn automatic_selection_uses_capable_machines_and_honors_explicit_flags() {
    let root = std::env::temp_dir().join(format!("ployz-auto-cli-{}", uuid::Uuid::new_v4()));
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
    fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    let mut builds = support::test_description();
    builds.machine_id = support::machine_id('a');
    builds
        .capabilities
        .insert(BUILD_CAPABILITY.parse().unwrap());
    let mut idle = support::test_description();
    idle.machine_id = support::machine_id('b');
    let capable = |description: ployz_core::ContractDescription| {
        let mut service = support::DiscoveryService::new(description);
        service.machines = vec![
            support::machine('a', "tower"),
            support::machine('b', "forge"),
        ];
        service
            .descriptions
            .insert(support::machine_id('b'), idle.clone());
        service.builds = Some(Arc::new(support::BuildRecorder::default()));
        service
    };
    let compose = |preference: &str| {
        fs::write(
            root.join("compose.yaml"),
            format!(
                "name: demo\n{preference}services:\n  api:\n    image: example.test/api:built\n    build: .\n"
            ),
        )
        .unwrap();
    };
    let run = |address: &std::net::SocketAddr, args: &[&str]| {
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
    let (address, server) = support::serve_discovery(capable(builds.clone())).await;
    // Automatic selection skips the Machine that does not advertise Builds, and
    // an explicit pin reaches the same Machine.
    compose("");
    for args in [vec!["--remote", "api"], vec!["--remote=tower", "api"]] {
        let output = tokio::time::timeout(Duration::from_secs(20), run(&address, &args).output())
            .await
            .unwrap()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("tower"), "{stdout}");
        assert!(
            stdout.contains(support::machine_id('a').as_str()),
            "{stdout}"
        );
    }
    // An ambiguous name refuses, naming every Machine it matched.
    let (ambiguous_address, ambiguous_server) = {
        let mut service = support::DiscoveryService::new(builds.clone());
        service.machines = vec![
            support::machine('a', "tower"),
            support::machine_named(&support::machine_id('b'), "tower"),
        ];
        support::serve_discovery(service).await
    };
    {
        let args = vec!["--remote=tower", "api"];
        let output = run(&ambiguous_address, &args).output().await.unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{stderr}");
        assert!(
            stderr.contains("ambiguous")
                && stderr.contains(support::machine_id('a').as_str())
                && stderr.contains(support::machine_id('b').as_str()),
            "{stderr}"
        );
    }
    ambiguous_server.abort();

    // Pinning the Machine that does not advertise Builds refuses with evidence.
    let output = run(&address, &["--remote=forge", "api"])
        .output()
        .await
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("forge") && stderr.contains("--local"),
        "{stderr}"
    );
    assert!(!root.join("docker-called").exists(), "{stderr}");
    // --local runs here even with a Cluster available.
    assert!(
        !run(&address, &["--local", "api"])
            .output()
            .await
            .unwrap()
            .status
            .success()
    );
    assert!(root.join("docker-called").exists());
    server.abort();

    // No visible Machine advertises Builds: every one of them answers without
    // the capability, so the refusal names them all.
    let mut refusing = capable(idle.clone());
    let mut without_builds = support::test_description();
    without_builds.machine_id = support::machine_id('a');
    refusing
        .descriptions
        .insert(support::machine_id('a'), without_builds);
    let (address, server) = support::serve_discovery(refusing).await;
    compose("");
    let output = run(&address, &["--remote", "api"]).output().await.unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("tower")
            && stderr.contains("forge")
            && stderr.contains("does not support remote Builds")
            && stderr.contains("--remote="),
        "{stderr}"
    );
    server.abort();
    fs::remove_dir_all(root).unwrap();
}
