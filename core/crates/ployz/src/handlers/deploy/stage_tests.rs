//! A deploy refused before admission sets back an `--image` it staged over another image,
//! and never empties a Service that had no image.

use super::*;
use ployz_store::{
    Actor, CreateProject, CreateService, EnvironmentId, OrganizationId, ProjectId, ProjectName,
    SealingKey, ServiceLineageId,
};
use std::sync::Arc;

const IMAGE: &str = "nginx@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// What `web.image` holds once `deploy web --image web=IMAGE --forget-older` is refused
/// before admission, for a `web` created with `image`.
fn staged_after_refusal(image: Option<&str>) -> Vec<serde_json::Value> {
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
                image: image.map(str::to_owned),
                template: None,
            },
        )
        .unwrap();
    let config = std::env::temp_dir().join(format!("ployz-stage-{}", mint()));
    let typed = format!("web={IMAGE}");
    let matches = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "--ployz-config",
            config.join("config.toml").to_str().unwrap(),
            "deploy",
            "web",
            "--image",
            &typed,
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
    local
        .read(
            &owner,
            &EnvironmentQuery {
                environment: EnvironmentRef::default(),
                path: Some(SettingPath::parse("web.image").unwrap()),
                all: false,
            },
        )
        .unwrap()
        .settings
        .into_iter()
        .map(|row| row.value)
        .collect()
}

/// A staged image left behind refuses every later silent deploy, as one whose saved image
/// isn't in its last approved version.
#[test]
fn a_deploy_refused_before_admission_sets_its_image_back() {
    assert_eq!(
        staged_after_refusal(Some("nginx:1")),
        [serde_json::json!("nginx:1")]
    );
}

/// An image set on a Service with none replaced its source and build settings; unsetting
/// it would leave the Service with no source at all.
#[test]
fn a_refused_deploy_leaves_its_image_on_a_service_that_had_none() {
    assert_eq!(staged_after_refusal(None), [serde_json::json!(IMAGE)]);
}
