use ployz_core::ServiceName;
use ployz_core::config::ServiceGitAccess;
use ployz_store::{
    Actor, AuthorizedRepository, Automated, BranchHead, Change, CheckSuite, ConfigStore,
    CreateGitService, CreateProject, DiffQuery, DiffView, Edit, EnvironmentId, EnvironmentName,
    EnvironmentQuery, EnvironmentRef, EnvironmentsQuery, HistoryQuery, MergeRow, NeverSync,
    NodeName, OnMerge, OnMergeCheck, OnMergeQuery, OrganizationId, ProjectId, ProjectName, Publish,
    PullRequest, PullRequestQuery, Ride, SavedRevision, ServiceLineageId, SetMergeValue, SetPrPlan,
    SettingPath, SystemEvent, Trusted, Written,
};
use serde_json::{Value, json};

use crate::backend;

pub(crate) const HEAD: &str = "1111111111111111111111111111111111111111";

pub(crate) fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

pub(crate) fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

pub(crate) fn vouched() -> Trusted {
    Trusted {
        repositories: vec![AuthorizedRepository {
            repository: backend::repo_name("acme/web"),
            repository_id: backend::repo_id(11),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: backend::git_branch("main"),
            branches: vec![backend::git_branch("release")],
        }],
        ..Trusted::default()
    }
}

/// Project `shop`: `production` runs Git Service `web` from `acme/web`'s `main`, with
/// `A=1`, `C=1` and `D=1` saved; pull request #5 opened `pr-5` from it.
pub(crate) fn shop() -> (ConfigStore, Actor) {
    shop_with(|_, _| {})
}

/// [`shop`], with `setup` run on `production` before pull request #5 opens.
pub(crate) fn shop_with(setup: impl FnOnce(&ConfigStore, &Actor)) -> (ConfigStore, Actor) {
    shop_copying(setup, None)
}

/// [`shop_with`], with pr-5 also copying `copy`.
pub(crate) fn shop_copying(
    setup: impl FnOnce(&ConfigStore, &Actor),
    copy: Option<Vec<NodeName>>,
) -> (ConfigStore, Actor) {
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
            &vouched(),
        )
        .unwrap();
    set(
        &store,
        &who,
        "production",
        &[
            ("web.env.A", json!("1")),
            ("web.env.C", json!("1")),
            ("web.env.D", json!("1")),
        ],
    );
    save(&store, &who, "production");
    setup(&store, &who);
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy,
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    pull(&store, &who, "main", "2026-09-29T10:00:00Z");
    (store, who)
}

/// Pull request #5, open, onto `target`.
pub(crate) fn pull(store: &ConfigStore, who: &Actor, target: &str, updated: &str) {
    let event = SystemEvent::PullRequest(PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: backend::git_branch("search"),
        head: backend::sha(HEAD),
        target_branch: backend::git_branch(target),
        commits: 1,
        open: true,
        merge_commit: None,
        target_head: None,
        updated: ployz_store::GithubTimestamp::parse(updated).unwrap(),
    });
    store
        .system(&who.organization, &event, &Trusted::default())
        .unwrap();
}

pub(crate) fn set(store: &ConfigStore, who: &Actor, environment: &str, changes: &[(&str, Value)]) {
    edit(
        store,
        who,
        environment,
        changes
            .iter()
            .map(|(path, value)| Change::Set {
                path: SettingPath::parse(path).unwrap(),
                value: value.clone(),
            })
            .collect(),
    );
}

pub(crate) fn unset(store: &ConfigStore, who: &Actor, environment: &str, path: &str) {
    let path = SettingPath::parse(path).unwrap();
    edit(store, who, environment, vec![Change::Unset { path }]);
}

pub(crate) fn edit(store: &ConfigStore, who: &Actor, environment: &str, changes: Vec<Change>) {
    store
        .write_trusted(
            who,
            &Edit {
                environment: at(environment),
                expect: None,
                changes,
            },
            &vouched(),
        )
        .unwrap();
}

pub(crate) fn save(store: &ConfigStore, who: &Actor, environment: &str) {
    store
        .write(
            who,
            &ployz_store::Command::Publish(Publish {
                environment: at(environment),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
}

pub(crate) fn on_merge(store: &ConfigStore, who: &Actor) -> OnMerge {
    store
        .read(
            who,
            &OnMergeQuery {
                environment: at("pr-5"),
            },
        )
        .unwrap()
}

/// The rows landing in production, by label.
pub(crate) fn rows(store: &ConfigStore, who: &Actor) -> Vec<MergeRow> {
    let on_merge = on_merge(store, who);
    assert_eq!(on_merge.destinations.len(), 1, "{on_merge:?}");
    assert_eq!(on_merge.destinations[0].name.as_str(), "production");
    on_merge.destinations[0].rows.clone()
}

pub(crate) fn labels(store: &ConfigStore, who: &Actor) -> Vec<(String, Ride)> {
    rows(store, who)
        .into_iter()
        .map(|row| (row.at.to_string(), row.ride))
        .collect()
}

pub(crate) fn row(store: &ConfigStore, who: &Actor, label: &str) -> MergeRow {
    rows(store, who)
        .into_iter()
        .find(|row| row.at.to_string() == label)
        .unwrap_or_else(|| panic!("no {label}"))
}

pub(crate) fn exclude(store: &ConfigStore, who: &Actor, label: &str, off: bool) {
    store
        .write(
            who,
            &NeverSync {
                environment: at("pr-5"),
                rows: vec![label.into()],
                off,
            },
        )
        .unwrap();
}

pub(crate) fn merge_value(label: &str, value: Option<&str>) -> SetMergeValue {
    SetMergeValue {
        environment: at("pr-5"),
        into: None,
        row: label.into(),
        value: value.map(Into::into),
    }
}

pub(crate) const MERGE: &str = "3333333333333333333333333333333333333333";
pub(crate) const LATER: &str = "4444444444444444444444444444444444444444";

pub(crate) fn system(store: &ConfigStore, who: &Actor, event: &SystemEvent) -> Automated {
    let Written::Automated(automated) = store
        .system(&who.organization, event, &Trusted::default())
        .unwrap()
    else {
        panic!("a system event writes Automated")
    };
    automated
}

/// Pull request #5 merged at `commit`, as GitHub said at `updated`.
pub(crate) fn merged(store: &ConfigStore, who: &Actor, updated: &str, commit: &str) -> Automated {
    merged_within(store, who, updated, commit, None)
}

/// Pull request #5 merged at `commit`, with GitHub saying `main`'s head `target_head`
/// contains it.
pub(crate) fn merged_within(
    store: &ConfigStore,
    who: &Actor,
    updated: &str,
    commit: &str,
    target_head: Option<&str>,
) -> Automated {
    let event = SystemEvent::PullRequest(PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: backend::git_branch("search"),
        head: backend::sha(HEAD),
        target_branch: backend::git_branch("main"),
        commits: 1,
        open: false,
        merge_commit: Some(backend::sha(commit)),
        target_head: target_head.map(backend::sha),
        updated: ployz_store::GithubTimestamp::parse(updated).unwrap(),
    });
    system(store, who, &event)
}

/// A push to `main`.
pub(crate) fn push(base: Option<&str>, head: &str, changed: Option<&[&str]>) -> SystemEvent {
    SystemEvent::BranchHead(BranchHead {
        repository_id: backend::repo_id(11),
        branch: backend::git_branch("main"),
        base: base.map(backend::sha),
        head: Some(backend::sha(head)),
        changed: changed.map(|paths| paths.iter().map(|path| (*path).to_owned()).collect()),
    })
}

/// The Services each admitted Deployment targets.
pub(crate) fn deployed(automated: &Automated) -> Vec<Vec<String>> {
    automated
        .admitted
        .iter()
        .map(|admitted| {
            admitted
                .deployment
                .services
                .iter()
                .map(ToString::to_string)
                .collect()
        })
        .collect()
}

/// `environment`'s Working value at `path`.
pub(crate) fn value(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    path: &str,
) -> Option<Value> {
    let query = EnvironmentQuery {
        environment: at(environment),
        path: None,
        all: true,
    };
    store
        .read(who, &query)
        .unwrap()
        .settings
        .into_iter()
        .find(|row| row.path.to_string() == path)
        .map(|row| row.value)
}

pub(crate) fn history(store: &ConfigStore, who: &Actor) -> Vec<SavedRevision> {
    store
        .read(
            who,
            &HistoryQuery {
                environment: at("production"),
                limit: None,
            },
        )
        .unwrap()
        .revisions
}

pub(crate) fn production_diff(store: &ConfigStore, who: &Actor) -> DiffView {
    store
        .read(
            who,
            &DiffQuery {
                environment: at("production"),
            },
        )
        .unwrap()
}

pub(crate) fn preview_listed(store: &ConfigStore, who: &Actor) -> bool {
    store
        .read(who, &EnvironmentsQuery::default())
        .unwrap()
        .environments
        .iter()
        .any(|listing| listing.name.as_str() == "pr-5")
}

/// What pull request #5's check says.
pub(crate) fn check(store: &ConfigStore, who: &Actor) -> Option<OnMergeCheck> {
    store
        .read(
            who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap()
        .check
}

/// CI passed on `head`.
pub(crate) fn passed(store: &ConfigStore, who: &Actor, head: &str) -> Automated {
    system(
        store,
        who,
        &SystemEvent::CheckSuite(CheckSuite {
            repository_id: backend::repo_id(11),
            suite: 1,
            head: backend::sha(head),
            status: serde_json::from_value(json!("completed")).unwrap(),
            conclusion: Some(serde_json::from_value(json!("success")).unwrap()),
            updated: ployz_store::GithubTimestamp::parse("2026-09-29T11:30:00Z").unwrap(),
        }),
    )
}

/// `web` deploys only for changes under `web/`; main's head is `HEAD`.
pub(crate) fn watching(store: &ConfigStore, who: &Actor) {
    set(
        store,
        who,
        "production",
        &[("web.watchPaths", json!(["web/**"]))],
    );
    system(store, who, &push(None, HEAD, None));
}

pub(crate) fn config(store: &ConfigStore, who: &Actor, n: u8, name: &str, on: &[&str]) {
    store
        .write(
            who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(n)).unwrap(),
                environment: at("production"),
                name: ployz_core::ConfigName::parse(name).unwrap(),
                mounts: on
                    .iter()
                    .map(|service| ployz_store::ConfigMountAt {
                        service: ServiceName::parse(*service).unwrap(),
                        dir: format!("/etc/{name}"),
                    })
                    .collect(),
            },
        )
        .unwrap();
}

pub(crate) fn runner() -> ployz_store::RunnerId {
    ployz_store::RunnerId::parse("runner").unwrap()
}
