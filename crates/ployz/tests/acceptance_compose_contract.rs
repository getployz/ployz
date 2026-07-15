use std::collections::BTreeMap;
use std::path::Path;

use ployz::deploy::compose::{ComposeInput, UnsupportedFieldMode, parse_deploy_file};
use ployz_test_support::ids::{namespace_id, service_id};

const DATABASE_COMPOSE: &str =
    include_str!("../../ployz-e2e/tests/fixtures/v1-acceptance-database.yaml");
const UMAMI_COMPOSE: &str = include_str!("../../ployz-e2e/tests/fixtures/v1-acceptance-umami.yaml");

fn parse(source: &str) -> ployz::deploy::compose::ParsedComposeDeploy {
    let (parsed, warnings) = parse_deploy_file(ComposeInput {
        source,
        base_dir: Path::new("."),
        interpolation_env: BTreeMap::new(),
        namespace_override: None,
        mode: UnsupportedFieldMode::Strict,
    })
    .expect("acceptance Compose parses");
    assert!(warnings.is_empty());
    parsed
}

#[test]
fn database_deploy_is_a_full_single_service_namespace_revision() {
    let parsed = parse(DATABASE_COMPOSE);
    let [database] = parsed.services.as_slice() else {
        panic!("database Deploy must contain exactly one service");
    };

    assert_eq!(
        (&parsed.namespace_id, &database.service_id),
        (&namespace_id("v1_acceptance"), &service_id("db"))
    );
}

#[test]
fn umami_waits_for_database_dns_before_starting() {
    let parsed = parse(UMAMI_COMPOSE);
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
}
