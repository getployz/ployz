//! A Deployment is applied once traffic moved; its old copies retire after.

use super::support::*;
use ployz::deploy::DeployWarning;
use ployz_store::{
    Actor, Admit, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId, DeploymentQuery,
    DeploymentStatus, DeploymentSummary, DeploymentWarning, EnvironmentId, EnvironmentRef,
    OrganizationId, ProjectId, ProjectName, RunnerId, SealingKey, ServiceLineageId, Trusted,
};
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader};

struct Shop {
    home: tempfile::TempDir,
    store: Arc<ConfigStore>,
    who: Actor,
    address: std::net::SocketAddr,
    url: String,
}

impl Shop {
    /// A shop whose web Service one Deployment already put on `service`'s Machine.
    async fn deployed(service: DeployService) -> Self {
        Self::deployed_on(service, |home| {
            format!("sqlite:{}", home.path().join("store.db").display())
        })
        .await
    }

    async fn deployed_on(
        service: DeployService,
        database: impl FnOnce(&tempfile::TempDir) -> String + Send + 'static,
    ) -> Self {
        let (home, store, who, url) = tokio::task::spawn_blocking(move || {
            let home = tempfile::tempdir().unwrap();
            let url = database(&home);
            let key = SealingKey::from_file(&home.path().join("store.db.key")).unwrap();
            let store = Arc::new(ConfigStore::open(&url, key).unwrap());
            let who = Actor::system(OrganizationId::parse("local").unwrap());
            store
                .write(
                    &who,
                    &CreateProject {
                        id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                        name: ProjectName::parse("shop").unwrap(),
                        default_environment: EnvironmentId::parse(
                            "00000000-0000-4000-8000-000000000002",
                        )
                        .unwrap(),
                        region: None,
                    },
                )
                .unwrap();
            store
                .write(
                    &who,
                    &CreateService {
                        id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003")
                            .unwrap(),
                        environment: EnvironmentRef::default(),
                        name: ployz_core::ServiceName::parse("web").unwrap(),
                        image: Some("nginx".into()),
                        template: None,
                    },
                )
                .unwrap();
            (home, store, who, url)
        })
        .await
        .unwrap();
        let (address, _server) = listening(service).await;
        let shop = Self {
            home,
            store,
            who,
            address,
            url,
        };
        let first = if shop.url.starts_with("sqlite:") {
            shop.run(1, false)
        } else {
            tokio::task::block_in_place(|| shop.run(1, false))
        }
        .await
        .unwrap();
        assert_eq!(first.status, DeploymentStatus::Applied);
        shop
    }

    fn id(number: u8) -> DeploymentId {
        DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{number:02}")).unwrap()
    }

    /// Admit and run Deployment `number`; `recreate` replaces web's copy.
    fn run(
        &self,
        number: u8,
        recreate: bool,
    ) -> impl Future<Output = Result<DeploymentSummary, ployz_core::RpcError>> + 'static {
        let id = Self::id(number);
        self.store
            .write_trusted(
                &self.who,
                &Admit::Deploy(Deploy {
                    id: id.clone(),
                    environment: EnvironmentRef::default(),
                    services: Vec::new(),
                    version: None,
                    upload: None,
                    accept_volume_loss: Vec::new(),
                    message: None,
                    prune: false,
                    no_prune: false,
                    versions: Vec::new(),
                    copies: Vec::new(),
                    recreate: if recreate {
                        vec![ployz_core::ServiceName::parse("web").unwrap()]
                    } else {
                        Vec::new()
                    },
                    rule: None,
                }),
                &Trusted::default(),
            )
            .unwrap();
        ployz::sdk::run_deployment(
            ployz_core::Operator::Cloud,
            Arc::clone(&self.store),
            id,
            RunnerId::parse(format!("cloud-run-{number}")).unwrap(),
            vec![ployz::context::Connection::tcp(self.address)],
            Ok(Default::default()),
        )
    }

    fn cli(&self, args: &[&str]) -> tokio::process::Command {
        let home = self.home.path();
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .current_dir(home)
            .env("HOME", home)
            .env("PLOYZ_CONFIG", home.join("config.yaml"))
            .env("PLOYZ_STORE", &self.url)
            .env_remove("PLOYZ_TOKEN")
            .env_remove("PLOYZ_PROJECT")
            .env_remove("PLOYZ_ENV")
            .env_remove("PLOYZ_CONTEXT")
            .env_remove("PLOYZ_CONNECT")
            .args([
                "--why=test",
                "--connect",
                &format!("tcp://{}", self.address),
            ])
            .args(args)
            .kill_on_drop(true);
        command
    }

    async fn show(&self, number: &str) -> DeploymentSummary {
        let output = self
            .cli(&["--json", "deployment", "show", number])
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<ployz_store::DeploymentView>(&output.stdout)
            .unwrap()
            .deployment
    }

    fn status(&self, number: u8) -> DeploymentStatus {
        self.store
            .read(
                &self.who,
                &DeploymentQuery {
                    id: Self::id(number),
                },
            )
            .unwrap()
            .deployment
            .status
    }
}

#[tokio::test]
async fn the_old_copy_stays_in_until_the_store_commits_live() {
    held_live_write(LiveWrite::Commit).await;
}

#[tokio::test]
async fn cancelling_while_the_live_write_is_held_leaves_the_old_copy_in() {
    held_live_write(LiveWrite::CancelAndCommit).await;
}

#[tokio::test]
async fn a_rejected_live_write_does_not_delay_cancellation() {
    held_live_write(LiveWrite::Cancel).await;
}

#[tokio::test]
async fn an_unreadable_store_ends_the_live_wait_at_the_lease_deadline() {
    held_live_write(LiveWrite::LoseLease).await;
}

enum LiveWrite {
    Commit,
    CancelAndCommit,
    Cancel,
    LoseLease,
}

async fn held_live_write(write: LiveWrite) {
    use ployz_store::testing::backend;
    let service = DeployService::new(machine('a', "one")).lists_created();
    let rotations = service.rotations();
    let shop = Shop::deployed(service.clone()).await;
    let old = *rotations.lock().unwrap().keys().next().unwrap();
    let url = format!("sqlite:{}", shop.home.path().join("store.db").display());
    backend::execute(
        &url,
        r#"CREATE TRIGGER hold_switch BEFORE UPDATE ON config_deployment
           WHEN NEW.run LIKE '%"live_at":%' AND OLD.run NOT LIKE '%"live_at":%'
           BEGIN SELECT RAISE(ABORT, 'switch storage unavailable'); END"#,
    );

    let running = tokio::spawn(shop.run(2, true));
    tokio::time::timeout(Duration::from_secs(10), async {
        while rotations.lock().unwrap().len() < 2 {
            // real-wait: the fake Machine receives the new copy's rotation from an SDK task.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the new copy entered rotation");
    assert!(
        // real-wait: independently observe whether the SDK withdrew the old copy.
        tokio::time::timeout(Duration::from_millis(250), service.unrouted_copy(&old))
            .await
            .is_err(),
        "the old copy went Out before the Store committed Live"
    );
    assert_eq!(shop.status(2), DeploymentStatus::Running);
    if matches!(write, LiveWrite::LoseLease) {
        backend::execute(
            &url,
            "ALTER TABLE config_deployment RENAME TO offline_deployment",
        );
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(600)).await;
        let error = tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .expect("the Live wait ends at the last acknowledged lease")
            .unwrap()
            .unwrap_err();
        assert_eq!(error.message, "The Deployment runner's lease expired");
        assert!(!service.unrouted().lock().unwrap().contains(&old));
        return;
    }
    let cancel = matches!(write, LiveWrite::Cancel | LiveWrite::CancelAndCommit);
    let release = matches!(write, LiveWrite::Commit | LiveWrite::CancelAndCommit);
    if cancel {
        shop.store
            .write(
                &shop.who,
                &ployz_store::Cancel {
                    deployment: Shop::id(2),
                },
            )
            .unwrap();
        assert_eq!(shop.status(2), DeploymentStatus::Cancelling);
    }
    if release {
        backend::execute(&url, "DROP TRIGGER hold_switch");
    }

    let ran = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ran.live_at.is_some(), release && !cancel);
    if cancel {
        assert_eq!(ran.status, DeploymentStatus::Cancelled);
        assert!(!service.unrouted().lock().unwrap().contains(&old));
    } else {
        assert_eq!(ran.status, DeploymentStatus::Applied);
        assert!(service.unrouted().lock().unwrap().contains(&old));
    }
}

#[tokio::test]
async fn a_deployment_is_applied_before_its_old_copy_is_removed() {
    let service = DeployService::new(machine('a', "one"))
        .lists_created()
        .hold_removals();
    let removed = service.removed_containers();
    let shop = Shop::deployed(service.clone()).await;

    let running = tokio::spawn(shop.run(2, true));
    service.removal_waited().await;
    // The runner records the outcome beside the removal; the removal never ends.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while shop.status(2) != DeploymentStatus::Applied {
            // real-wait: the runner records in a blocking store call this test can't await.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("applied while its old copy is still being removed");
    assert!(removed.lock().unwrap().is_empty());

    service.release_removals();
    let ran = running.await.unwrap().unwrap();
    assert_eq!(ran.status, DeploymentStatus::Applied);
    assert_eq!(removed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_cancel_after_apply_stops_waiting_for_cleanup_without_undoing_live() {
    let service = DeployService::new(machine('a', "one"))
        .lists_created()
        .hold_removals();
    let shop = Shop::deployed(service.clone()).await;
    let mut running = shop.cli(&["deploy", "--recreate"]);
    let running = tokio::spawn(async move { running.output().await.unwrap() });
    service.removal_waited().await;
    let applied = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let shown = shop.show("2").await;
            if shown.status == DeploymentStatus::Applied {
                break shown;
            }
        }
    })
    .await
    .unwrap();
    let old = *service.unrouted().lock().unwrap().iter().next().unwrap();

    let cancelled = shop
        .store
        .write(
            &shop.who,
            &ployz_store::Cancel {
                deployment: applied.id,
            },
        )
        .unwrap();
    let at = cancelled.live_at.unwrap();
    assert_eq!(cancelled.status, DeploymentStatus::Applied);
    assert_eq!(
        cancelled.cleanup,
        Some(ployz_store::CleanupStatus::Unconfirmed)
    );
    let output = tokio::time::timeout(Duration::from_secs(8), running)
        .await
        .expect("cancel releases the removal wait without its reply")
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("removal requested but not confirmed"),
        "{stderr}"
    );
    assert!(!stderr.contains("left running"), "{stderr}");
    let ran = shop.show("2").await;
    assert!(
        ran.warnings.iter().any(|warning| matches!(
            warning,
            DeploymentWarning::Deploy { warning: DeployWarning::CleanupUnconfirmed { container_id, .. } }
                if *container_id == old
        )),
        "{:?}",
        ran.warnings
    );
    assert_eq!(ran.status, DeploymentStatus::Applied);
    assert_eq!(ran.live_at, Some(at));
    assert_eq!(ran.cleanup, Some(ployz_store::CleanupStatus::Unconfirmed));
    assert_eq!(
        service
            .rotations()
            .lock()
            .unwrap()
            .values()
            .filter(|rotation| rotation.rotation == ployz_core::CopyRotation::In)
            .count(),
        1
    );
    service.release_removals();
}

#[tokio::test]
async fn an_old_copy_that_will_not_go_is_a_warning_on_an_applied_deployment() {
    let service = DeployService::new(machine('a', "one"))
        .lists_created()
        .fail_removals(ployz_core::RpcError::internal("docker is busy"));
    let shop = Shop::deployed(service).await;

    let ran = shop.run(2, true).await.unwrap();

    assert_eq!(ran.status, DeploymentStatus::Applied);
    let left: Vec<_> = ran
        .warnings
        .iter()
        .filter(|warning| {
            matches!(
                warning,
                DeploymentWarning::Deploy {
                    warning: DeployWarning::LeftRunning { .. }
                }
            )
        })
        .collect();
    assert_eq!(left.len(), 1, "{:?}", ran.warnings);
}

#[tokio::test]
async fn stopping_old_runs_until_the_old_copy_retired_after_the_outcome_is_gone() {
    let service = DeployService::new(machine('a', "one"))
        .lists_created()
        .hold_removals();
    let shop = Shop::deployed(service.clone()).await;
    let stopping_old = |shop: &Shop| {
        shop.store
            .read(&shop.who, &DeploymentQuery { id: Shop::id(2) })
            .unwrap()
            .steps
            .into_iter()
            .find(|step| step.name == ployz_core::deploy_steps::StepName::StoppingOld)
    };

    let running = tokio::spawn(shop.run(2, true));
    service.removal_waited().await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while shop.status(2) != DeploymentStatus::Applied {
            // real-wait: the runner records in a blocking store call this test can't await.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("applied while its old copy is still being removed");
    let open = stopping_old(&shop).expect("stopping_old started at the switch");
    assert_eq!(open.ended_at, None, "the old copy is still being removed");

    service.release_removals();
    running.await.unwrap().unwrap();
    let ended = stopping_old(&shop).expect("stopping_old");
    assert!(ended.ended_at.is_some(), "{ended:?}");
}

#[test]
fn a_failed_scale_down_after_the_switch_keeps_the_live_timestamp() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (one, two) = (machine('a', "one"), machine('b', "two"));
    let service = DeployService::new(one.clone())
        .with_machines(vec![one, two])
        .lists_created()
        .hold_removals()
        .fail_removals(ployz_core::RpcError::internal("scale-down removal failed"));
    let shop = runtime.block_on(Shop::deployed_on(
        service.clone(),
        ployz_store::testing::backend::fresh_url,
    ));
    runtime.block_on(cancel_before_the_removal_failure(&shop, &service));
}

async fn cancel_before_the_removal_failure(shop: &Shop, service: &DeployService) {
    let add = |name, id| {
        shop.store
            .write(
                &shop.who,
                &CreateService {
                    id: ServiceLineageId::parse(id).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ployz_core::ServiceName::parse(name).unwrap(),
                    image: Some("nginx".into()),
                    template: None,
                },
            )
            .unwrap()
    };
    let replicas = |value| {
        shop.store
            .write(
                &shop.who,
                &ployz_store::Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![ployz_store::Change::Set {
                        path: ployz_store::SettingPath::parse("api.replicas").unwrap(),
                        value: serde_json::json!(value),
                    }],
                },
            )
            .unwrap()
    };

    let initial = tokio::task::block_in_place(|| {
        add("api", "00000000-0000-4000-8000-000000000004");
        replicas(2);
        shop.run(2, false)
    });
    assert_eq!(initial.await.unwrap().status, DeploymentStatus::Applied);
    tokio::task::block_in_place(|| {
        add("worker", "00000000-0000-4000-8000-000000000005");
        replicas(1);
    });

    let mut deploy = shop
        .cli(&["deploy"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(deploy.stderr.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(10), async {
        service.removal_waited().await;
        while let Some(line) = lines.next_line().await.unwrap() {
            if line == "Deployment #3 Live." {
                return;
            }
        }
        panic!("the CLI never printed Live");
    })
    .await
    .expect("Live is printed while the scale-down removal is still held");
    let live = shop.show("3").await;
    let live_at = live.live_at.unwrap();
    assert!(deploy.try_wait().unwrap().is_none());
    tokio::task::block_in_place(|| {
        shop.store
            .write(
                &shop.who,
                &ployz_store::Cancel {
                    deployment: live.id,
                },
            )
            .unwrap();
    });
    service.release_removals();
    let (output, messages) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(deploy.wait_with_output(), async {
            let mut messages = Vec::new();
            while let Some(line) = lines.next_line().await.unwrap() {
                messages.push(line);
            }
            messages
        })
    })
    .await
    .expect("deploy returns when the removal fails");
    let output = output.unwrap();
    assert_eq!(output.status.code(), Some(3), "{messages:?}");
    assert!(
        messages
            .iter()
            .any(|line| line.contains("scale-down removal failed")),
        "{messages:?}"
    );
    assert!(!messages.iter().any(|line| line == "Deployment #3 Live."));
    let failed = shop.show("3").await;
    assert_eq!(failed.status, DeploymentStatus::Failed);
    assert_eq!(failed.live_at, Some(live_at));
    assert!(
        failed.warnings.iter().any(|warning| matches!(
            warning,
            DeploymentWarning::Deploy {
                warning: DeployWarning::LeftRunning { .. }
            }
        )),
        "{:?}",
        failed.warnings
    );
}

#[tokio::test]
async fn finished_records_the_retirement_warning_even_when_a_separate_warning_write_is_refused() {
    let service = DeployService::new(machine('a', "one"))
        .lists_created()
        .fail_removals(ployz_core::RpcError::internal("docker is busy"));
    let shop = Shop::deployed(service).await;
    let url = format!("sqlite:{}", shop.home.path().join("store.db").display());
    ployz_store::testing::backend::execute(
        &url,
        "CREATE TRIGGER reject_warning_alone BEFORE UPDATE ON config_deployment
         WHEN json_extract(NEW.run, '$.warnings') IS NOT NULL
          AND NEW.cleanup_status = 'pending'
         BEGIN SELECT RAISE(ABORT, 'warning must accompany Finished'); END",
    );

    let output = shop.cli(&["deploy", "--recreate"]).output().await.unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("left running") && stderr.contains("docker is busy"),
        "{stderr}"
    );
    let shown = shop
        .cli(&["--json", "deployment", "show", "2"])
        .output()
        .await
        .unwrap();
    assert!(shown.status.success(), "{shown:?}");
    let view: ployz_store::DeploymentView = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Applied);
    assert_eq!(view.deployment.cleanup, None);
    assert!(view.deployment.warnings.iter().any(|warning| {
        matches!(warning, DeploymentWarning::Deploy {
            warning: DeployWarning::LeftRunning { reason, .. }
        } if reason.contains("docker is busy"))
    }));
}

#[tokio::test]
async fn a_start_first_deploy_reports_live_before_failed_cleanup_and_exits_partial() {
    let service = DeployService::new(machine('a', "one"))
        .lists_created()
        .hold_removals()
        .fail_removals(ployz_core::RpcError::internal("docker is busy"));
    let shop = Shop::deployed(service.clone()).await;

    let mut deploy = shop
        .cli(&["deploy", "--recreate"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(deploy.stderr.take().unwrap()).lines();
    let seen = tokio::time::timeout(Duration::from_secs(10), async {
        service.removal_waited().await;
        let mut seen = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            seen.push(line);
            if seen
                .last()
                .is_some_and(|line| line.contains("stopping_old"))
            {
                break;
            }
        }
        seen
    })
    .await
    .expect("follow prints the traffic switch and stopping_old while removal is held");
    assert!(
        seen.iter().any(|line| line == "Deployment #2 Live."),
        "{seen:?}"
    );
    assert!(
        seen.last()
            .is_some_and(|line| line.contains("stopping_old")),
        "{seen:?}"
    );
    assert!(deploy.try_wait().unwrap().is_none());

    service.release_removals();
    let (output, warnings) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(deploy.wait_with_output(), async {
            let mut warnings = Vec::new();
            while let Some(line) = lines.next_line().await.unwrap() {
                warnings.push(line);
            }
            warnings
        })
    })
    .await
    .expect("deploy returns once cleanup answers");
    assert_eq!(output.unwrap().status.code(), Some(3), "{warnings:?}");
    assert!(
        warnings
            .iter()
            .any(|line| line.contains("left running") && line.contains("docker is busy")),
        "{warnings:?}"
    );
    assert!(!warnings.iter().any(|line| line == "Deployment #2 Live."));

    let show = shop
        .cli(&["--json", "deployment", "show", "2"])
        .output()
        .await
        .unwrap();
    assert!(show.status.success(), "{show:?}");
    let shown: ployz_store::DeploymentView = serde_json::from_slice(&show.stdout).unwrap();
    assert!(shown.deployment.live_at.unwrap() <= shown.deployment.ended_at.unwrap());

    let show = shop
        .cli(&["deployment", "show", "2"])
        .output()
        .await
        .unwrap();
    assert!(show.status.success(), "{show:?}");
    assert!(String::from_utf8_lossy(&show.stdout).contains("Live at "));
}

#[tokio::test]
async fn a_stop_first_deploy_records_when_the_new_copy_switched() {
    let service = DeployService::new(machine('a', "one")).lists_created();
    let shop = Shop::deployed(service).await;
    shop.store
        .write(
            &shop.who,
            &ployz_store::Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![ployz_store::Change::Set {
                    path: ployz_store::SettingPath::parse("web.updateOrder").unwrap(),
                    value: serde_json::json!("stop-first"),
                }],
            },
        )
        .unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        shop.cli(&["--json", "deploy", "--recreate"]).output(),
    )
    .await
    .expect("stop-first deploy finishes")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let view: ployz_store::DeploymentView = serde_json::from_slice(&output.stdout).unwrap();
    let end = |name| {
        view.steps
            .iter()
            .find(|step| step.name == name)
            .unwrap()
            .ended_at
            .unwrap()
    };
    use ployz_store::StepName;
    assert!(end(StepName::StoppingOld) <= end(StepName::SwitchingTraffic));
    assert!(view.deployment.live_at.unwrap() <= view.deployment.ended_at.unwrap());
}

#[tokio::test]
async fn a_deploy_that_fails_before_switching_has_no_live_at() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone()).lists_created();
    let mut shop = Shop::deployed(service.clone()).await;
    let live = shop
        .store
        .read(&shop.who, &DeploymentQuery { id: Shop::id(1) })
        .unwrap();
    assert!(live.deployment.live_at.is_some());

    let (address, _server) = listening(service.fail_pulls_on(machine.machine.id)).await;
    shop.address = address;
    let failed = shop.run(2, true).await.unwrap();
    assert_eq!(failed.status, DeploymentStatus::Failed);
    assert_eq!(failed.live_at, None);
}
