#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use std::{fs, os::unix::fs::PermissionsExt, sync::atomic::Ordering};

#[allow(dead_code)]
#[path = "deploy_client/support.rs"]
mod support;
use support::*;

#[tokio::test]
async fn changes_cli_selects_compose_and_reports_human_and_json_without_side_effects() {
    let root = std::env::temp_dir().join(format!("ployz-changes-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("docker"),
        "#!/bin/sh\ntouch docker-ran\nexit 99\n",
    )
    .unwrap();
    fs::set_permissions(root.join("docker"), fs::Permissions::from_mode(0o755)).unwrap();
    let yaml = "name: app\nservices:\n  web:\n    image: web\n    command: [captured]\n    build: .\n    depends_on: [db]\n    environment: {TOKEN: 'secret://token'}\n  db: {image: postgres, profiles: [data]}\n  unselected: {image: alpine}\nsecrets: {token: {x-command: 'touch provider-ran; printf secret'}}\n";
    fs::write(root.join("chosen.yaml"), yaml).unwrap();
    fs::write(root.join("config.yaml"), "contexts: {}\n").unwrap();
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone());
    let mutations = service.mutating_rpcs();
    let mut old = spec("web");
    old.container.command = vec!["old".into()];
    let mut retired = running_container(&machine, &spec("retired")).into_parts();
    retired.container_id = ployz_core::ContainerId::parse("2".repeat(64)).unwrap();
    service.listed_containers().lock().unwrap().extend([
        running_container(&machine, &old),
        retired.try_into().unwrap(),
    ]);
    let (address, server) = listening(service).await;
    for json in [true, false] {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .current_dir(&root)
            .env_remove("PLOYZ_CONTEXT")
            .env_remove("COMPOSE_FILE")
            .env_remove("COMPOSE_PROJECT_NAME")
            .env(
                "PATH",
                format!("{}:{}", root.display(), std::env::var("PATH").unwrap()),
            )
            .args([
                "--connect",
                &format!("tcp://{address}"),
                "--ployz-config",
                "config.yaml",
                "changes",
                "--project-name",
                "app",
                "-f",
                "chosen.yaml",
                "--profile",
                "data",
                "web",
            ]);
        if json {
            command.args(["-o", "json"]);
        }
        let output = command.output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        if json {
            let review: serde_json::Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(review["project_name"], "app");
            assert_eq!(review["would_remove"][0], "app/retired");
            assert_eq!(review["prune_refusal"], "selected_services");
            assert_eq!(review["selection"][0]["name"], "web");
            assert_eq!(
                review["compared_settings"],
                serde_json::json!(ployz_core::COMPARED_SERVICE_SETTINGS)
            );
            assert_eq!(
                review["observer_machine_id"],
                machine.machine.id.to_string()
            );
            assert_eq!(review["services"].as_array().unwrap().len(), 2);
            assert_eq!(
                review["services"][1]["observations"][0]["changes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|change| change["setting"] == "command")
                    .unwrap()["after"],
                serde_json::json!(["captured"])
            );
        } else {
            assert!(
                stdout.contains("Review coverage: service settings"),
                "{stdout}"
            );
            assert!(
                stdout.contains("old") && stdout.contains("captured"),
                "{stdout}"
            );
            assert!(stdout.contains("no observed Container"), "{stdout}");
            assert!(stdout.contains("retired: preserved."), "{stdout}");
        }
        assert!(!stdout.contains("secret://token") && !stdout.contains("provider-ran"));
    }
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
    assert!(!root.join("provider-ran").exists());
    assert!(!root.join("docker-ran").exists());
    assert_eq!(fs::read_to_string(root.join("chosen.yaml")).unwrap(), yaml);
    let mut entries = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries, ["chosen.yaml", "config.yaml", "docker"]);
    server.abort();
    fs::remove_dir_all(root).unwrap();
}
