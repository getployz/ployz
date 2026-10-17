use ployz_core::config::ServiceGitAccess;
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AuthorizedRepository, Automated, BranchHead, Change, ConfigStore, CreateGitService,
    Edit, EnvironmentRef, OnMergeConclusion, PullRequest, PullRequestQuery, Ride, ServiceLineageId,
    SetPrPlan, SettingPath, SystemEvent, Trusted,
};
use serde_json::{Value, json};

use crate::backend;
use crate::support::{
    HEAD, LATER, MERGE, at, check, deployed, exclude, merge_value, merged, passed, preview_listed,
    pull, push, row, save, set, shop, shop_with, system, unset, uuid, value, vouched, watching,
};

/// Pull request `number` from `head_branch` onto `target`, as GitHub said at `updated`.
fn pull_request(
    number: u64,
    head_branch: &str,
    target: &str,
    merge: Option<&str>,
    open: bool,
    updated: &str,
) -> SystemEvent {
    SystemEvent::PullRequest(PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(number),
        title: "Add search".into(),
        author: "ada".into(),
        author_id: Some(backend::ada()),
        bot: false,
        head_branch: backend::git_branch(head_branch),
        head_repository_id: Some(backend::repo_id(11)),
        head: backend::sha(HEAD),
        target_branch: backend::git_branch(target),
        commits: 1,
        open,
        merge_commit: merge.map(backend::sha),
        target_head: None,
        updated: ployz_store::GithubTimestamp::parse(updated).unwrap(),
    })
}

/// A push to `branch`.
fn push_on(branch: &str, base: Option<&str>, head: &str, changed: &[&str]) -> SystemEvent {
    SystemEvent::BranchHead(BranchHead {
        repository_id: backend::repo_id(11),
        branch: backend::git_branch(branch),
        base: base.map(backend::sha),
        head: Some(backend::sha(head)),
        changed: Some(changed.iter().map(|path| (*path).to_owned()).collect()),
    })
}

/// PR Environments stay when their pull request closes.
fn keep(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: None,
                start_from: None,
                copy: None,
                setup: None,
                remove_on_close: Some(false),
                include_bots: None,
            },
        )
        .unwrap();
}

/// Every Service the admitted Deployments target, sorted.
fn deployed_services(automated: &Automated) -> Vec<String> {
    let mut services: Vec<String> = deployed(automated).into_iter().flatten().collect();
    services.sort();
    services
}

#[test]
fn a_kept_preview_closed_unmerged_awaits_no_merge() {
    let (store, who) = shop();
    keep(&store, &who);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    system(
        &store,
        &who,
        &pull_request(5, "search", "main", None, false, "2026-09-29T11:00:00Z"),
    );
    assert!(preview_listed(&store, &who));
    let main = backend::git_branch("main");
    assert!(
        store
            .awaiting_merge(&who.organization, backend::repo_id(11), &main)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn what_landed_deploys_at_once_if_the_branch_already_moved_past_the_merge() {
    let (store, who) = shop();
    watching(&store, &who);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let later = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert!(later.admitted.is_empty(), "{later:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed(&landed), [["web"]]);
}

#[test]
fn a_merge_that_changes_only_a_mounted_config_deploys_its_service() {
    let config = || ployz_core::ConfigName::parse("sentry").unwrap();
    let file = |environment: EnvironmentRef, content: &str| ployz_store::PutConfigFile {
        environment,
        config: config(),
        file: ployz_core::ConfigFileName::parse("config.yml").unwrap(),
        content: content.into(),
        mode: None,
        uid: None,
        gid: None,
    };
    let (store, who) = shop_with(|store, who| {
        store
            .write(
                who,
                &ployz_store::CreateConfig {
                    id: ployz_store::ConfigId::parse(uuid(9)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: config(),
                    mounts: vec![ployz_store::ConfigMountAt {
                        service: ServiceName::parse("web").unwrap(),
                        dir: "/etc/sentry".into(),
                    }],
                },
            )
            .unwrap();
        store
            .write(who, &file(EnvironmentRef::default(), "level: info\n"))
            .unwrap();
        save(store, who, "production");
    });
    watching(&store, &who);
    store
        .write(&who, &file(at("pr-5"), "level: debug\n"))
        .unwrap();
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed(&landed), [["web"]]);
}

#[test]
fn a_merge_deploys_an_image_service_it_changed_and_the_services_that_read_it() {
    let (store, who) = shop_with(|store, who| {
        store
            .write(
                who,
                &ployz_store::CreateService {
                    id: ServiceLineageId::parse(uuid(9)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("api").unwrap(),
                    image: Some("api:1".into()),
                    template: None,
                },
            )
            .unwrap();
        set(
            store,
            who,
            "production",
            &[
                ("api.env.X", json!("1")),
                ("web.env.API", json!("${{ api.X }}")),
            ],
        );
        save(store, who, "production");
    });
    watching(&store, &who);
    set(&store, &who, "pr-5", &[("api.env.X", json!("2"))]);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed_services(&landed), ["api", "web"]);
}

#[test]
fn a_merge_that_renames_a_private_dns_deploys_the_services_that_read_its_address() {
    let (store, who) = shop_with(|store, who| {
        store
            .write(
                who,
                &ployz_store::CreateService {
                    id: ServiceLineageId::parse(uuid(9)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("api").unwrap(),
                    image: Some("api:1".into()),
                    template: None,
                },
            )
            .unwrap();
        set(
            store,
            who,
            "production",
            &[("web.env.API", json!("${{ api.PLOYZ_PRIVATE_DOMAIN }}"))],
        );
        save(store, who, "production");
    });
    watching(&store, &who);
    set(
        &store,
        &who,
        "pr-5",
        &[("api.privateDns", json!("backend"))],
    );
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed_services(&landed), ["api", "web"]);
}

#[test]
fn a_deploy_warns_only_about_the_services_it_ships() {
    let (store, who) = shop_with(|store, who| {
        store
            .write(
                who,
                &ployz_store::CreateService {
                    id: ServiceLineageId::parse(uuid(9)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("db").unwrap(),
                    image: Some("db:1".into()),
                    template: None,
                },
            )
            .unwrap();
        // web reads db, so the preview has db too.
        set(
            store,
            who,
            "production",
            &[("web.env.DB", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"))],
        );
        save(store, who, "production");
    });
    watching(&store, &who);
    set(
        &store,
        &who,
        "pr-5",
        &[("db.env.TOKEN", json!({ "secret": "test-token" }))],
    );
    system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed_services(&landed), ["db"]);
    let warnings = |automated: &Automated| {
        let claimed = store
            .claim(
                &automated.admitted[0].deployment.id,
                &ployz_store::RunnerId::parse("runner").unwrap(),
            )
            .unwrap();
        json!(claimed.deployment.warnings)
    };
    let not_set = json!([{ "service": "db", "variable": "TOKEN" }]);
    assert_eq!(warnings(&landed), not_set);

    // A push under web/ ships web alone: db's Not set secret isn't its to warn about.
    let pushed = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["web/app.rs"])),
    );
    assert_eq!(deployed_services(&pushed), ["web"]);
    assert_eq!(warnings(&pushed), json!([]));
}

#[test]
fn what_landed_waits_for_ci_on_the_newest_head() {
    let (store, who) = shop();
    watching(&store, &who);
    set(
        &store,
        &who,
        "production",
        &[("web.waitForCi", json!(true))],
    );
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(pushed.waiting.len(), 1, "{pushed:?}");
    // Another push before CI passed: what landed waits on it instead.
    let later = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert_eq!(later.waiting.len(), 1, "{later:?}");

    assert_eq!(deployed(&passed(&store, &who, LATER)), [["web"]]);
}

#[test]
fn a_push_without_a_head_keeps_what_landed() {
    let (store, who) = shop();
    // web follows no push: only what landed deploys it, after the branch comes back.
    set(
        &store,
        &who,
        "production",
        &[("web.autoDeploy", json!("false"))],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let deleted = SystemEvent::BranchHead(BranchHead {
        repository_id: backend::repo_id(11),
        branch: backend::git_branch("main"),
        base: Some(backend::sha(HEAD)),
        head: None,
        changed: None,
    });
    system(&store, &who, &deleted);

    let pushed = system(&store, &who, &push(None, MERGE, Some(&["README.md"])));
    assert_eq!(deployed(&pushed), [["web"]]);
}

#[test]
fn a_new_git_service_from_the_merged_repository_lands_tracking_the_target_branch() {
    let (store, who) = shop();
    system(&store, &who, &push(None, HEAD, None));
    // The preview adds worker from its own repository, built from another branch.
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(8)).unwrap(),
                environment: at("pr-5"),
                name: ServiceName::parse("worker").unwrap(),
                repository: backend::repo_name("acme/web"),
                branch: Some(backend::git_branch("release")),
            },
            &vouched(),
        )
        .unwrap();
    save(&store, &who, "pr-5");

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "worker.branch"),
        Some(json!("main"))
    );
    // The merge push deploys it, and so does the next push to main.
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(deployed(&pushed), [["web", "worker"]]);
    let later = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert_eq!(deployed(&later), [["web", "worker"]]);
}

#[test]
fn merges_from_two_branches_into_one_environment_each_deploy() {
    let (store, who) = shop_with(|store, who| {
        store
            .write_trusted(
                who,
                &CreateGitService {
                    id: ServiceLineageId::parse(uuid(8)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("worker").unwrap(),
                    repository: backend::repo_name("acme/web"),
                    branch: Some(backend::git_branch("release")),
                },
                &vouched(),
            )
            .unwrap();
        save(store, who, "production");
    });
    set(
        &store,
        &who,
        "production",
        &[("worker.watchPaths", json!(["worker/**"]))],
    );
    watching(&store, &who);
    system(&store, &who, &push_on("release", None, HEAD, &[]));
    system(
        &store,
        &who,
        &pull_request(6, "fix", "release", None, true, "2026-09-29T10:00:00Z"),
    );
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    set(&store, &who, "pr-6", &[("worker.env.W", json!("2"))]);

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    system(
        &store,
        &who,
        &pull_request(
            6,
            "fix",
            "release",
            Some(LATER),
            false,
            "2026-09-29T11:00:00Z",
        ),
    );
    let main = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(deployed(&main), [["web"]]);
    let release = system(
        &store,
        &who,
        &push_on("release", Some(HEAD), LATER, &["README.md"]),
    );
    assert_eq!(deployed(&release), [["worker"]]);
}

#[test]
fn a_kept_preview_edited_after_its_merge_still_reads_landed() {
    let (store, who) = shop();
    keep(&store, &who);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    store
        .write(&who, &merge_value("web.env.A", Some("3")))
        .unwrap();

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("3"))
    );
    assert_eq!(check(&store, &who), None);
    set(&store, &who, "pr-5", &[("web.env.A", json!("5"))]);
    assert_eq!(check(&store, &who), None);
}

#[test]
fn a_stale_merged_event_still_closes_the_preview() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    pull(&store, &who, "main", "2026-09-29T12:00:00Z");

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("2"))
    );
    assert!(!preview_listed(&store, &who));
}

#[test]
fn an_excluded_rows_edit_can_always_be_cleared() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    store
        .write(&who, &merge_value("web.env.A", Some("9")))
        .unwrap();
    exclude(&store, &who, "web.env.A", false);

    store.write(&who, &merge_value("web.env.A", None)).unwrap();
    exclude(&store, &who, "web.env.A", true);
    let a = row(&store, &who, "web.env.A");
    assert_eq!((a.lands, a.ride), (json!("2"), Ride::Rides));
}

#[test]
fn a_removal_takes_no_edit_and_lands_as_a_removal() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    store
        .write(&who, &merge_value("web.env.A", Some("9")))
        .unwrap();
    // The pull request then removes A: its Edit no longer applies.
    unset(&store, &who, "pr-5", "web.env.A");
    let a = row(&store, &who, "web.env.A");
    assert_eq!((a.lands, a.ride), (Value::Null, Ride::Rides));
    let refused = store
        .write(&who, &merge_value("web.env.A", Some("9")))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{refused:?}");

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(value(&store, &who, "production", "web.env.A"), None);
}

#[test]
fn a_config_file_reading_a_secret_the_merge_left_not_set_warns() {
    let (store, who) = shop();
    watching(&store, &who);
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "test-token" }))],
    );
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let config = || ployz_core::ConfigName::parse("sentry").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(9)).unwrap(),
                environment: EnvironmentRef::default(),
                name: config(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::PutConfigFile {
                environment: EnvironmentRef::default(),
                config: config(),
                file: ployz_core::ConfigFileName::parse("config.yml").unwrap(),
                content: "token: \"${{ web.TOKEN }}\"\n".into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
    save(&store, &who, "production");

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let id = &pushed.admitted[0].deployment.id;
    let claimed = store
        .claim(id, &ployz_store::RunnerId::parse("runner").unwrap())
        .unwrap();
    let warnings = json!(claimed.deployment.warnings);
    let warned = json!({ "config": "sentry", "file": "config.yml", "secret": "web.TOKEN" });
    assert!(warnings.as_array().unwrap().contains(&warned), "{warnings}");
}

#[test]
fn a_merge_reported_after_newer_facts_still_reads_merged_on_the_check() {
    let (store, who) = shop();
    keep(&store, &who);
    // A rides naming api, which stays here: production can't take the merge.
    store
        .write(
            &who,
            &ployz_store::CreateService {
                id: ServiceLineageId::parse(uuid(9)).unwrap(),
                environment: at("pr-5"),
                name: ServiceName::parse("api").unwrap(),
                image: Some("api:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("${{ api.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    exclude(&store, &who, "api", false);
    system(
        &store,
        &who,
        &pull_request(5, "search", "main", None, false, "2026-09-29T11:00:01Z"),
    );
    system(
        &store,
        &who,
        &pull_request(
            5,
            "search",
            "main",
            Some(MERGE),
            false,
            "2026-09-29T11:00:00Z",
        ),
    );

    let view = store
        .read(
            &who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap();
    assert_eq!(
        view.pull_request.and_then(|facts| facts.merge_commit),
        Some(backend::sha(MERGE))
    );
    let stuck = view.check.unwrap();
    assert_eq!(
        (stuck.conclusion, stuck.title.as_str()),
        (OnMergeConclusion::Neutral, "Didn't land in production")
    );
}

#[test]
fn the_check_reads_a_config_file_row_as_a_changed_file() {
    let config = || ployz_core::ConfigName::parse("sentry").unwrap();
    let file = |environment: EnvironmentRef, content: &str| ployz_store::PutConfigFile {
        environment,
        config: config(),
        file: ployz_core::ConfigFileName::parse("config.yml").unwrap(),
        content: content.into(),
        mode: None,
        uid: None,
        gid: None,
    };
    let (store, who) = shop_with(|store, who| {
        store
            .write(
                who,
                &ployz_store::CreateConfig {
                    id: ployz_store::ConfigId::parse(uuid(9)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: config(),
                    mounts: vec![ployz_store::ConfigMountAt {
                        service: ServiceName::parse("web").unwrap(),
                        dir: "/etc/sentry".into(),
                    }],
                },
            )
            .unwrap();
        store
            .write(who, &file(EnvironmentRef::default(), "level: info\n"))
            .unwrap();
        save(store, who, "production");
    });
    store
        .write(&who, &file(at("pr-5"), "level: debug\n"))
        .unwrap();

    let check = check(&store, &who).unwrap();
    assert_eq!(check.summary, "- `configs.sentry · config.yml`: changed");
}

#[test]
fn a_merge_reported_after_a_retarget_lands_in_the_branch_it_merged_into() {
    let (store, who) = shop();
    // Newer facts point the pull request at release, which no Environment follows.
    system(
        &store,
        &who,
        &pull_request(5, "search", "release", None, true, "2026-09-29T11:00:01Z"),
    );
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    // The older merge into main still lands in production, main's Environment.
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);

    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("2"))
    );
    let view = store
        .read(
            &who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap();
    assert_eq!(
        view.pull_request.map(|facts| facts.target_branch),
        Some(backend::git_branch("main"))
    );
}

#[test]
fn a_full_manual_deploy_takes_what_landed_from_the_next_push() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.autoDeploy", json!("false"))],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    // Someone deploys production by hand: web ships with what landed.
    store
        .write_trusted(
            &who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: ployz_store::DeploymentId::parse(::uuid::Uuid::new_v4().to_string()).unwrap(),
                environment: at("production"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    // A later saved edit waits for a Deploy: auto-deploy is off.
    set(&store, &who, "production", &[("web.env.C", json!("2"))]);
    save(&store, &who, "production");

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(deployed(&pushed).is_empty(), "{pushed:?}");
}

#[test]
fn two_merges_before_their_pushes_each_deploy_with_their_own_commit() {
    let (store, who) = shop_with(|store, who| {
        for (n, name) in [(9, "api"), (10, "db")] {
            store
                .write(
                    who,
                    &ployz_store::CreateService {
                        id: ServiceLineageId::parse(uuid(n)).unwrap(),
                        environment: EnvironmentRef::default(),
                        name: ServiceName::parse(name).unwrap(),
                        image: Some(format!("{name}:1")),
                        template: None,
                    },
                )
                .unwrap();
        }
        set(
            store,
            who,
            "production",
            &[
                ("web.env.API", json!("${{ api.PLOYZ_PRIVATE_DOMAIN }}")),
                ("web.env.DB", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}")),
            ],
        );
        save(store, who, "production");
    });
    watching(&store, &who);
    system(
        &store,
        &who,
        &pull_request(6, "fix", "main", None, true, "2026-09-29T10:00:00Z"),
    );
    set(&store, &who, "pr-5", &[("api.env.X", json!("2"))]);
    set(&store, &who, "pr-6", &[("db.env.Y", json!("2"))]);

    // Both merge before GitHub reports either push.
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    system(
        &store,
        &who,
        &pull_request(6, "fix", "main", Some(LATER), false, "2026-09-29T11:00:00Z"),
    );
    let first = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(deployed_services(&first), ["api"]);
    let second = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert_eq!(deployed_services(&second), ["db"]);
}

#[test]
fn a_merge_that_renames_a_git_service_keeps_it_on_the_commit_it_last_built() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.autoDeploy", json!("false"))],
    );
    system(&store, &who, &push(None, HEAD, None));
    // web builds HEAD by hand.
    let built = ployz_store::DeploymentId::parse(::uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .write_trusted(
            &who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: built.clone(),
                environment: at("production"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let web = ServiceName::parse("web").unwrap();
    store
        .pin(
            &built,
            &std::collections::BTreeMap::from([(web.clone(), backend::sha(HEAD))]),
        )
        .unwrap();
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    store.claim(&built, &runner).unwrap();
    store
        .record(
            &built,
            &runner,
            ployz_store::RunEvidence::Build(ployz_store::BuildReport {
                service: web,
                status: ployz_store::BuildStatus::Built,
                message: None,
                log: String::new(),
            }),
        )
        .unwrap();

    set(&store, &who, "pr-5", &[("web.privateDns", json!("site"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let [admitted] = pushed.admitted.as_slice() else {
        panic!("one Deployment: {pushed:?}")
    };
    // web follows no push: the renamed Service ships the code it last built.
    let sources = store.sources(&admitted.deployment.id).unwrap();
    assert_eq!(
        sources
            .iter()
            .map(|source| (source.service.as_str(), source.commit.clone()))
            .collect::<Vec<_>>(),
        [("site", Some(backend::sha(HEAD)))]
    );
}

#[test]
fn a_merge_that_moves_a_git_service_to_another_repository_never_pins_the_old_build() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.autoDeploy", json!("false"))],
    );
    system(&store, &who, &push(None, HEAD, None));
    // web builds HEAD by hand.
    let built = ployz_store::DeploymentId::parse(::uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .write_trusted(
            &who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: built.clone(),
                environment: at("production"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let web = ServiceName::parse("web").unwrap();
    store
        .pin(
            &built,
            &std::collections::BTreeMap::from([(web.clone(), backend::sha(HEAD))]),
        )
        .unwrap();
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    store.claim(&built, &runner).unwrap();
    store
        .record(
            &built,
            &runner,
            ployz_store::RunEvidence::Build(ployz_store::BuildReport {
                service: web,
                status: ployz_store::BuildStatus::Built,
                message: None,
                log: String::new(),
            }),
        )
        .unwrap();

    // The preview moves web to another repository; trusted, as a Cloud edit is.
    let mut api = vouched();
    api.repositories.push(AuthorizedRepository {
        repository: backend::repo_name("acme/api"),
        repository_id: backend::repo_id(12),
        access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
        default_branch: backend::git_branch("main"),
        branches: Vec::new(),
    });
    store
        .write_trusted(
            &who,
            &Edit {
                environment: at("pr-5"),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("web.repository").unwrap(),
                    value: json!("acme/api"),
                }],
            },
            &api,
        )
        .unwrap();
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let [admitted] = pushed.admitted.as_slice() else {
        panic!("one Deployment: {pushed:?}")
    };
    // web's last build came from acme/web: on acme/api it resolves its branch's head.
    let sources = store.sources(&admitted.deployment.id).unwrap();
    assert_eq!(
        sources
            .iter()
            .map(|source| (source.service.as_str(), source.commit.clone()))
            .collect::<Vec<_>>(),
        [("web", None)]
    );
}
