#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Protected Environments, on SQLite and on Postgres (see `backend`): a new Project's
//! Default starts Protected, the switch flips either way, only a Project's own
//! Environments take it, and only a caller holding `environments:protect` saves one a new repository
//! or deploy branch, however it was staged.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::ServiceGitAccess;
use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Approval, ApprovalDigest, AuthorizedRepository, Automated, Change, ConfigStore,
    CreateBranch, CreateEnvironment, CreateGitService, CreateProject, CreateService, DiffQuery,
    Edit, EnvironmentId, EnvironmentName, EnvironmentRef, EnvironmentsQuery, OrganizationId,
    Permission, Principal, ProjectId, ProjectName, Publish, Published, PullRequest, SeenProtection,
    ServiceLineageId, SetDefaultEnvironment, SetEnvironmentProtected, SetPrPlan, SettingPath,
    SyncChanges, SyncQuery, SystemEvent, Trusted, Written,
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
        ..ployz_store::Trusted::default()
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
    assert_eq!(view.environment, EnvironmentId::parse(uuid(4)).unwrap());
    let listed: Vec<_> = view
        .view
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

/// A token holding only `environments:protect`, limited to `environments`.
fn protecting_only(environments: &[&str]) -> Trusted {
    Trusted {
        permissions: Some([Permission::EnvironmentsProtect].into()),
        environments: Some(
            environments
                .iter()
                .map(|name| EnvironmentName::parse(*name).unwrap())
                .collect(),
        ),
        ..Trusted::default()
    }
}

#[test]
fn a_token_limited_to_an_environment_protects_it_and_no_other() {
    let (store, who) = shop();
    let who = person(&who);
    let token = protecting_only(&["staging"]);

    store
        .write_trusted(&who, &set("staging", true), &token)
        .unwrap();
    assert_eq!(
        protection(&store, &who),
        [("production".into(), true), ("staging".into(), true)]
    );
    store
        .write_trusted(&who, &set("staging", false), &token)
        .unwrap();
    assert_eq!(
        protection(&store, &who),
        [("production".into(), true), ("staging".into(), false)]
    );

    for protected in [false, true] {
        let refused = store
            .write_trusted(&who, &set("production", protected), &token)
            .unwrap_err();
        assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
        assert_eq!(refused.message, "This token can deploy only to staging.");
    }
    assert_eq!(
        protection(&store, &who),
        [("production".into(), true), ("staging".into(), false)]
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
        .write_trusted(
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
            &acme_as(false),
        )
        .unwrap();
}

/// The permissions Cloud found the caller holds: every one, or every one but protecting.
fn holding(protects: bool) -> Option<BTreeSet<Permission>> {
    Some(
        Permission::ALL
            .iter()
            .copied()
            .filter(|permission| protects || *permission != Permission::EnvironmentsProtect)
            .collect(),
    )
}

/// Cloud's evidence for `acme`, for a caller who holds `environments:protect` or not.
fn acme_as(protects: bool) -> Trusted {
    Trusted {
        permissions: holding(protects),
        ..acme()
    }
}

/// Stage `changes` in `environment`; a change without a value unsets its path.
fn edit(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    changes: &[(&str, Option<Value>)],
    protects: bool,
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
        .write_trusted(who, &edit, &acme_as(protects))
        .map(drop)
}

/// Sync everything fix-web changed into production.
fn sync_up(store: &ConfigStore, who: &Actor, protects: bool) -> Result<(), RpcError> {
    sync_as(store, who, &acme_as(protects))
}

fn sync_as(store: &ConfigStore, who: &Actor, trusted: &Trusted) -> Result<(), RpcError> {
    sync_closing(store, who, trusted, false)
}

fn sync_closing(
    store: &ConfigStore,
    who: &Actor,
    trusted: &Trusted,
    close_after: bool,
) -> Result<(), RpcError> {
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
                close_after,
                version: view.version,
                picks: None,
                skip: Vec::new(),
                values: BTreeMap::new(),
                id: None,
            },
            trusted,
        )
        .map(drop)
}

fn publish(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    protects: bool,
) -> Result<Published, RpcError> {
    publish_as(store, who, environment, &acme_as(protects))
}

fn publish_as(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    trusted: &Trusted,
) -> Result<Published, RpcError> {
    store.write_trusted(
        who,
        &Publish {
            environment: at(environment),
            ..Publish::default()
        },
        trusted,
    )
}

/// Assert `refused` asks an admin to approve `what` in Protected production, and
/// return Cloud's evidence for a caller without `environments:protect` holding that
/// approval.
fn admin_approved(refused: RpcError, what: &str) -> Trusted {
    assert_eq!(refused.code, RpcErrorCode::from("approval_required"));
    assert_eq!(
        refused.message,
        format!("production is Protected; an admin must approve this {what}.")
    );
    assert_eq!(refused.details["protected"], json!(true));
    assert!(refused.details["effects"].is_array());
    let version = refused.details["diff"]["version"].as_str().unwrap();
    let approval = refused.details["approval"].as_str().unwrap();
    assert!(approval.starts_with(&format!("{version}:")), "{approval}");
    Trusted {
        approval: Approval::Approved(ApprovalDigest::parse(approval).unwrap()),
        ..acme_as(false)
    }
}

fn assert_forbidden<T: std::fmt::Debug>(result: Result<T, RpcError>, details: Value) {
    let error = result.unwrap_err();
    assert_eq!(error.code, RpcErrorCode::from("forbidden"));
    assert_eq!(error.message, "Needs environments:protect");
    let mut expected = json!({"permission": "environments:protect"});
    expected
        .as_object_mut()
        .unwrap()
        .extend(details.as_object().unwrap().clone());
    assert_eq!(error.details, expected);
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
fn a_repository_synced_into_protected_production_needs_environments_protect() {
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
fn a_git_service_synced_into_protected_production_needs_environments_protect() {
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
            &acme_as(false),
        )
        .unwrap();

    assert_forbidden(sync_up(&store, &who, false), json!({"services": ["docs"]}));
    sync_up(&store, &who, true).unwrap();
}

#[test]
fn a_protected_services_branch_needs_environments_protect() {
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
fn unsetting_a_protected_services_repository_needs_environments_protect() {
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
fn restoring_an_old_branch_from_history_needs_environments_protect() {
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
    let restore = |protects| {
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
            &acme_as(protects),
        )
    };

    assert_forbidden(restore(false), json!({"services": ["web"]}));
    assert_eq!(production(&store, &who, "web.branch"), json!("dev"));
    restore(true).unwrap();
    assert_eq!(production(&store, &who, "web.branch"), json!("main"));
}

#[test]
fn a_source_staged_before_protecting_still_publishes_only_for_environments_protect() {
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
fn a_save_into_protected_production_asks_an_admin_even_when_cloud_requires_nothing() {
    let (store, system) = shop();
    let who = person(&system);
    store
        .write_trusted(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(6)).unwrap(),
                environment: at("production"),
                name: ServiceName::parse("api").unwrap(),
                image: Some("api:1".into()),
                template: None,
            },
            &acme_as(false),
        )
        .unwrap();

    let refused = publish(&store, &who, "production", false).unwrap_err();
    let approved = admin_approved(refused, "deploy");
    assert!(
        publish_as(&store, &who, "production", &approved)
            .unwrap()
            .created
    );
    assert!(
        publish(&store, &who, "production", false).is_ok(),
        "a resave asks nothing"
    );

    edit(
        &store,
        &who,
        "production",
        &[("api.replicas", Some(json!(2)))],
        false,
    )
    .unwrap();
    assert!(publish(&store, &who, "production", true).unwrap().created);
}

/// Production's staged `web.env.FLAG`, if any.
fn production_env(store: &ConfigStore, who: &Actor) -> Option<Value> {
    let query = ployz_store::EnvironmentQuery {
        environment: at("production"),
        path: Some(SettingPath::parse("web.env.FLAG").unwrap()),
        all: false,
    };
    let settings = store.read(who, &query).ok()?.settings;
    settings.into_iter().next().map(|setting| setting.value)
}

#[test]
fn a_sync_approval_covers_only_the_changes_it_reviewed() {
    let (store, system) = shop();
    let who = person(&system);
    fix_web(&store, &who);
    edit(
        &store,
        &who,
        "fix-web",
        &[("web.env.FLAG", Some(json!("on")))],
        false,
    )
    .unwrap();
    let before = store
        .read(
            &who,
            &DiffQuery {
                environment: at("production"),
            },
        )
        .unwrap()
        .version;

    let refused = sync_up(&store, &who, false).unwrap_err();
    let approval = refused.details["approval"].as_str().unwrap().to_owned();
    assert!(approval.starts_with(&format!("{before}:")), "{approval}");
    let approved = admin_approved(refused, "sync");

    let closing = sync_closing(&store, &who, &approved, true).unwrap_err();
    assert_eq!(closing.code, RpcErrorCode::from("approval_required"));
    assert_ne!(closing.details["approval"], json!(approval));

    edit(
        &store,
        &who,
        "fix-web",
        &[("web.env.FLAG", Some(json!("off")))],
        false,
    )
    .unwrap();
    let changed = sync_as(&store, &who, &approved).unwrap_err();
    assert_eq!(changed.code, RpcErrorCode::from("approval_required"));
    assert_ne!(changed.details["approval"], json!(approval));
    assert!(production_env(&store, &who).is_none());

    sync_as(&store, &who, &admin_approved(changed, "sync")).unwrap();
    assert_eq!(production_env(&store, &who), Some(json!("off")));
}

#[test]
fn a_sync_into_protected_production_asks_an_admin_and_stages_nothing_until_approved() {
    let (store, system) = shop();
    let who = person(&system);
    fix_web(&store, &who);
    edit(
        &store,
        &who,
        "fix-web",
        &[("web.env.FLAG", Some(json!("on")))],
        false,
    )
    .unwrap();

    let refused = sync_up(&store, &who, false).unwrap_err();
    assert!(production_env(&store, &who).is_none());
    let approved = admin_approved(refused, "sync");
    sync_as(&store, &who, &approved).unwrap();
    assert_eq!(production_env(&store, &who), Some(json!("on")));
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
fn only_environments_protect_flips_protection_even_after_another_protects() {
    let (store, system) = shop();
    let who = person(&system);
    let flip = |environment, protected, protects| {
        store.write_trusted(
            &who,
            &set(environment, protected),
            &Trusted {
                permissions: holding(protects),
                ..Trusted::default()
            },
        )
    };

    flip("staging", true, true).unwrap();
    assert_forbidden(flip("staging", false, false), json!({}));
    assert_forbidden(flip("production", true, false), json!({}));
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
        permissions: holding(true),
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
        environment: EnvironmentId::parse(uuid(4)).unwrap(),
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
fn an_unprotect_decided_on_another_environment_refuses() {
    let (store, system) = shop();
    // Both Protected once, so only which Environment it is tells them apart.
    store.write(&system, &set("staging", true)).unwrap();
    let production = SeenProtection {
        environment: EnvironmentId::parse(uuid(2)).unwrap(),
        protected: true,
        protections: 1,
    };
    // A human approved unprotecting the Default, production; staging became the
    // Default before the agent's write landed.
    store
        .write(
            &system,
            &SetDefaultEnvironment {
                environment: at("staging"),
            },
        )
        .unwrap();
    let error = store
        .write_trusted(
            &person(&system),
            &SetEnvironmentProtected {
                environment: EnvironmentRef::default(),
                protected: false,
                seen: Some(production),
            },
            &Trusted {
                permissions: holding(true),
                ..Trusted::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(
        error.details,
        json!({"project": "shop", "environment": "staging"})
    );
    assert_eq!(
        protection(&store, &system),
        vec![("production".into(), true), ("staging".into(), true)]
    );
}

#[test]
fn removing_a_project_with_a_protected_environment_needs_environments_protect() {
    let remove = |store: &ConfigStore, who: &Actor, protects| {
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
                permissions: holding(protects),
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
