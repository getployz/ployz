//! Region names, on SQLite and on Postgres (see `backend`): naming a region changes what it
//! shows as and nothing a Deploy reads, and an unnamed region shows its built-in name.

use std::collections::BTreeSet;

use ployz_core::ServiceName;
use ployz_store::{
    Actor, ConfigStore, CreateEnvironment, CreateProject, CreateService, DiffQuery, EnvironmentId,
    EnvironmentName, EnvironmentQuery, EnvironmentRef, NameRegion, NewRegionQuery, OrganizationId,
    Permission, PlanQuery, ProjectId, ProjectName, Publish, Region, RegionKey, RegionName,
    RegionsQuery, ServiceLineageId, Trusted,
};

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

fn rename(key: &str, name: &str) -> NameRegion {
    NameRegion {
        key: RegionKey::parse(key).unwrap(),
        name: RegionName::parse(name).unwrap(),
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
            key: RegionKey::default_region(),
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

const KEY_RULE: &str = "A region is the value of a Server's region label: letters, digits, hyphens, underscores and dots, for example fsn.";

#[test]
fn a_key_is_what_placement_reads_in_a_region_label() {
    for key in ["fsn", "us-east-1", "eu_west.1", "FSN_1"] {
        assert_eq!(
            RegionKey::parse(key).unwrap().as_str(),
            key.to_ascii_lowercase()
        );
    }
    for key in ["", "a b", "fsn/1", "é", "🦀"] {
        assert_eq!(RegionKey::parse(key).unwrap_err().message, KEY_RULE);
    }
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
