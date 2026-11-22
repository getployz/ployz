//! Region names, on SQLite and on Postgres (see `backend`): naming a region changes what it
//! shows as and nothing a Deploy reads, and an unnamed region shows its built-in name.

use std::collections::BTreeSet;

use ployz_core::ServiceName;
use ployz_store::{
    Actor, ConfigStore, CreateEnvironment, CreateProject, CreateService, DiffQuery, EnvironmentId,
    EnvironmentName, EnvironmentQuery, EnvironmentRef, NameRegion, NewRegionQuery, OrganizationId,
    Permission, PlanQuery, ProjectId, ProjectName, Publish, Region, RegionName, RegionsQuery,
    ServiceLineageId, Trusted, region_key,
};

use ployz_store::testing::backend;

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
                region: None,
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
    store
        .write(
            &who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
        )
        .unwrap();
    (store, who)
}

/// Naming the region `key`, which a Server's label holds.
fn rename(key: &str, name: &str) -> NameRegion {
    NameRegion {
        key: region_key(key).unwrap(),
        name: RegionName::parse(name).unwrap(),
        labels: vec![key.to_owned()],
        creates: false,
    }
}

/// Naming a region an enrollment makes, with `labels` the Servers' regions so far.
fn creates(key: &str, name: &str, labels: &[&str]) -> NameRegion {
    NameRegion {
        creates: true,
        labels: labels.iter().map(|label| (*label).to_owned()).collect(),
        ..rename(key, name)
    }
}

/// What a Deploy of the Default Environment reads: its Settings, its diff and its plan.
fn deployable(store: &ConfigStore, who: &Actor) -> String {
    let environment = EnvironmentRef::default();
    let settings = store
        .read(
            who,
            &EnvironmentQuery {
                environment: environment.clone(),
                path: None,
                all: true,
            },
        )
        .unwrap();
    let diff = store
        .read(
            who,
            &DiffQuery {
                environment: environment.clone(),
            },
        )
        .unwrap();
    let plan = store
        .read(
            who,
            &PlanQuery {
                environment,
                ..PlanQuery::default()
            },
        )
        .unwrap();
    serde_json::to_string(&(settings, diff, plan)).unwrap()
}

fn shown(store: &ConfigStore, who: &Actor, labels: &[&str]) -> Vec<(String, String)> {
    store
        .read(
            who,
            &RegionsQuery {
                labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            },
        )
        .unwrap()
        .regions
        .into_iter()
        .map(|region| (region.key.to_string(), region.name))
        .collect()
}

#[test]
fn renaming_a_region_changes_nothing_a_deploy_reads() {
    let (store, who) = shop();
    let before = deployable(&store, &who);

    let named = store
        .write(&who, &rename("default", "Falkenstein"))
        .unwrap();
    assert_eq!(
        named,
        Region {
            key: ployz_core::Region::default(),
            name: "Falkenstein".into()
        }
    );
    assert_eq!(deployable(&store, &who), before);
    assert_eq!(
        shown(&store, &who, &["default"]),
        [("default".into(), "Falkenstein".into())]
    );
}

#[test]
fn naming_a_region_again_converges_on_the_last_name() {
    let (store, who) = shop();
    store.write(&who, &rename("syd", "Sydney")).unwrap();
    store.write(&who, &rename("syd", "Sydney")).unwrap();
    assert_eq!(shown(&store, &who, &[]), [("syd".into(), "Sydney".into())]);
    store.write(&who, &rename("SYD", "Sydney South")).unwrap();
    assert_eq!(
        shown(&store, &who, &["Syd"]),
        [("syd".into(), "Sydney South".into())]
    );
}

#[test]
fn a_region_with_no_name_shows_its_built_in_one() {
    let (store, who) = shop();
    assert_eq!(
        shown(&store, &who, &["default", "FSN", "eu_west", "not a region"]),
        [
            ("default".into(), "Default".into()),
            ("eu_west".into(), "eu_west".into()),
            ("fsn".into(), "fsn".into())
        ]
    );
}

#[test]
fn region_names_belong_to_their_organization() {
    let (store, who) = shop();
    store
        .write(&who, &rename("default", "Falkenstein"))
        .unwrap();
    let other = Actor::system(OrganizationId::parse("other").unwrap());
    assert_eq!(
        shown(&store, &other, &["default"]),
        [("default".into(), "Default".into())]
    );
}

#[test]
fn naming_a_region_needs_servers_operate() {
    let (store, _) = shop();
    let member = Actor {
        organization: OrganizationId::parse("org").unwrap(),
        principal: Some(ployz_store::Principal::parse("ada").unwrap()),
    };
    let without = Trusted {
        permissions: Some(
            Permission::ALL
                .iter()
                .copied()
                .filter(|permission| *permission != Permission::ServersOperate)
                .collect::<BTreeSet<_>>(),
        ),
        ..Trusted::default()
    };
    let refused = store
        .write_trusted(&member, &rename("default", "Falkenstein"), &without)
        .unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert_eq!(
        shown(&store, &member, &["default"]),
        [("default".into(), "Default".into())]
    );

    let with = Trusted {
        permissions: Some(BTreeSet::from([Permission::ServersOperate])),
        ..Trusted::default()
    };
    store
        .write_trusted(&member, &rename("default", "Falkenstein"), &with)
        .unwrap();
    assert_eq!(
        shown(&store, &member, &["default"]),
        [("default".into(), "Falkenstein".into())]
    );
}

fn new_region(
    store: &ConfigStore,
    who: &Actor,
    name: &str,
    labels: &[&str],
) -> Result<String, String> {
    store
        .read(
            who,
            &NewRegionQuery {
                name: RegionName::parse(name).unwrap(),
                labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            },
        )
        .map(|view| view.key.to_string())
        .map_err(|refused| refused.message)
}

#[test]
fn a_new_region_takes_a_key_from_its_name_unless_a_region_has_it_in_any_case() {
    let (store, who) = shop();
    store.write(&who, &rename("lhr", "London")).unwrap();

    assert_eq!(
        new_region(&store, &who, "US East (Ohio)", &["fsn"]),
        Ok("us-east-ohio".into())
    );
    assert_eq!(
        new_region(&store, &who, "fsn", &["FSN"]),
        Err("Region fsn already exists.".into())
    );
    assert_eq!(
        new_region(&store, &who, "LHR", &[]),
        Err("Region London (lhr) already exists.".into())
    );
    assert_eq!(
        new_region(&store, &who, "london", &[]),
        Err("Region London (lhr) already exists.".into())
    );
    assert_eq!(
        new_region(&store, &who, "東京", &[]),
        Err("Use at least one letter a-z or digit.".into())
    );
}

const KEY_RULE: &str = "A region is the value of a Server's region label: up to 63 letters, digits, hyphens, underscores and dots, for example fsn.";

#[test]
fn a_key_is_what_placement_reads_in_a_region_label() {
    for key in ["fsn", "us-east-1", "eu_west.1", "FSN_1"] {
        assert_eq!(region_key(key).unwrap().as_str(), key.to_ascii_lowercase());
    }
    assert!(region_key("a".repeat(63)).is_ok());
    for key in ["", "a b", "fsn/1", "é", "🦀", &"a".repeat(64)] {
        assert_eq!(region_key(key).unwrap_err().message, KEY_RULE);
    }
}

#[test]
fn renaming_a_region_no_server_or_name_holds_is_refused_and_makes_no_region() {
    let (store, who) = shop();
    let refused = store
        .write(
            &who,
            &NameRegion {
                labels: vec!["fsn".into()],
                ..rename("nosuch", "Foo")
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, ployz_core::RpcErrorCode::NotFound);
    assert_eq!(refused.message, "No region nosuch.");
    assert_eq!(
        shown(&store, &who, &["fsn"]),
        [("fsn".into(), "fsn".into())]
    );
    store
        .write(
            &who,
            &NameRegion {
                labels: vec![],
                ..rename("default", "Falkenstein")
            },
        )
        .unwrap();
}

#[test]
fn renaming_a_region_to_another_regions_name_is_refused_in_any_case() {
    let (store, who) = shop();
    store.write(&who, &rename("syd", "Sydney")).unwrap();
    let both = |key: &str, name: &str| NameRegion {
        labels: vec!["syd".into(), "default".into()],
        ..rename(key, name)
    };
    for name in ["Sydney", "sydney", "SYDNEY"] {
        let refused = store.write(&who, &both("default", name)).unwrap_err();
        assert_eq!(
            refused.message, "Region Sydney (syd) already exists.",
            "{name}"
        );
    }
    assert_eq!(
        shown(&store, &who, &["syd", "default"]),
        [
            ("default".into(), "Default".into()),
            ("syd".into(), "Sydney".into())
        ]
    );
    store.write(&who, &both("syd", "SYDNEY")).unwrap();
    assert_eq!(
        shown(&store, &who, &["syd"]),
        [("syd".into(), "SYDNEY".into())]
    );
}

#[test]
fn two_new_regions_with_one_key_cannot_both_be_made() {
    let (store, who) = shop();
    store
        .write(&who, &creates("us-east", "US East", &[]))
        .unwrap();
    for (key, name) in [("us-east", "US-East"), ("us-east", "us east")] {
        let refused = store
            .write(&who, &creates(key, name, &["us-east"]))
            .unwrap_err();
        assert_eq!(
            refused.message, "Region US East (us-east) already exists.",
            "{name}"
        );
    }
    store
        .write(&who, &creates("us-east", "US East", &["us-east"]))
        .unwrap();
    assert_eq!(
        shown(&store, &who, &[]),
        [("us-east".into(), "US East".into())]
    );
    let wrong = store
        .write(&who, &creates("elsewhere", "Sydney", &[]))
        .unwrap_err();
    assert_eq!(wrong.message, "A region named Sydney has another key.");
}

#[test]
fn a_new_environment_runs_where_it_was_created_and_an_older_one_in_any_region() {
    let (store, who) = shop();
    let create = |n: u8, name: &str, region: Option<&str>| {
        store
            .write(
                &who,
                &CreateEnvironment {
                    id: EnvironmentId::parse(format!("00000000-0000-4000-8000-0000000000{n:02}"))
                        .unwrap(),
                    project: None,
                    name: EnvironmentName::parse(name).unwrap(),
                    region: region.map(|region| ployz_core::Region::parse(region).unwrap()),
                },
            )
            .unwrap();
    };
    let region_of = |name: &str| {
        store
            .read(
                &who,
                &EnvironmentQuery {
                    environment: EnvironmentRef {
                        project: None,
                        environment: Some(EnvironmentName::parse(name).unwrap()),
                    },
                    path: None,
                    all: false,
                },
            )
            .unwrap()
            .region
            .map(|region| region.to_string())
    };
    create(10, "staging", Some("Default"));
    create(11, "qa", None);

    assert_eq!(region_of("staging"), Some("default".to_owned()));
    assert_eq!(region_of("qa"), None);
    assert_eq!(region_of("production"), None);
}

#[test]
fn a_projects_default_environment_runs_where_the_project_was_created() {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    let created = store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
                region: Some(ployz_core::Region::parse("Default").unwrap()),
            },
        )
        .unwrap();
    let region = store
        .read(
            &who,
            &EnvironmentQuery {
                environment: EnvironmentRef::default(),
                path: None,
                all: false,
            },
        )
        .unwrap()
        .region;
    assert_eq!(created.environment.name.as_str(), "production");
    assert_eq!(
        region.map(|region| region.to_string()),
        Some("default".to_owned())
    );
}
