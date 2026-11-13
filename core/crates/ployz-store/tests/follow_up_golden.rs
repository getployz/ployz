#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! The follow-up every refusal carries in `details.next`, as the CLI and the dashboard
//! read it: the exact command line, or the `{tool, input}` object, of each Store
//! producer. These literals were captured from the Store before follow-ups were typed.

use ployz_core::ServiceName;
use ployz_store::{
    Actor, Change, ConfigStore, CreateProject, CreateService, Edit, EnvironmentId, EnvironmentName,
    EnvironmentQuery, EnvironmentRef, KeepBranch, NumberedDeploymentQuery, OrganizationId,
    ProjectId, ProjectName, Remove, ServiceLineageId, SetGeneratedDomain, SettingPath, Trusted,
};
use serde_json::{Value, json};

mod backend;

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

fn who() -> Actor {
    Actor::system(OrganizationId::parse("org").unwrap())
}

/// Project `shop` with Service `web`.
fn shop() -> (ConfigStore, Actor) {
    let (store, who) = (backend::open(), who());
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
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(3)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("web:1".into()),
                template: None,
            },
        )
        .unwrap();
    (store, who)
}

fn set(path: &str, value: Value) -> Edit {
    Edit {
        environment: at("production"),
        expect: None,
        changes: vec![Change::Set {
            path: SettingPath::parse(path).unwrap(),
            value,
        }],
    }
}

#[test]
fn an_organization_without_a_project_says_to_make_one() {
    let (store, who) = (backend::open(), who());
    let refused = store
        .read(
            &who,
            &EnvironmentQuery {
                environment: EnvironmentRef::default(),
                path: None,
                all: false,
            },
        )
        .unwrap_err();
    assert_eq!(refused.details["next"], "ployz project new NAME");
}

#[test]
fn removing_an_unknown_project_says_to_list_them() {
    let (store, who) = shop();
    let refused = store
        .write(
            &who,
            &Remove {
                project: ProjectName::parse("nosuch").unwrap(),
                environment: None,
                version: None,
                accept_volume_loss: Default::default(),
                every_copy: Default::default(),
                teardown: None,
            },
        )
        .unwrap_err();
    assert_eq!(refused.details["next"], "ployz project ls");
}

#[test]
fn a_node_that_is_no_branch_says_how_to_make_one() {
    let (store, who) = shop();
    let refused = store
        .write(
            &who,
            &KeepBranch {
                environment: at("production"),
                kept: true,
            },
        )
        .unwrap_err();
    assert_eq!(
        refused.details["next"],
        "ployz env branch NAME --from production --project shop"
    );
}

#[test]
fn a_missing_deployment_says_how_to_list_or_deploy() {
    let (store, who) = shop();
    let numbered = store
        .read(
            &who,
            &NumberedDeploymentQuery {
                environment: at("production"),
                number: Some(3),
            },
        )
        .unwrap_err();
    assert_eq!(
        numbered.details["next"],
        "ployz deployment ls --project shop --env production"
    );
    let latest = store
        .read(
            &who,
            &NumberedDeploymentQuery {
                environment: at("production"),
                number: None,
            },
        )
        .unwrap_err();
    assert_eq!(latest.details["next"], "ployz deploy");
}

#[test]
fn domains_are_edited_with_their_own_command() {
    let (store, who) = shop();
    let refused = store
        .write(&who, &set("web.domains", json!(["app.example.com"])))
        .unwrap_err();
    assert_eq!(refused.details["next"], "ployz domain add web");

    let none = store
        .write_trusted(
            &who,
            &SetGeneratedDomain {
                environment: at("production"),
                service: ServiceName::parse("web").unwrap(),
                domain: None,
                prefix: "fresh".parse().unwrap(),
                port: Default::default(),
            },
            &Trusted::default(),
        )
        .unwrap_err();
    assert_eq!(none.details["next"], "ployz domain add web");
}

#[test]
fn healthcheck_text_that_is_no_path_says_how_to_patch_it() {
    let (store, who) = shop();
    let refused = store
        .write(&who, &set("web.healthcheck", json!("pg_isready")))
        .unwrap_err();
    assert_eq!(
        refused.details["next"],
        "ployz set web --patch '{\"healthcheck\":{\"command\":\"pg_isready\"}}' --env production --project shop"
    );
}
