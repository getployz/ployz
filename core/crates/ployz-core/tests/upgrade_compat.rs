use std::path::Path;

use ployz_core::config::lower_deployment;
use ployz_core::{ResolvedServiceSpec, SpecChange, compare_specs};
use serde::Deserialize;

/// One release's stored spec of a Postgres Service Container, and the Cloud config it
/// was deployed from. A release adds one file to `fixtures/stored_db_spec`.
#[derive(Deserialize)]
struct StoredRelease {
    deployment: serde_json::Value,
    stored: ResolvedServiceSpec,
}

/// A daemon upgraded from any release keeps its Containers through the first Deploy of
/// unchanged config: what that release stored, as the daemon adopts it at sync, plans
/// UpToDate against today's lowering.
#[test]
fn a_spec_stored_by_any_release_is_up_to_date_against_todays_lowering() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stored_db_spec");
    let mut releases = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    releases.sort();
    assert!(releases.len() >= 2, "fixtures in {}", directory.display());

    for release in releases {
        let StoredRelease {
            deployment,
            mut stored,
        } = serde_json::from_str(&std::fs::read_to_string(&release).unwrap()).unwrap();
        stored.adopt_unprobed();
        let intent = lower_deployment(serde_json::from_value(deployment).unwrap()).unwrap();
        let mut requested = intent
            .target
            .into_iter()
            .find(|spec| spec.name == stored.name)
            .unwrap();
        requested.mount_graph = requested
            .mount_graph
            .scope_to_namespace(&intent.namespace)
            .unwrap();
        assert_eq!(
            compare_specs(&stored, &requested),
            SpecChange::UpToDate,
            "{}: stored healthcheck {:?} vs requested {:?}",
            release.display(),
            stored.container.healthcheck,
            requested.container.healthcheck
        );
    }
}
