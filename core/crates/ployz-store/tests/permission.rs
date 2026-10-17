//! Every command refuses a caller whose permissions, as Cloud found them, lack the one
//! it names, before it reads or writes anything. No permission set means no one
//! checked, which is only the hidden local Store.

use std::collections::BTreeSet;

use ployz_core::{RpcError, ServiceName};
use ployz_store::{
    Actor, CancelRemoval, Change, ConfigStore, CreateProject, CreateService, DeploymentId, Edit,
    EnvironmentId, EnvironmentName, EnvironmentQuery, EnvironmentRef, OrganizationId, Permission,
    ProjectId, ProjectName, Remove, ServiceLineageId, SetBuildOrder, SettingPath, Start, Trusted,
};
use serde_json::json;

mod backend;

fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    (store, who)
}

fn holding(permissions: &[Permission]) -> Trusted {
    Trusted {
        permissions: Some(permissions.iter().copied().collect::<BTreeSet<_>>()),
        ..Trusted::default()
    }
}

fn set_replicas(count: u8) -> Edit {
    Edit {
        environment: EnvironmentRef::default(),
        expect: None,
        changes: vec![Change::Set {
            path: SettingPath::parse("web.replicas").unwrap(),
            value: json!(count),
        }],
    }
}

fn revision(store: &ConfigStore, who: &Actor) -> u64 {
    let query = EnvironmentQuery {
        environment: EnvironmentRef::default(),
        path: None,
        all: false,
    };
    store.read(who, &query).unwrap().environment.revision.0
}

fn needs(refused: &RpcError, permission: &str) {
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert_eq!(refused.message, format!("Needs {permission}"));
    assert_eq!(refused.details, json!({ "permission": permission }));
}

#[test]
fn a_write_without_the_permission_it_names_is_forbidden_and_writes_nothing() {
    let (store, who) = shop();
    let before = revision(&store, &who);
    let refused = store
        .write_trusted(
            &who,
            &set_replicas(2),
            &holding(&[Permission::ProjectRead, Permission::ChangesDeploy]),
        )
        .unwrap_err();
    needs(&refused, "changes:stage");
    assert_eq!(revision(&store, &who), before);
}

#[test]
fn a_write_with_the_permission_it_names_lands() {
    let (store, who) = shop();
    let before = revision(&store, &who);
    store
        .write_trusted(
            &who,
            &set_replicas(2),
            &holding(&[Permission::ChangesStage]),
        )
        .unwrap();
    assert_eq!(revision(&store, &who), before + 1);
}

#[test]
fn no_permission_set_means_no_one_checked() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &set_replicas(2), &Trusted::default())
        .unwrap();
}

#[test]
fn a_deploy_without_changes_deploy_is_refused_before_it_looks_anything_up() {
    let (store, who) = shop();
    let start = Start {
        deployment: DeploymentId::parse("00000000-0000-4000-8000-000000000999").unwrap(),
    };
    let refused = store
        .write_trusted(&who, &start, &holding(&[Permission::ChangesStage]))
        .unwrap_err();
    needs(&refused, "changes:deploy");
    let missing = store
        .write_trusted(&who, &start, &holding(&[Permission::ChangesDeploy]))
        .unwrap_err();
    assert_eq!(missing.code.as_str(), "not_found", "{missing:?}");
}

#[test]
fn the_build_order_and_a_removal_cancel_name_their_own_permissions() {
    let (store, who) = shop();
    let everything_but = |missing: Permission| {
        holding(
            &Permission::ALL
                .iter()
                .copied()
                .filter(|p| *p != missing)
                .collect::<Vec<_>>(),
        )
    };
    let refused = store
        .write_trusted(
            &who,
            &SetBuildOrder { build_order: None },
            &everything_but(Permission::OrgSettings),
        )
        .unwrap_err();
    needs(&refused, "org:settings");
    let cancel = CancelRemoval {
        project: ProjectName::parse("shop").unwrap(),
        environment: Some(EnvironmentName::parse("production").unwrap()),
    };
    let refused = store
        .write_trusted(
            &who,
            &cancel,
            &everything_but(Permission::EnvironmentDelete),
        )
        .unwrap_err();
    needs(&refused, "environment:delete");
}

#[test]
fn removing_a_project_needs_project_delete_on_top_of_environment_delete() {
    let (store, who) = shop();
    let remove = |environment: Option<&str>| Remove {
        project: ProjectName::parse("shop").unwrap(),
        environment: environment.map(|name| EnvironmentName::parse(name).unwrap()),
        version: None,
        accept_volume_loss: Default::default(),
        every_copy: Default::default(),
    };
    let refused = store
        .write_trusted(
            &who,
            &remove(None),
            &holding(&[Permission::EnvironmentDelete]),
        )
        .unwrap_err();
    needs(&refused, "project:delete");
    let refused = store
        .write_trusted(
            &who,
            &remove(Some("production")),
            &holding(&[Permission::ProjectDelete]),
        )
        .unwrap_err();
    needs(&refused, "environment:delete");
}
