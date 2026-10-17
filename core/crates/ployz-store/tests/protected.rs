//! Protected Environments, on SQLite and on Postgres (see `backend`): a new Project's
//! Default starts Protected, the switch flips either way, only a Project's own
//! Environments take it, and only a caller who may protect saves one a new repository
//! or deploy branch, however it was staged.

use std::collections::BTreeMap;

use ployz_core::config::ServiceGitAccess;
use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AuthorizedRepository, Automated, Change, ConfigStore, CreateBranch, CreateEnvironment,
    CreateGitService, CreateProject, CreateService, Edit, EnvironmentId, EnvironmentName,
    EnvironmentRef, EnvironmentsQuery, OrganizationId, Principal, ProjectId, ProjectName, Publish,
    Published, PullRequest, SeenProtection, ServiceLineageId, SetEnvironmentProtected, SetPrPlan,
    SettingPath, SyncChanges, SyncQuery, SystemEvent, Trusted, Written,
};
use serde_json::{Value, json};

mod backend;

const HEAD: &str = "1111111111111111111111111111111111111111";

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

/// Cloud's evidence for `acme/web` and `acme/docs`, each with a `dev` branch.
fn acme() -> Trusted {
    let repository = |name: &str, n| AuthorizedRepository {
        repository: backend::repo_name(name),
        repository_id: backend::repo_id(n),
        access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
        default_branch: backend::git_branch("main"),
        branches: vec![backend::git_branch("dev")],
    };
    Trusted {
        repositories: vec![repository("acme/web", 11), repository("acme/docs", 12)],
        ..backend::cloud()
    }
}

/// Project `shop`: Default `production` runs Git Service `web` from `acme/web`'s `main`,
/// published, beside an empty `staging`.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(3)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                repository: backend::repo_name("acme/web"),
                branch: None,
            },
            &acme(),
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::Command::Publish(ployz_store::Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateEnvironment {
                id: EnvironmentId::parse(uuid(4)).unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    (store, who)
}

/// Each Environment's name and whether it is Protected.
fn protection(store: &ConfigStore, who: &Actor) -> Vec<(String, bool)> {
    store
        .read(who, &EnvironmentsQuery::default())
        .unwrap()
        .environments
        .into_iter()
        .map(|listing| (listing.name.to_string(), listing.protected))
        .collect()
}

fn set(environment: &str, protected: bool) -> SetEnvironmentProtected {
    SetEnvironmentProtected {
        environment: at(environment),
        protected,
        seen: None,
    }
}

#[test]
fn a_new_projects_default_environment_starts_protected() {
    let (store, who) = shop();

    assert_eq!(
        protection(&store, &who),
        [("production".into(), true), ("staging".into(), false)]
    );
}

#[test]
fn protecting_flips_either_way_and_returns_the_projects_environments() {
    let (store, who) = shop();

    let Written::Protected(view) = store
        .write(
            &who,
            &ployz_store::Command::SetEnvironmentProtected(set("staging", true)),
        )
        .unwrap()
    else {
        panic!("protecting writes Protected")
    };
    let listed: Vec<_> = view
        .environments
        .iter()
        .map(|listing| (listing.name.to_string(), listing.protected))
        .collect();
    assert_eq!(
        listed,
        [("production".into(), true), ("staging".into(), true)]
    );

    store.write(&who, &set("production", false)).unwrap();
    store.write(&who, &set("staging", false)).unwrap();
    assert_eq!(
        protection(&store, &who),
        [("production".into(), false), ("staging".into(), false)]
    );

    store.write(&who, &set("staging", false)).unwrap();
    assert_eq!(
        protection(&store, &who),
        [("production".into(), false), ("staging".into(), false)]
    );
}

#[test]
fn each_protection_counts_once_so_an_unprotect_approval_holds_for_one() {
    let (store, who) = shop();
    let protections = || {
        store
            .read(&who, &EnvironmentsQuery::default())
            .unwrap()
            .environments
            .into_iter()
            .map(|listing| (listing.name.to_string(), listing.protections))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        protections(),
        [("production".into(), 1), ("staging".into(), 0)]
    );

    store.write(&who, &set("production", true)).unwrap();
    store.write(&who, &set("staging", false)).unwrap();
    assert_eq!(
        protections(),
        [("production".into(), 1), ("staging".into(), 0)]
    );

    store.write(&who, &set("production", false)).unwrap();
    store.write(&who, &set("production", true)).unwrap();
    store.write(&who, &set("staging", true)).unwrap();
    assert_eq!(
        protections(),
        [("production".into(), 2), ("staging".into(), 1)]
    );
}

#[test]
fn a_branch_cannot_be_protected() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(5)).unwrap(),
                from: at("production"),
                name: EnvironmentName::parse("fix-web").unwrap(),
                copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                live: Vec::new(),
                setup: Vec::new(),
                keep: false,
                fix: None,
            },
        )
        .unwrap();

    let error = store.write(&who, &set("fix-web", true)).unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unsupported);
    assert_eq!(
        error.message,
        "fix-web is a Branch: only a Project's own Environments can be Protected"
    );
    assert_eq!(
        error.details,
        json!({"project": "shop", "environment": "fix-web"})
    );
    assert!(
        protection(&store, &who)
            .iter()
            .all(|(name, protected)| name != "fix-web" || !protected)
    );
}

#[test]
fn a_pr_environment_cannot_be_protected() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy: None,
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    let Written::Automated(Automated { .. }) = store
        .system(
            &who.organization,
            &SystemEvent::PullRequest(PullRequest {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
                title: "Add search".into(),
                author: "ada".into(),
                author_id: Some(backend::ada()),
                bot: false,
                head_branch: backend::git_branch("search"),
                head_repository_id: Some(backend::repo_id(11)),
                head: backend::sha(HEAD),
                target_branch: backend::git_branch("main"),
                commits: 1,
                open: true,
                merge_commit: None,
                target_head: None,
                updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:00:01Z").unwrap(),
            }),
            &backend::members(),
        )
        .unwrap()
    else {
        panic!("a pull request writes Automated")
    };

    let error = store.write(&who, &set("pr-5", true)).unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unsupported);
    assert_eq!(
        error.message,
        "pr-5 is a PR Environment: only a Project's own Environments can be Protected"
    );
    assert!(protection(&store, &who).contains(&("pr-5".into(), false)));
}

/// `who` as a person, rather than the Store's own automation.
fn person(who: &Actor) -> Actor {
    Actor {
        principal: Some(Principal::parse("nick").unwrap()),
        ..who.clone()
    }
}

fn fix_web(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(5)).unwrap(),
                from: at("production"),
                name: EnvironmentName::parse("fix-web").unwrap(),
                copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                live: Vec::new(),
                setup: Vec::new(),
                keep: false,
                fix: None,
            },
        )
        .unwrap();
}

/// Cloud's evidence for `acme`, and whether it found the caller may protect.
fn acme_as(may_protect: bool) -> Trusted {
    Trusted {
        may_protect,
        ..acme()
    }
}

/// Stage `changes` in `environment`; a change without a value unsets its path.
fn edit(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    changes: &[(&str, Option<Value>)],
    may_protect: bool,
) -> Result<(), RpcError> {
    let changes = changes
        .iter()
        .map(|(path, value)| {
            let path = SettingPath::parse(path).unwrap();
            match value {
                Some(value) => Change::Set {
                    path,
                    value: value.clone(),
                },
                None => Change::Unset { path },
            }
        })
        .collect();
    let edit = Edit {
        environment: at(environment),
        expect: None,
        changes,
    };
    store
        .write_trusted(who, &edit, &acme_as(may_protect))
        .map(drop)
}

/// Sync everything fix-web changed into production.
fn sync_up(store: &ConfigStore, who: &Actor, may_protect: bool) -> Result<(), RpcError> {
    let view = store
        .read(
            who,
            &SyncQuery {
                from: at("fix-web"),
                into: Some(at("production")),
            },
        )
        .unwrap();
    store
        .write_trusted(
            who,
            &SyncChanges {
                from: at("fix-web"),
                into: Some(at("production")),
                close_after: false,
                version: view.version,
                picks: None,
                skip: Vec::new(),
                values: BTreeMap::new(),
                id: None,
            },
            &acme_as(may_protect),
        )
        .map(drop)
}

fn publish(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    may_protect: bool,
) -> Result<Published, RpcError> {
    store.write_trusted(
        who,
        &Publish {
            environment: at(environment),
            ..Publish::default()
        },
        &acme_as(may_protect),
    )
}

fn assert_forbidden<T: std::fmt::Debug>(result: Result<T, RpcError>, details: Value) {
    let error = result.unwrap_err();
    assert_eq!(error.code, RpcErrorCode::from("forbidden"));
    assert_eq!(
        error.message,
        "Only an owner or admin of this Organization can do this."
    );
    assert_eq!(error.details, details);
}

/// Whether `result` got past the protection check, refused or not for another reason.
fn passed<T>(result: &Result<T, RpcError>) -> bool {
    result.as_ref().map_or_else(
        |error| error.code != RpcErrorCode::from("forbidden"),
        |_| true,
    )
}

/// Production's staged value at `path`.
fn production(store: &ConfigStore, who: &Actor, path: &str) -> Value {
    store
        .read(
            who,
            &ployz_store::EnvironmentQuery {
                environment: at("production"),
                path: Some(SettingPath::parse(path).unwrap()),
                all: false,
            },
        )
        .unwrap()
        .settings
        .remove(0)
        .value
}

#[test]
fn a_repository_synced_into_protected_production_needs_whoever_may_protect() {
    let (store, system) = shop();
    let who = person(&system);
    fix_web(&store, &who);
    edit(
        &store,
        &who,
        "fix-web",
        &[("web.repository", Some(json!("acme/docs")))],
        false,
    )
    .unwrap();

    assert_forbidden(sync_up(&store, &who, false), json!({"services": ["web"]}));
    assert_eq!(
        production(&store, &who, "web.repository"),
        json!("acme/web")
    );

    sync_up(&store, &who, true).unwrap();
    assert!(publish(&store, &who, "production", true).unwrap().created);
}

#[test]
fn a_git_service_synced_into_protected_production_needs_whoever_may_protect() {
    let (store, system) = shop();
    let who = person(&system);
    fix_web(&store, &who);
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(7)).unwrap(),
                environment: at("fix-web"),
                name: ServiceName::parse("docs").unwrap(),
                repository: backend::repo_name("acme/docs"),
                branch: Some(backend::git_branch("dev")),
            },
            &acme(),
        )
        .unwrap();

    assert_forbidden(sync_up(&store, &who, false), json!({"services": ["docs"]}));
    sync_up(&store, &who, true).unwrap();
}

#[test]
fn a_protected_services_branch_needs_whoever_may_protect() {
    let (store, system) = shop();
    let who = person(&system);
    let dev = [("web.branch", Some(json!("dev")))];

    assert_forbidden(
        edit(&store, &who, "production", &dev, false),
        json!({"services": ["web"]}),
    );
    edit(&store, &who, "production", &dev, true).unwrap();
}

#[test]
fn unsetting_a_protected_services_repository_needs_whoever_may_protect() {
    let (store, system) = shop();
    let who = person(&system);
    let image = [
        ("web.repository", None),
        ("web.image", Some(json!("web:2"))),
    ];

    assert_forbidden(
        edit(&store, &who, "production", &image, false),
        json!({"services": ["web"]}),
    );
    edit(&store, &who, "production", &image, true).unwrap();
}

#[test]
fn restoring_an_old_branch_from_history_needs_whoever_may_protect() {
    let (store, system) = shop();
    edit(
        &store,
        &system,
        "production",
        &[("web.branch", Some(json!("dev")))],
        true,
    )
    .unwrap();
    publish(&store, &system, "production", true).unwrap();
    let who = person(&system);
    let query = ployz_store::HistoryPreviewQuery {
        environment: at("production"),
        revision: ployz_store::Revision(1),
        action: ployz_store::HistoryAction::Restore,
        change: None,
    };
    let restore = |may_protect| {
        let preview = store.read(&who, &query).unwrap();
        store.write_trusted(
            &who,
            &ployz_store::StageHistory {
                environment: at("production"),
                revision: query.revision,
                action: query.action,
                change: None,
                version: preview.version,
                accept_overwrite: true,
            },
            &acme_as(may_protect),
        )
    };

    assert_forbidden(restore(false), json!({"services": ["web"]}));
    assert_eq!(production(&store, &who, "web.branch"), json!("dev"));
    restore(true).unwrap();
    assert_eq!(production(&store, &who, "web.branch"), json!("main"));
}

#[test]
fn a_source_staged_before_protecting_still_publishes_only_for_whoever_may_protect() {
    let (store, system) = shop();
    let who = person(&system);
    store.write(&system, &set("production", false)).unwrap();
    edit(
        &store,
        &who,
        "production",
        &[("web.branch", Some(json!("dev")))],
        false,
    )
    .unwrap();
    store.write(&system, &set("production", true)).unwrap();

    assert_forbidden(
        publish(&store, &who, "production", false),
        json!({"services": ["web"]}),
    );
    assert!(publish(&store, &who, "production", true).unwrap().created);
}

#[test]
fn a_change_leaving_every_repository_and_branch_saves_for_anyone() {
    let (store, system) = shop();
    let who = person(&system);
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(6)).unwrap(),
                environment: at("production"),
                name: ServiceName::parse("api").unwrap(),
                image: Some("api:1".into()),
                template: None,
            },
        )
        .unwrap();

    assert!(publish(&store, &who, "production", false).unwrap().created);
}

#[test]
fn cloud_itself_and_an_unprotected_environment_save_any_branch() {
    let (store, system) = shop();
    fix_web(&store, &system);
    edit(
        &store,
        &system,
        "fix-web",
        &[("web.repository", Some(json!("acme/docs")))],
        true,
    )
    .unwrap();
    sync_up(&store, &system, true).unwrap();
    assert!(
        publish(&store, &system, "production", true)
            .unwrap()
            .created
    );

    let who = person(&system);
    store.write(&system, &set("production", false)).unwrap();
    edit(
        &store,
        &who,
        "production",
        &[("web.branch", Some(json!("dev")))],
        false,
    )
    .unwrap();
    assert!(publish(&store, &who, "production", false).unwrap().created);
}

#[test]
fn only_whoever_may_protect_flips_protection_even_after_another_protects() {
    let (store, system) = shop();
    let who = person(&system);
    let flip = |environment, protected, may_protect| {
        store.write_trusted(
            &who,
            &set(environment, protected),
            &Trusted {
                may_protect,
                ..Trusted::default()
            },
        )
    };

    // Cloud judged the member before an admin protected staging; the Store judges
    // the flag in the transaction that reads `protected`.
    flip("staging", true, true).unwrap();
    assert_forbidden(
        flip("staging", false, false),
        json!({"project": "shop", "environment": "staging"}),
    );
    assert_forbidden(
        flip("production", true, false),
        json!({"project": "shop", "environment": "production"}),
    );
    assert_eq!(
        protection(&store, &who),
        [("production".into(), true), ("staging".into(), true)]
    );

    flip("staging", false, true).unwrap();
    assert!(protection(&store, &who).contains(&("staging".into(), false)));
}

#[test]
fn an_unprotect_decided_on_an_earlier_protection_refuses() {
    let (store, system) = shop();
    let admin = Trusted {
        may_protect: true,
        ..Trusted::default()
    };
    let unprotect = |seen| {
        store.write_trusted(
            &person(&system),
            &SetEnvironmentProtected {
                seen: Some(seen),
                ..set("staging", false)
            },
            &admin,
        )
    };
    let stale = |result: Result<_, RpcError>, protected, protections| {
        let error = result.unwrap_err();
        assert_eq!(error.code, RpcErrorCode::Conflict);
        assert_eq!(
            error.details,
            json!({
                "project": "shop",
                "environment": "staging",
                "protected": protected,
                "protections": protections,
            })
        );
    };
    let seen = |protected, protections| SeenProtection {
        protected,
        protections,
    };

    // A human approved lifting the first protection; staging was unprotected and
    // protected again before the agent's write landed.
    store.write(&system, &set("staging", true)).unwrap();
    store.write(&system, &set("staging", false)).unwrap();
    store.write(&system, &set("staging", true)).unwrap();
    stale(unprotect(seen(true, 1)), true, 2);
    assert!(protection(&store, &system).contains(&("staging".into(), true)));

    unprotect(seen(true, 2)).unwrap();
    assert!(protection(&store, &system).contains(&("staging".into(), false)));

    // The agent saw staging unprotected, so no one was asked; a person protected it
    // before the write landed.
    store.write(&system, &set("staging", true)).unwrap();
    stale(unprotect(seen(false, 2)), true, 3);
    assert!(protection(&store, &system).contains(&("staging".into(), true)));
}

#[test]
fn an_unnamed_caller_cloud_found_no_role_for_may_not_protect() {
    let (store, system) = shop();
    // A user Cloud couldn't name (their row is gone) and found no role for is not automation.
    let unnamed =
        |protected| store.write_trusted(&system, &set("staging", protected), &Trusted::default());

    assert_forbidden(
        unnamed(true),
        json!({"project": "shop", "environment": "staging"}),
    );
    store.write(&system, &set("staging", true)).unwrap();
    assert_forbidden(
        unnamed(false),
        json!({"project": "shop", "environment": "staging"}),
    );
    assert!(protection(&store, &system).contains(&("staging".into(), true)));
}

#[test]
fn removing_a_project_with_a_protected_environment_needs_whoever_may_protect() {
    let remove = |store: &ConfigStore, who: &Actor, may_protect| {
        store.write_trusted(
            who,
            &ployz_store::Remove {
                project: ProjectName::parse("shop").unwrap(),
                environment: None,
                version: None,
                accept_volume_loss: BTreeMap::new(),
                every_copy: BTreeMap::new(),
            },
            &Trusted {
                may_protect,
                ..Trusted::default()
            },
        )
    };
    let (store, system) = shop();
    let who = person(&system);

    assert_forbidden(
        remove(&store, &who, false),
        json!({"protected": ["production"]}),
    );
    assert_eq!(protection(&store, &who).len(), 2);
    store.write(&system, &set("production", false)).unwrap();
    let unprotected = remove(&store, &who, false);
    assert!(passed(&unprotected), "{unprotected:?}");

    let (store, system) = shop();
    let admin = remove(&store, &person(&system), true);
    assert!(passed(&admin), "{admin:?}");
}
