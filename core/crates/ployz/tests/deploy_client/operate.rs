//! Operating the selected Environment: `ps`, `exec` and `service start|stop|restart`
//! map a bare Service name to the Environment's Namespace, which the hidden
//! in-process Config Store (`PLOYZ_STORE`) fixes.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; a missing entry must fail the test."
)]

use super::*;
use serde_json::Value;

/// `ployz ARGS` in its own home, away from any ambient sign-in, Store or link.
async fn run(
    store: Option<&std::path::Path>,
    address: &str,
    args: &[&str],
) -> std::process::Output {
    let home = tempfile::tempdir().unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .current_dir(home.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env("PLOYZ_CONFIG", home.path().join("config.yaml"))
        .env_remove("PLOYZ_STORE")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV");
    if let Some(store) = store {
        command.env("PLOYZ_STORE", format!("sqlite:{}", store.display()));
    }
    command
        .args(["--connect", &format!("tcp://{address}")])
        .args(args)
        .output()
        .await
        .unwrap()
}

/// `ployz --json ARGS`, as [`run`].
async fn ployz(store: Option<&std::path::Path>, address: &str, args: &[&str]) -> (i32, Value) {
    let output = run(store, address, &[&["--json"], args].concat()).await;
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
    for args in [
        &["project", "new", "shop"][..],
        &["env", "new", "staging"],
        &[
            "service",
            "add",
            "web",
            "--image",
            "nginx:1",
            "--env",
            "production",
        ],
        &[
            "service", "add", "web", "--image", "nginx:1", "--env", "staging",
        ],
    ] {
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
async fn restart_reports_one_restart_or_the_start_that_failed() {
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
            .push(deployed_container(&machine, &spec("web")));
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
            assert!(actions.is_empty(), "{restarted}");
            assert_eq!(
                restarted["container_failures"][0]["error"]["message"],
                "no start"
            );
            assert_eq!(restarted["next"], "ployz ps", "{restarted}");
        } else {
            assert_eq!(code, 0, "{restarted}");
            assert_eq!(actions, ["restart"], "{restarted}");
            assert!(restarted.get("next").is_none(), "{restarted}");
        }
        server.abort();
    }
}

#[tokio::test]
async fn restart_returns_once_a_slow_start_passes_its_healthcheck() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone()).turn_healthy_on_release();
    service
        .listed_containers()
        .lock()
        .unwrap()
        .push(deployed_container(&machine, &spec("web")));
    let (address, server) = listening(service.clone()).await;

    let restart = tokio::spawn(async move {
        ployz(None, &address.to_string(), &["service", "restart", "web"]).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(30), service.health_waited())
        .await
        .expect("the restart never waited for health");
    assert!(!restart.is_finished(), "returned before the start passed");
    service.release_health();
    let (code, restarted) = restart.await.unwrap();
    assert_eq!(code, 0, "{restarted}");
    server.abort();
}

#[tokio::test]
async fn restart_reports_a_start_that_never_passes_its_healthcheck() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone())
        .hold_health()
        .with_effective_healthcheck(ployz_core::HealthcheckSpec::Configured(
            ployz_core::ConfiguredHealthcheck {
                test: ployz_core::HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
                interval_millis: None,
                timeout_millis: None,
                start_period_millis: None,
                start_interval_millis: None,
                retries: None,
                deadline_millis: Some(1_000),
            },
        ));
    service
        .listed_containers()
        .lock()
        .unwrap()
        .push(deployed_container(&machine, &spec("web")));
    let (address, server) = listening(service).await;

    let (code, restarted) = ployz(None, &address.to_string(), &["service", "restart", "web"]).await;
    assert_eq!(code, 3, "{restarted}");
    assert_eq!(
        restarted["container_failures"][0]["error"]["details"]["type"], "timed_out",
        "{restarted}"
    );
    assert_eq!(restarted["next"], "ployz ps", "{restarted}");
    server.abort();
}

#[tokio::test]
async fn server_clean_removes_only_a_namespace_no_environment_owns() {
    let store = tempfile::tempdir().unwrap();
    let db = store.path().join("store.db");
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone());
    service.listed_containers().lock().unwrap().extend([
        running_container_in(&machine, &spec("web"), "shop-production", '1'),
        running_container_in(&machine, &spec("web"), "left-over", '2'),
    ]);
    let (address, server) = listening(service).await;
    let address = address.to_string();
    for args in [
        &["project", "new", "shop"][..],
        &["service", "add", "web", "--image", "web:1"],
    ] {
        assert_eq!(ployz(Some(&db), &address, args).await.0, 0, "{args:?}");
    }
    // A Deployment fixes the Environment's Namespace, whatever its outcome.
    ployz(Some(&db), &address, &["deploy"]).await;

    let (code, listed) = ployz(Some(&db), &address, &["server", "clean"]).await;
    assert_eq!(code, 0, "{listed}");
    assert_eq!(
        listed["namespaces"][0]["namespace"], "left-over",
        "{listed}"
    );
    assert_eq!(
        listed["namespaces"].as_array().unwrap().len(),
        1,
        "{listed}"
    );
    assert_eq!(
        listed["next"],
        format!(
            "ployz server clean --connect tcp://{address} --namespace left-over --confirm left-over"
        )
    );
    assert_eq!(listed["omitted"], serde_json::json!([]), "{listed}");

    let owned = ["server", "clean", "--namespace", "shop-production"];
    let (code, refused) = ployz(Some(&db), &address, &owned).await;
    assert_eq!(code, 1, "{refused}");
    assert_eq!(refused["error"]["code"], "conflict", "{refused}");
    assert_eq!(
        refused["error"]["details"]["next"],
        "ployz env rm production --project shop"
    );

    let (code, unconfirmed) = ployz(
        Some(&db),
        &address,
        &["server", "clean", "--namespace", "left-over"],
    )
    .await;
    assert_eq!(code, 2, "{unconfirmed}");
    assert_eq!(
        unconfirmed["error"]["code"], "confirmation_required",
        "{unconfirmed}"
    );
    assert_eq!(
        unconfirmed["error"]["details"]["namespace"], "left-over",
        "{unconfirmed}"
    );
    let (code, _) = ployz(
        Some(&db),
        &address,
        &[
            "server",
            "clean",
            "--namespace",
            "left-over",
            "--confirm",
            "left",
        ],
    )
    .await;
    assert_eq!(code, 2);

    let confirmed = [
        "server",
        "clean",
        "--namespace",
        "left-over",
        "--confirm",
        "left-over",
    ];
    let (code, cleaned) = ployz(Some(&db), &address, &confirmed).await;
    assert_eq!(code, 0, "{cleaned}");
    assert_eq!(cleaned["namespace"], "left-over", "{cleaned}");
    server.abort();
}

#[tokio::test]
async fn local_deploy_failure_recovery_keeps_explicit_connection_and_context() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("store.db");
    let selected = root.path().join("selected team.yaml");
    let machine = machine('a', "one");
    let daemon = DeployService::new(machine.clone()).fail_create_volume("create denied");
    let (address, server) = listening(daemon).await;
    let connect = format!("tcp://{address}");
    std::fs::write(
        &selected,
        "current_context: staging\ncontexts:\n  staging:\n    connections: [tcp://127.0.0.1:1]\n",
    )
    .unwrap();
    let invoke = |args: Vec<&'static str>| {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .current_dir(root.path())
            .env("HOME", root.path())
            .env_remove("XDG_CONFIG_HOME")
            .env("PLOYZ_CONFIG", root.path().join("default.yaml"))
            .env("PLOYZ_STORE", format!("sqlite:{}", db.display()))
            .env_remove("PLOYZ_TOKEN")
            .env_remove("PLOYZ_PROJECT")
            .env_remove("PLOYZ_ENV")
            .env_remove("PLOYZ_CONTEXT")
            .env_remove("PLOYZ_CONNECT")
            .args([
                "--json",
                "--color=never",
                "--ployz-config",
                selected.to_str().unwrap(),
                "--connect",
                &connect,
            ])
            .args(&args)
            .kill_on_drop(true);
        async move {
            tokio::time::timeout(Duration::from_secs(10), command.output())
                .await
                .unwrap_or_else(|error| panic!("{args:?}: {error}"))
                .unwrap()
        }
    };
    for args in [
        vec!["project", "new", "shop"],
        vec!["service", "add", "web", "--image", "web:1"],
    ] {
        let output = invoke(args).await;
        assert!(output.status.success(), "{output:?}");
    }
    let output = invoke(vec!["deploy", "--context", "staging"]).await;
    server.abort();
    server.await.ok();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "failed", "{result}");
    let hints = stderr
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("inspect: ")
                .or_else(|| line.trim().strip_prefix("retry: "))
        })
        .map(|line| shell_words::split(line).unwrap())
        .collect::<Vec<_>>();
    let prefix = [
        "ployz",
        "--ployz-config",
        selected.to_str().unwrap(),
        "--connect",
        &connect,
    ];
    let retry = [
        "deploy",
        "--project",
        "shop",
        "--env",
        "production",
        "--context",
        "staging",
    ];
    let expected = prefix.iter().copied().chain(retry).collect::<Vec<_>>();
    assert_eq!(hints, [expected], "{stderr}");
}

#[tokio::test]
async fn publish_reviews_volume_loss_the_servers_hold_and_publishes_once_accepted() {
    let store = tempfile::tempdir().unwrap();
    let db = store.path().join("store.db");
    let service = DeployService::new(machine('a', "one"));
    let held = std::sync::Arc::clone(&service.volumes);
    let (address, server) = listening(service).await;
    let address = address.to_string();
    let cli = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let (db, address) = (db.clone(), address.clone());
        async move {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            ployz(Some(&db), &address, &args).await
        }
    };
    for args in [
        &["project", "new", "shop"][..],
        &["service", "add", "web", "--image", "web:1"],
    ] {
        let (code, result) = cli(args).await;
        assert_eq!(code, 0, "{args:?}: {result}");
    }
    let (code, added) = cli(&["volume", "add", "data", "--mount", "web:/data", "--docker"]).await;
    assert_eq!(code, 0, "{added}");
    let (code, deployed) = cli(&["deploy"]).await;
    assert_eq!(code, 0, "{deployed}");
    held.lock().unwrap().push(ployz_core::DockerVolume {
        id: ployz_core::DockerVolumeId {
            machine_id: machine('a', "one").machine.id,
            name: ployz_core::DockerVolumeName::parse(format!(
                "shop-production_vol-{}",
                added["volume"]["id"].as_str().unwrap()
            ))
            .unwrap(),
        },
        options: Default::default(),
        labels: Default::default(),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    });
    let (code, removed) = cli(&["volume", "rm", "data"]).await;
    assert_eq!(code, 0, "{removed}");

    let (code, refused) = cli(&["deploy", "--skip-deploy"]).await;
    assert_ne!(code, 0, "{refused}");
    let refused = &refused["error"];
    assert_eq!(refused["code"], "confirmation_required", "{refused}");
    assert_eq!(refused["details"]["accept"], serde_json::json!(["data"]));
    let version = refused["details"]["version"].as_str().unwrap();
    let retry = shell_words::split(refused["details"]["retry"].as_str().unwrap()).unwrap();
    assert_eq!(
        retry,
        [
            "ployz",
            "deploy",
            "--skip-deploy",
            "--accept-volume-loss",
            "data",
            "--expect-version",
            version
        ],
        "{refused}"
    );

    let (code, wrong) = cli(&[
        "deploy",
        "--skip-deploy",
        "--expect-version",
        version,
        "--accept-volume-loss",
        "cache",
    ])
    .await;
    assert_ne!(code, 0, "{wrong}");
    assert_eq!(wrong["error"]["code"], "invalid_argument", "{wrong}");

    let (code, published) = cli(&[
        "deploy",
        "--skip-deploy",
        "--expect-version",
        version,
        "--accept-volume-loss",
        "data",
    ])
    .await;
    assert_eq!(code, 0, "{published}");
    assert_eq!(published["created"], true, "{published}");
    assert_eq!(held.lock().unwrap().len(), 1, "saving deletes nothing");
    let (code, listed) = cli(&["deployment", "ls"]).await;
    assert_eq!(code, 0, "{listed}");
    assert_eq!(
        listed["deployments"].as_array().unwrap().len(),
        1,
        "{listed}"
    );
    server.abort();
}

/// A Store with Volume `data` deployed on Server `one` of `service`, then removed
/// from the Environment, unsaved: the next Deploy removes deployed data.
async fn removing_deployed_data(
    db: &std::path::Path,
    service: DeployService,
) -> (
    String,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let (address, server) = listening(service).await;
    let address = address.to_string();
    for args in [
        &["project", "new", "shop"][..],
        &["service", "add", "web", "--image", "web:1"],
        &["volume", "add", "data", "--mount", "web:/data", "--docker"],
        &["deploy"],
        &["volume", "rm", "data"],
    ] {
        let (code, result) = ployz(Some(db), &address, args).await;
        assert_eq!(code, 0, "{args:?}: {result}");
    }
    (address, server)
}

/// The `--json` refusal of a Deploy that removes `data` through `address`, which
/// no longer answers, as it names `address` and the connection's cause.
async fn refused_through_dead_endpoint(db: &std::path::Path, address: &str) -> Value {
    let (code, refused) = ployz(Some(db), address, &["deploy", "--skip-deploy"]).await;
    assert_eq!(code, 1, "{refused}");
    let refused = refused["error"].clone();
    assert_eq!(refused["code"], "unavailable", "{refused}");
    assert_eq!(
        refused["message"],
        format!(
            "This removes deployed Volumes, and Ployz couldn't reach tcp://{address} to check \
             for their data, so it refuses. Try again once it can be reached"
        ),
        "{refused}"
    );
    assert_eq!(
        refused["details"],
        serde_json::json!({
            "volumes": ["data"],
            "unanswered": [],
            "endpoint": format!("tcp://{address}"),
        }),
        "{refused}"
    );
    refused
}

#[tokio::test]
async fn a_removal_deployed_while_its_server_is_down_names_the_endpoint_and_why() {
    let store = tempfile::tempdir().unwrap();
    let db = store.path().join("store.db");
    let (address, server) =
        removing_deployed_data(&db, DeployService::new(machine('a', "one"))).await;
    server.abort();
    let _ = server.await;

    let refused = refused_through_dead_endpoint(&db, &address).await;
    let cause: Vec<&str> = refused["cause"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cause| cause.as_str().unwrap())
        .collect();
    assert!(
        cause
            .last()
            .is_some_and(|cause| cause.contains("Connection refused")),
        "{refused}"
    );

    let human = run(Some(&db), &address, &["deploy", "--skip-deploy"]).await;
    assert_eq!(human.status.code(), Some(1), "{human:?}");
    let stderr = String::from_utf8_lossy(&human.stderr);
    assert!(
        stderr.contains(&format!(
            "error: This removes deployed Volumes, and Ployz couldn't reach tcp://{address}"
        )),
        "{stderr}"
    );
    assert!(
        stderr
            .lines()
            .any(|line| line.trim_start().starts_with("cause:")
                && line.contains("Connection refused")),
        "{stderr}"
    );
}

#[tokio::test]
async fn a_removal_deployed_while_its_server_fails_the_check_names_the_error() {
    let store = tempfile::tempdir().unwrap();
    let db = store.path().join("store.db");
    let service = DeployService::new(machine('a', "one"));
    let machines_error = std::sync::Arc::clone(&service.machines_error);
    let (address, server) = removing_deployed_data(&db, service).await;
    *machines_error.lock().unwrap() = Some(ployz_core::RpcError {
        code: ployz_core::RpcErrorCode::Internal,
        message: "The Machine list is unavailable.".into(),
        details: Value::Null,
        cause: vec!["database is locked".into()],
    });

    let (code, refused) = ployz(Some(&db), &address, &["deploy", "--skip-deploy"]).await;
    assert_eq!(code, 1, "{refused}");
    let refused = &refused["error"];
    assert_eq!(refused["code"], "unavailable", "{refused}");
    assert_eq!(
        refused["message"],
        "This removes deployed Volumes, and Server one returned an error when Ployz checked \
         for their data, so it refuses. Fix the error Server one returned, then try again",
        "{refused}"
    );
    assert_eq!(
        refused["details"],
        serde_json::json!({ "volumes": ["data"], "server": "one" }),
        "{refused}"
    );
    assert_eq!(
        refused["cause"],
        serde_json::json!(["The Machine list is unavailable.", "database is locked"]),
        "{refused}"
    );

    let human = run(Some(&db), &address, &["deploy", "--skip-deploy"]).await;
    assert_eq!(human.status.code(), Some(1), "{human:?}");
    let stderr = String::from_utf8_lossy(&human.stderr);
    assert!(stderr.contains("cause: database is locked"), "{stderr}");
    server.abort();
}

#[tokio::test]
async fn a_removal_refused_by_an_unknown_daemon_names_the_endpoint_and_no_server() {
    let store = tempfile::tempdir().unwrap();
    let db = store.path().join("store.db");
    let service = DeployService::new(machine('a', "one"));
    let describe_refusal = std::sync::Arc::clone(&service.describe_refusal);
    let (address, server) = removing_deployed_data(&db, service).await;
    *describe_refusal.lock().unwrap() = Some("not authorized".into());

    let (code, refused) = ployz(Some(&db), &address, &["deploy", "--skip-deploy"]).await;
    assert_eq!(code, 1, "{refused}");
    let refused = &refused["error"];
    assert_eq!(refused["code"], "unavailable", "{refused}");
    assert_eq!(
        refused["details"],
        serde_json::json!({ "volumes": ["data"], "endpoint": format!("tcp://{address}") }),
        "{refused}"
    );
    server.abort();
}

#[tokio::test]
async fn a_removal_deployed_through_a_dead_endpoint_names_no_server_it_did_not_ask() {
    let store = tempfile::tempdir().unwrap();
    let db = store.path().join("store.db");
    // Server two ran an earlier Deployment of this Organization.
    let (two, server) = listening(DeployService::new(machine('b', "two"))).await;
    let two = two.to_string();
    for args in [
        &["project", "new", "shop"][..],
        &["service", "add", "api", "--image", "api:1"],
        &["deploy"],
    ] {
        let (code, result) = ployz(Some(&db), &two, args).await;
        assert_eq!(code, 0, "{args:?}: {result}");
    }
    server.abort();
    let (address, server) = listening(DeployService::new(machine('a', "one"))).await;
    let address = address.to_string();
    for args in [
        &["service", "add", "web", "--image", "web:1"][..],
        &["volume", "add", "data", "--mount", "web:/data", "--docker"],
        &["deploy"],
        &["volume", "rm", "data"],
    ] {
        let (code, result) = ployz(Some(&db), &address, args).await;
        assert_eq!(code, 0, "{args:?}: {result}");
    }
    server.abort();
    let _ = server.await;

    refused_through_dead_endpoint(&db, &address).await;
}

#[tokio::test]
async fn ctrl_c_during_cleanup_prints_each_copy_left_running_and_exits_non_zero() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("store.db");
    let (entry, edge) = (machine('a', "entry"), machine('b', "edge"));
    let earlier = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "web:1", "pull_policy": "always" }
    }))
    .unwrap();
    let mut old = running_container_in(&entry, &earlier, "shop-production", '1');
    old.try_update(|parts| parts.rotation = Some(ployz_core::CopyRotation::In))
        .unwrap();
    let old_id = old.container_id;
    let daemon = DeployService::new(entry.clone())
        .with_machines(vec![entry, edge.clone()])
        .never_unroute_on(edge.machine.id);
    daemon.listed_containers().lock().unwrap().push(old);
    let (address, server) = listening(daemon.clone()).await;
    let connect = format!("tcp://{address}");
    let command = |args: &[&str]| {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .current_dir(root.path())
            .env("HOME", root.path())
            .env("PLOYZ_CONFIG", root.path().join("config.yaml"))
            .env("PLOYZ_STORE", format!("sqlite:{}", db.display()))
            .env_remove("PLOYZ_TOKEN")
            .env_remove("PLOYZ_PROJECT")
            .env_remove("PLOYZ_ENV")
            .env_remove("PLOYZ_CONTEXT")
            .env_remove("PLOYZ_CONNECT")
            .args(["--color=never", "--connect", &connect])
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    };
    for args in [
        &["project", "new", "shop"][..],
        &["service", "add", "web", "--image", "web:2"],
    ] {
        let output = command(args).output().await.unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let deploy = command(&["deploy"]).spawn().unwrap();
    tokio::time::timeout(Duration::from_secs(20), daemon.unrouted_copy(&old_id))
        .await
        .expect("cleanup takes the old copy out of rotation");
    let interrupted = std::process::Command::new("kill")
        .args(["-INT", &deploy.id().unwrap().to_string()])
        .status()
        .unwrap();
    assert!(interrupted.success());
    let output = tokio::time::timeout(Duration::from_secs(20), deploy.wait_with_output())
        .await
        .expect("Ctrl-C ends the deploy")
        .unwrap();
    server.abort();

    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_ne!(output.status.code(), Some(0), "{printed}");
    assert!(
        printed.contains(&format!(
            "shop-production/web copy {} left running on entry (interrupted). The next deploy cleans it up.",
            &old_id.as_str()[..12]
        )),
        "{printed}"
    );
    assert!(!printed.contains(&old_id.to_string()), "{printed}");
}

#[tokio::test]
async fn context_config_empty_context_deploy_names_add_servers_before_a_volume_check() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("store.db");
    let (_, server) = removing_deployed_data(&db, DeployService::new(machine('a', "one"))).await;
    server.abort();
    let path = home.path().join("config.yaml");
    ployz::context::Config::new(
        &path,
        Some("verify".into()),
        std::collections::BTreeMap::from([(
            "verify".into(),
            ployz::context::Context {
                connections: Vec::new(),
            },
        )]),
    )
    .save()
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["deploy", "--json"])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("PLOYZ_CONFIG", &path)
        .env("PLOYZ_STORE", format!("sqlite:{}", db.display()))
        .env_remove("PLOYZ_CONTEXT")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_TOKEN")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV")
        .output()
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{json}");
    assert_eq!(
        json["error"]["message"], "Context verify has no Servers.",
        "{json}"
    );
    assert_eq!(
        json["error"]["details"]["next"], "ployz server add --standalone --context verify",
        "{json}"
    );
}
