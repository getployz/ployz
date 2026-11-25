use super::*;

#[tokio::test]
async fn a_server_whose_daemon_predates_kept_volumes_says_it_needs_an_upgrade() {
    let mut description = test_description();
    description.machine_id = machine_id('b');
    let mut older = DiscoveryService::new(description);
    older.machines = vec![machine('a', "one"), machine('b', "two")];
    let (address, _two) = serve_discovery(older).await;
    let config = std::env::temp_dir().join(format!("ployz-kept-{}.yaml", MachineId::random()));
    std::fs::write(
        &config,
        format!("contexts:\n  lab:\n    connections:\n      - tcp: {address}\n"),
    )
    .unwrap();
    let run = |args: &'static [&'static str]| {
        let config = config.clone();
        async move {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
                .arg("--why=test")
                .env_remove("PLOYZ_TOKEN")
                .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
                .args(["--ployz-config", config.to_str().unwrap(), "--json"])
                .args(args)
                .args(["--context", "lab"])
                .output()
                .await
                .unwrap()
        }
    };

    for args in [
        &["server", "volumes", "two"][..],
        &["server", "volumes", "two", "prune", "--yes"][..],
    ] {
        let output = run(args).await;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result.pointer("/error/message"),
            Some(&json!(
                "two needs a daemon upgrade to list its kept volumes."
            )),
            "{args:?} {result}"
        );
        assert_eq!(result.pointer("/error/code"), Some(&json!("unsupported")));
        assert_eq!(
            result.pointer("/error/details/capability"),
            Some(&json!(ployz_core::LIST_KEPT_VOLUMES_CAPABILITY))
        );
    }
    let _ = std::fs::remove_file(&config);
}

#[tokio::test]
async fn a_prune_age_that_is_empty_zero_or_past_the_calendar_deletes_nothing() {
    let config = std::env::temp_dir().join(format!("ployz-kept-{}.yaml", MachineId::random()));
    std::fs::write(
        &config,
        "contexts:\n  lab:\n    connections:\n      - tcp: 127.0.0.1:9\n",
    )
    .unwrap();
    for age in [
        "--older-than=",
        "--older-than=0s",
        "--older-than=100000000d",
    ] {
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--why=test")
            .env_remove("PLOYZ_TOKEN")
            .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
            .args(["--ployz-config", config.to_str().unwrap(), "--json"])
            .args(["server", "volumes", "two", "prune", age, "--yes"])
            .args(["--context", "lab"])
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{age}: {output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result.pointer("/error/code"),
            Some(&json!("invalid_argument")),
            "{age}: {result}"
        );
    }
    let _ = std::fs::remove_file(&config);
}
