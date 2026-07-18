use std::collections::BTreeMap;
use std::path::Path;

use ployz::deploy::compose::{ComposeInput, UnsupportedFieldMode, parse_deploy_file};
use ployz_core::deploy::ContainerHealthcheckTest;
use ployz_test_support::ids::service_id;

const UMAMI_COMPOSE: &str =
    include_str!("../../../testing/ployz-e2e/tests/fixtures/v1-acceptance-umami.yaml");

fn parse(source: &str) -> ployz_core::deploy::DeployRequest {
    parse_with_env(source, BTreeMap::new())
}

fn parse_with_env(
    source: &str,
    interpolation_env: BTreeMap<String, String>,
) -> ployz_core::deploy::DeployRequest {
    let (parsed, warnings) = parse_deploy_file(ComposeInput {
        source,
        base_dir: Path::new("."),
        interpolation_env,
        namespace_override: None,
        mode: UnsupportedFieldMode::Strict,
    })
    .expect("acceptance Compose parses");
    assert!(warnings.is_empty());
    parsed
}

#[test]
fn umami_images_follow_dind_mirror_overrides() {
    let parsed = parse_with_env(
        UMAMI_COMPOSE,
        BTreeMap::from([
            (
                "PLOYZ_DIND_UMAMI_IMAGE".to_owned(),
                "mirror.example/umami:acceptance".to_owned(),
            ),
            (
                "PLOYZ_DIND_POSTGRES_IMAGE".to_owned(),
                "mirror.example/postgres:acceptance".to_owned(),
            ),
        ]),
    );
    let database = parsed
        .services
        .iter()
        .find(|service| service.service_id == service_id("db"))
        .expect("Postgres service exists");
    let umami = parsed
        .services
        .iter()
        .find(|service| service.service_id == service_id("umami"))
        .expect("Umami service exists");

    assert_eq!(
        database.image.as_str(),
        "mirror.example/postgres:acceptance"
    );
    assert_eq!(umami.image.as_str(), "mirror.example/umami:acceptance");
}

#[test]
fn umami_waits_for_database_dns_before_starting() {
    let parsed = parse(UMAMI_COMPOSE);
    assert_eq!(
        parsed.services.len(),
        2,
        "full acceptance fixture must contain exactly Postgres and Umami"
    );
    let database = parsed
        .services
        .iter()
        .find(|service| service.service_id == service_id("db"))
        .expect("Postgres service exists");
    let umami = parsed
        .services
        .iter()
        .find(|service| service.service_id == service_id("umami"))
        .expect("Umami service exists");

    assert_eq!(
        umami.runtime.command.as_ref().map(|command| command.as_slice()),
        Some(
            [
                "sh",
                "-c",
                "until node -e \"const net=require('net');const s=net.createConnection(5432,'db');s.setTimeout(2000);s.on('connect',()=>process.exit(0));s.on('error',()=>process.exit(1));s.on('timeout',()=>process.exit(1));\"; do sleep 1; done; exec npm run start-docker",
            ]
            .map(str::to_owned)
            .as_slice()
        )
    );

    let database_healthcheck = database
        .runtime
        .healthcheck
        .as_ref()
        .expect("Postgres healthcheck exists");
    assert!(matches!(
        &database_healthcheck.test,
        ContainerHealthcheckTest::Shell(command)
            if command.as_str() == "pg_isready -U umami -d umami"
    ));
    let umami_healthcheck = umami
        .runtime
        .healthcheck
        .as_ref()
        .expect("Umami healthcheck is explicitly disabled");
    assert!(matches!(
        &umami_healthcheck.test,
        ContainerHealthcheckTest::Disable
    ));
}
