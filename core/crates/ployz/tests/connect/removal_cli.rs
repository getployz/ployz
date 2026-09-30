use super::*;

#[tokio::test]
async fn machine_removal_reports_complete_and_partial_results() {
    for (warning, invalid_config) in [
        (None, false),
        (Some("Docker reset failed: busy volume"), false),
        (None, true),
        (Some("Docker reset failed: busy volume"), true),
    ] {
        let service = DiscoveryService::new(test_description());
        *service.reset_warning.lock().unwrap() = warning.map(str::to_owned);
        let resets = service.reset_machines.clone();
        let erased = service.removed_volumes.clone();
        let (address, server) = serve_discovery(service).await;
        let config = std::env::temp_dir().join(format!(
            "ployz-removal-invalid-{}.yaml",
            MachineId::random()
        ));
        if invalid_config {
            std::fs::write(&config, "contexts: [invalid]").unwrap();
        }
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args([
                "--connect",
                &format!("tcp://{address}"),
                "--ployz-config",
                config.to_str().unwrap(),
                "server",
                "rm",
                "one",
                "--confirm",
                "one",
                "--accept-volume-loss",
                "data",
            ])
            .output()
            .await
            .unwrap();
        if invalid_config {
            std::fs::remove_file(config).unwrap();
        }
        assert_eq!(
            output.status.success(),
            warning.is_none() && !invalid_config,
            "{output:?}"
        );
        assert_eq!(*resets.lock().unwrap(), [machine_id('a')]);
        assert!(erased.lock().unwrap().is_empty());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("Removed Server one"), "{stdout}");
        assert!(
            stdout.contains(
                "Volumes the Cluster loses (1); only the Server's disk keeps their data:"
            ),
            "{stdout}"
        );
        assert!(
            !stdout.contains("Permanently delete") && !stdout.contains("Deleted volume"),
            "{stdout}"
        );
        if invalid_config {
            assert!(
                stderr.contains("local context cleanup failed after Server removal"),
                "{stderr}"
            );
        }
        if let Some(warning) = warning {
            assert!(stderr.contains(warning), "{stderr}");
            assert!(!stdout.contains("Deleted volume"), "{stdout}");
        } else {
            assert!(
                stdout.contains("Volume data was not erased by reset: data on"),
                "{stdout}"
            );
        }
        assert!(!stderr.contains("No changes made"), "{stderr}");
        server.abort();
    }
}

#[tokio::test]
async fn machine_reset_refuses_failed_service_observation_before_mutation() {
    let service = DiscoveryService::new(test_description());
    service.container_list_outcomes.lock().unwrap().insert(
        machine_id('a'),
        VecDeque::from([Err(Status::internal("container inventory failed"))]),
    );
    let resets = service.reset_machines.clone();
    let removals = service.removed_machines.clone();
    let (address, server) = serve_discovery(service).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "server",
            "rm",
            "one",
            "--confirm",
            "one",
            "--accept-volume-loss",
            "data",
        ])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(resets.lock().unwrap().is_empty());
    assert!(removals.lock().unwrap().is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("container inventory failed"), "{stderr}");
    assert!(stderr.contains(machine_id('a').as_str()), "{stderr}");
    assert!(stderr.contains("No changes made"), "{stderr}");
    server.abort();
}

#[tokio::test]
async fn server_removal_without_the_typed_name_names_what_goes_and_the_retry() {
    let service = DiscoveryService::new(test_description());
    let resets = service.reset_machines.clone();
    let removals = service.removed_machines.clone();
    let (address, server) = serve_discovery(service).await;
    let connect = format!("tcp://{address}");
    let run = |extra: &'static [&'static str]| {
        let connect = connect.clone();
        async move {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
                .args(["--json", "--connect", &connect, "server", "rm", "one"])
                .args(extra)
                .output()
                .await
                .unwrap()
        }
    };

    let output = run(&[]).await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let at = |pointer: &str| error.pointer(pointer).unwrap().clone();
    assert_eq!(at("/error/code"), "confirmation_required", "{error}");
    assert_eq!(at("/error/details/server/name"), "one", "{error}");
    assert_eq!(at("/error/details/data_loss/0/id/name"), "data", "{error}");
    let next = at("/error/details/next");
    let next = next.as_str().unwrap();
    assert!(next.starts_with("ployz server rm one --connect"), "{next}");
    assert!(
        next.ends_with("--confirm one --accept-volume-loss data"),
        "{next}"
    );

    let output = run(&["--confirm", "One"]).await;
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        error.pointer("/error/code").unwrap(),
        "invalid_argument",
        "{error}"
    );

    assert!(resets.lock().unwrap().is_empty());
    assert!(removals.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn last_cloud_managed_server_is_refused_before_any_confirmation() {
    let service = DiscoveryService::new(test_description());
    *service.management_clients.lock().unwrap() =
        vec![ployz_core::ManagementClientLabel::parse("cloud").unwrap()];
    let resets = service.reset_machines.clone();
    let (address, _server) = serve_discovery(service).await;
    let config = std::env::temp_dir().join(format!("ployz-last-{}.yaml", MachineId::random()));
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "--ployz-config",
            config.to_str().unwrap(),
            "--json",
            "server",
            "rm",
            "one",
        ])
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        error.pointer("/error/code"),
        Some(&json!("conflict")),
        "{error}"
    );
    assert_eq!(
        error.pointer("/error/details/next"),
        Some(&json!("ployz org rm ORGANIZATION"))
    );
    assert!(resets.lock().unwrap().is_empty());
}
