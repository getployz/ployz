//! Every command refuses a caller whose permissions, as Cloud found them, lack the one
//! it names, before it reads or writes anything. No permission set means no one
//! checked, which is only the hidden local Store and the Store's own automation: a
//! named caller with none sent holds none.

use std::collections::BTreeSet;

use ployz_core::{RpcError, ServiceName};
use ployz_store::{
    Actor, Admit, Cancel, CancelRemoval, Change, Command, ConfigStore, CreateEnvironment,
    CreateProject, CreateService, Deploy, DeploymentId, Edit, EnvironmentId, EnvironmentName,
    EnvironmentQuery, EnvironmentRef, OrganizationId, Permission, Principal, ProjectId,
    ProjectName, Removal, Remove, RemoveService, RenameProject, ServiceLineageId, SetBuildOrder,
    SetPrPlan, SettingPath, Trusted,
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

fn everything_but(missing: Permission) -> Trusted {
    holding(
        &Permission::ALL
            .iter()
            .copied()
            .filter(|p| *p != missing)
            .collect::<Vec<_>>(),
    )
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

fn deploy_production(n: u8) -> Admit {
    Admit::Deploy(Deploy {
        id: DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap(),
        environment: EnvironmentRef {
            project: None,
            environment: Some(EnvironmentName::parse("production").unwrap()),
        },
        services: Vec::new(),
        version: None,
        upload: None,
        accept_volume_loss: Vec::new(),
        message: None,
        prune: false,
        no_prune: false,
    })
}

fn remove(environment: Option<&str>) -> Remove {
    Remove {
        project: ProjectName::parse("shop").unwrap(),
        environment: environment.map(|name| EnvironmentName::parse(name).unwrap()),
        version: None,
        accept_volume_loss: Default::default(),
        every_copy: Default::default(),
    }
}

/// Start removing the whole Project, deployed first so a Deployment takes it off;
/// that Deployment's id.
fn removing(store: &ConfigStore, who: &Actor) -> DeploymentId {
    backend::deploy(store, who, "production", 1);
    let mut remove = remove(None);
    let removal = match store.write(who, &remove) {
        Err(error) if error.code.as_str() == "confirmation_required" => {
            remove.version = error
                .details
                .get("version")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            store.write(who, &remove).unwrap()
        }
        other => other.unwrap(),
    };
    let Removal::Removing { left, .. } = removal else {
        panic!("the removal finished before anything could cancel it")
    };
    left.iter()
        .find_map(|left| left.deployment.as_ref().map(|d| d.id.clone()))
        .unwrap()
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
fn a_named_caller_with_no_permission_set_holds_none() {
    let (store, system) = shop();
    let ada = Actor {
        organization: system.organization.clone(),
        principal: Some(Principal::parse("ada").unwrap()),
    };
    let before = revision(&store, &system);
    let refused = store
        .write_trusted(&ada, &set_replicas(2), &Trusted::default())
        .unwrap_err();
    needs(&refused, "changes:stage");
    let refused = store
        .commit(&ada, &Command::Edit(set_replicas(2)), &Trusted::default())
        .unwrap_err();
    needs(&refused, "changes:stage");
    assert_eq!(revision(&store, &system), before);
    store
        .write_trusted(
            &ada,
            &set_replicas(2),
            &holding(&[Permission::ChangesStage]),
        )
        .unwrap();
    assert_eq!(revision(&store, &system), before + 1);
}

#[test]
fn the_system_actor_with_no_permission_set_is_unchecked() {
    let (store, who) = shop();
    let before = revision(&store, &who);
    store
        .write_trusted(&who, &set_replicas(2), &Trusted::default())
        .unwrap();
    assert_eq!(revision(&store, &who), before + 1);
}

#[test]
fn a_deploy_without_changes_deploy_is_refused_before_it_looks_anything_up() {
    let (store, who) = shop();
    let nowhere = Admit::Deploy(Deploy {
        id: DeploymentId::parse("00000000-0000-4000-8000-000000000999").unwrap(),
        environment: EnvironmentRef {
            project: None,
            environment: Some(EnvironmentName::parse("nowhere").unwrap()),
        },
        services: Vec::new(),
        version: None,
        upload: None,
        accept_volume_loss: Vec::new(),
        message: None,
        prune: false,
        no_prune: false,
    });
    let refused = store
        .write_trusted(&who, &nowhere, &holding(&[Permission::ChangesStage]))
        .unwrap_err();
    needs(&refused, "changes:deploy");
    let missing = store
        .write_trusted(&who, &nowhere, &holding(&[Permission::ChangesDeploy]))
        .unwrap_err();
    assert_eq!(missing.code.as_str(), "not_found", "{missing:?}");
}

#[test]
fn a_replayed_deploy_is_refused_like_a_new_one() {
    let (store, who) = shop();
    backend::admit(&store, &who, "production", 1);
    let before = revision(&store, &who);
    let refused = store
        .write_trusted(
            &who,
            &deploy_production(1),
            &holding(&[Permission::ChangesStage]),
        )
        .unwrap_err();
    needs(&refused, "changes:deploy");
    assert_eq!(revision(&store, &who), before);
}

#[test]
fn deploying_a_staged_destroy_needs_changes_destroy() {
    let (store, who) = shop();
    backend::deploy(&store, &who, "production", 1);
    store
        .write(
            &who,
            &RemoveService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    let before = revision(&store, &who);
    let mut keeping = deploy_production(2);
    if let Admit::Deploy(deploy) = &mut keeping {
        deploy.no_prune = true;
    }
    let refused = store
        .write_trusted(&who, &keeping, &holding(&[Permission::ChangesDeploy]))
        .unwrap_err();
    needs(&refused, "changes:destroy");
    assert_eq!(revision(&store, &who), before);
    store
        .write_trusted(
            &who,
            &deploy_production(2),
            &holding(&[
                Permission::ChangesDeploy,
                Permission::ChangesDestroy,
                Permission::EnvironmentsProtect,
            ]),
        )
        .unwrap();
}

#[test]
fn the_build_order_a_project_rename_and_a_pr_plan_name_their_own_permissions() {
    let (store, who) = shop();
    let refused = store
        .write_trusted(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: None,
                copy: None,
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
            &everything_but(Permission::ProjectEdit),
        )
        .unwrap_err();
    needs(&refused, "project:edit");
    let refused = store
        .write_trusted(
            &who,
            &SetBuildOrder { build_order: None },
            &everything_but(Permission::OrgSettings),
        )
        .unwrap_err();
    needs(&refused, "org:settings");
    let refused = store
        .write_trusted(
            &who,
            &RenameProject {
                project: ProjectName::parse("shop").unwrap(),
                name: ProjectName::parse("store").unwrap(),
            },
            &everything_but(Permission::ProjectEdit),
        )
        .unwrap_err();
    needs(&refused, "project:edit");
}

#[test]
fn removing_a_project_needs_project_delete_on_top_of_environment_delete() {
    let (store, who) = shop();
    let before = revision(&store, &who);
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
    assert_eq!(revision(&store, &who), before);
}

/// Stopping a removal is stopping its Deployment, so both routes to it answer a
/// caller the same way: changes:deploy is enough, and nothing less is.
#[test]
fn cancelling_a_removal_needs_changes_deploy_on_either_route() {
    let cancel = CancelRemoval {
        project: ProjectName::parse("shop").unwrap(),
        environment: None,
    };

    let (store, who) = shop();
    let deployment = removing(&store, &who);
    let refused = store
        .write_trusted(&who, &cancel, &everything_but(Permission::ChangesDeploy))
        .unwrap_err();
    needs(&refused, "changes:deploy");
    let refused = store
        .write_trusted(
            &who,
            &Cancel { deployment },
            &everything_but(Permission::ChangesDeploy),
        )
        .unwrap_err();
    needs(&refused, "changes:deploy");
    let cancelled = store
        .write_trusted(&who, &cancel, &holding(&[Permission::ChangesDeploy]))
        .unwrap();
    assert_eq!(cancelled.cancelled.len(), 1);

    let (store, who) = shop();
    let deployment = removing(&store, &who);
    store
        .write_trusted(
            &who,
            &Cancel {
                deployment: deployment.clone(),
            },
            &holding(&[Permission::ChangesDeploy]),
        )
        .unwrap();
    let over = store
        .write_trusted(&who, &cancel, &holding(&[Permission::ChangesDeploy]))
        .unwrap_err();
    assert!(
        over.message.contains("isn't being removed"),
        "cancelling the Deployment should have ended the removal: {over:?}"
    );
}

fn staging() -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse("staging").unwrap()),
    }
}

fn deploying_to(environments: &[&str]) -> Trusted {
    Trusted {
        environments: Some(
            environments
                .iter()
                .map(|name| EnvironmentName::parse(*name).unwrap())
                .collect(),
        ),
        ..holding(&[Permission::ChangesStage, Permission::ChangesDeploy])
    }
}

fn with_staging() -> (ConfigStore, Actor) {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateEnvironment {
                id: EnvironmentId::parse("00000000-0000-4000-8000-000000000004").unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    (store, who)
}

#[test]
fn a_token_for_one_environment_writes_it_and_is_refused_elsewhere_writing_nothing() {
    let (store, who) = with_staging();
    store
        .write_trusted(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: staging(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
            &deploying_to(&["staging"]),
        )
        .unwrap();
    let before = revision(&store, &who);
    let refused = store
        .write_trusted(&who, &set_replicas(2), &deploying_to(&["staging"]))
        .unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert_eq!(refused.message, "This token can deploy only to staging.");
    assert_eq!(refused.details, json!({ "environments": ["staging"] }));
    assert_eq!(revision(&store, &who), before);
    let refused = store
        .write_trusted(&who, &deploy_production(1), &deploying_to(&["staging"]))
        .unwrap_err();
    assert_eq!(refused.message, "This token can deploy only to staging.");
    assert_eq!(revision(&store, &who), before);
    store
        .write_trusted(
            &who,
            &set_replicas(2),
            &deploying_to(&["production", "staging"]),
        )
        .unwrap();
    assert_eq!(revision(&store, &who), before + 1);
}

#[test]
fn a_token_for_an_environment_cannot_write_what_belongs_to_none() {
    let (store, who) = with_staging();
    let refused = store
        .write_trusted(
            &who,
            &SetBuildOrder { build_order: None },
            &Trusted {
                environments: Some([EnvironmentName::parse("staging").unwrap()].into()),
                ..Trusted::default()
            },
        )
        .unwrap_err();
    assert_eq!(refused.message, "This token can deploy only to staging.");
}
