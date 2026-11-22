//! A deploy refused before admission leaves no `--image` staged.

use super::*;
use ployz_store::{
    Actor, CreateProject, CreateService, EnvironmentId, OrganizationId, ProjectId, ProjectName,
    SealingKey, ServiceLineageId,
};
use std::sync::Arc;

/// A staged image left behind refuses every later silent deploy, as one whose saved image
/// isn't in its last approved version.
#[test]
fn a_deploy_refused_before_admission_sets_its_image_back() {
    let local =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap());
    let owner = Actor::system(OrganizationId::parse("owner").unwrap());
    local
        .write(
            &owner,
            &CreateProject {
                id: ProjectId::parse(mint()).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(mint()).unwrap(),
                region: None,
            },
        )
        .unwrap();
    let web = ServiceName::parse("web").unwrap();
    local
        .write(
            &owner,
            &CreateService {
                id: ServiceLineageId::parse(mint()).unwrap(),
                environment: EnvironmentRef::default(),
                name: web.clone(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    let config = std::env::temp_dir().join(format!("ployz-stage-{}", mint()));
    let image = format!("web=nginx@sha256:{}", "a".repeat(64));
    let matches = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "--ployz-config",
            config.join("config.toml").to_str().unwrap(),
            "deploy",
            "web",
            "--image",
            &image,
            // With no device key to sign, `--forget-older` refuses before admission.
            "--forget-older",
        ])
        .unwrap();
    let matches = leaf_matches(&matches);
    let store = Store::for_test(
        super::super::store::Backend::Local(Arc::clone(&local), owner.clone()),
        matches,
    );
    let request = Request {
        environment: EnvironmentRef::default(),
        services: vec![web],
        version: None,
        source: None,
        accept: Vec::new(),
        message: None,
        no_prune: false,
    };

    assert!(upload_and_ship(matches, &store, request, None).is_err());
    let working = local
        .read(
            &owner,
            &EnvironmentQuery {
                environment: EnvironmentRef::default(),
                path: Some(SettingPath::parse("web.image").unwrap()),
                all: false,
            },
        )
        .unwrap();
    assert_eq!(
        working
            .settings
            .iter()
            .map(|row| &row.value)
            .collect::<Vec<_>>(),
        [&serde_json::json!("nginx:1")]
    );
}
