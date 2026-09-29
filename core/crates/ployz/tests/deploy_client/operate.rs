//! Operating the selected Environment: `ps`, `exec` and `service start|stop|restart`
//! map a bare Service name to the Environment's Namespace, which the hidden
//! in-process Config Store (`PLOYZ_STORE`) fixes.

#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; a missing entry must fail the test."
)]

use super::*;
use serde_json::Value;

/// `ployz --json ARGS` in its own home, away from any ambient sign-in, Store or link.
async fn ployz(store: Option<&std::path::Path>, address: &str, args: &[&str]) -> (i32, Value) {
    let home = tempfile::tempdir().unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("PLOYZ_CONFIG", home.path().join("config.yaml"))
        .env_remove("PLOYZ_STORE")
        .env_remove("PLOYZ_TOKEN")
        .env_remove("PLOYZ_CLOUD_URL")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV");
    if let Some(store) = store {
        command.env("PLOYZ_STORE", format!("sqlite:{}", store.display()));
    }
    let output = command
        .args(["--json", "--connect", &format!("tcp://{address}")])
        .args(args)
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = serde_json::Deserializer::from_str(&stdout)
        .into_iter::<Value>()
        .last()
        .and_then(Result::ok)
        .unwrap_or_else(|| {
            panic!(
                "{args:?}: {stdout} / {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
    (output.status.code().unwrap(), json)
}

fn services(result: &Value, key: &str) -> Vec<String> {
    result[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["service"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_bare_service_name_means_the_selected_environments_service() {
    let store = tempfile::tempdir().unwrap();
    let db = store.path().join("store.db");
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone());
    service.listed_containers().lock().unwrap().extend([
        running_container_in(&machine, &spec("web"), "shop-production", '1'),
        running_container_in(&machine, &spec("web"), "shop-staging", '2'),
    ]);
    let (address, server) = listening(service).await;
    let address = address.to_string();
    for args in [&["project", "new", "shop"][..], &["env", "new", "staging"]] {
        assert_eq!(ployz(Some(&db), &address, args).await.0, 0, "{args:?}");
    }

    let (code, ps) = ployz(Some(&db), &address, &["ps", "--env", "staging"]).await;
    assert_eq!(code, 0, "{ps}");
    let listed = ps["containers"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "{ps}");
    assert_eq!(listed[0]["namespace"], "shop-staging", "{ps}");

    for (scope, expected) in [
        (&["--env", "staging"][..], "shop-staging/web"),
        (&[], "shop-production/web"),
    ] {
        let mut args = vec!["service", "stop", "web"];
        args.extend(scope);
        let (code, stopped) = ployz(Some(&db), &address, &args).await;
        assert_eq!(code, 0, "{stopped}");
        assert_eq!(services(&stopped, "containers"), [expected], "{stopped}");
    }

    // Without a Config Store the name is the whole Cluster's, where it's ambiguous.
    let (code, error) = ployz(None, &address, &["service", "stop", "web"]).await;
    assert_ne!(code, 0);
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("matches multiple Services"),
        "{error}"
    );
    server.abort();
}

#[tokio::test]
async fn a_server_that_fails_to_answer_makes_the_result_partial() {
    let one = machine('a', "one");
    let two = machine('b', "two");
    let service = DeployService::new(one.clone())
        .with_machines(vec![one.clone(), two.clone()])
        .fail_listing_on(two.machine.id);
    service
        .listed_containers()
        .lock()
        .unwrap()
        .push(running_container(&one, &spec("web")));
    let (address, server) = listening(service).await;
    let address = address.to_string();

    let (code, ps) = ployz(None, &address, &["ps"]).await;
    assert_eq!(code, 3, "{ps}");
    assert_eq!(ps["containers"].as_array().unwrap().len(), 1, "{ps}");
    assert_eq!(ps["failures"][0]["machine_id"], two.machine.id.to_string());

    let (code, stopped) = ployz(None, &address, &["service", "stop", "web"]).await;
    assert_eq!(code, 3, "{stopped}");
    assert_eq!(services(&stopped, "containers"), ["app/web"], "{stopped}");
    assert_eq!(stopped["next"], "ployz ps", "{stopped}");
    server.abort();
}

#[tokio::test]
async fn restart_stops_then_starts_and_reports_a_start_that_failed() {
    for fail in [false, true] {
        let machine = machine('a', "one");
        let mut service = DeployService::new(machine.clone());
        if fail {
            service = service.fail_starts("no start");
        }
        service
            .listed_containers()
            .lock()
            .unwrap()
            .push(running_container(&machine, &spec("web")));
        let (address, server) = listening(service).await;

        let (code, restarted) =
            ployz(None, &address.to_string(), &["service", "restart", "web"]).await;
        let actions = restarted["containers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["action"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        if fail {
            assert_eq!(code, 3, "{restarted}");
            assert_eq!(actions, ["stop"], "{restarted}");
            assert_eq!(
                restarted["container_failures"][0]["error"]["message"],
                "no start"
            );
            assert_eq!(restarted["next"], "ployz ps", "{restarted}");
        } else {
            assert_eq!(code, 0, "{restarted}");
            assert_eq!(actions, ["stop", "start"], "{restarted}");
            assert!(restarted.get("next").is_none(), "{restarted}");
        }
        server.abort();
    }
}
