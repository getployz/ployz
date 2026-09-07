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
                "machine",
                "rm",
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
        assert!(stdout.contains("Removed Machine one"), "{stdout}");
        assert!(
            stdout.contains(
                "Volumes losing access through the cluster (1). Their data will not be erased:"
            ),
            "{stdout}"
        );
        assert!(
            !stdout.contains("Permanently delete") && !stdout.contains("Deleted volume"),
            "{stdout}"
        );
        if invalid_config {
            assert!(
                stderr.contains("local context cleanup failed after Machine removal"),
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
            "machine",
            "rm",
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
