//! How long lowering an Environment with 50 secret variables takes, on trunk and
//! on a branch that carries values sealed in the browser. Both cases are ignored;
//! run one with `--ignored` and compare the median it prints.
//!
//! `store_sealed` uses only Store APIs trunk has, so the file runs unchanged there:
//!
//! ```text
//! cargo nextest run -p ployz-store -E 'binary(lowering_perf)' --run-ignored all --no-capture
//! ```
//!
//! `browser_sealed` needs the `sealedVariables` flag table and runs on this branch only.

use ployz_core::ServiceName;
use ployz_store::{
    Actor, Change, ConfigStore, CreateProject, CreateService, Edit, EnvironmentId, EnvironmentRef,
    OrganizationId, ProjectId, ProjectName, RunnerId, ServiceLineageId, SettingPath,
};
use serde_json::{Value, json};

mod backend;

const ENVIRONMENT: &str = "00000000-0000-4000-8000-000000000002";
const SERVICE: &str = "00000000-0000-4000-8000-000000000003";
const VARIABLES: usize = 50;
const RUNS: u8 = 20;

fn who() -> Actor {
    Actor::system(OrganizationId::parse("org").unwrap())
}

/// Project `shop` with Service `web` holding `VARIABLES` secrets from `secret`.
fn shop(url: &str, secret: impl Fn(usize) -> Value) -> ConfigStore {
    let store = ConfigStore::open(url, backend::key()).unwrap();
    store
        .write(
            &who(),
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(ENVIRONMENT).unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who(),
            &CreateService {
                id: ServiceLineageId::parse(SERVICE).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    store
        .write(
            &who(),
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: (0..VARIABLES)
                    .map(|n| Change::Set {
                        path: SettingPath::parse(&format!("web.env.SECRET_{n}")).unwrap(),
                        value: json!({ "secret": secret(n) }),
                    })
                    .collect(),
            },
        )
        .unwrap();
    store
}

/// Admit, claim and finish `RUNS` Deploys; the median time a claim (the lowering a
/// runner waits for, with the key) and an admit (the plan, without it) took.
fn measure(store: &ConfigStore) {
    let runner = RunnerId::parse("runner").unwrap();
    let mut admits = Vec::new();
    let mut claims = Vec::new();
    for n in 1..=RUNS {
        let started = std::time::Instant::now();
        let id = backend::admit(store, &who(), "production", n);
        admits.push(started.elapsed());
        let started = std::time::Instant::now();
        let claimed = store.claim(&id, &runner).unwrap();
        claims.push(started.elapsed());
        backend::finish(store, &id, claimed);
    }
    println!(
        "{VARIABLES} secrets, {RUNS} runs: claim median {:?}, admit median {:?}",
        median(claims),
        median(admits)
    );
}

fn median(mut times: Vec<std::time::Duration>) -> std::time::Duration {
    times.sort();
    times.get(times.len() / 2).copied().unwrap_or_default()
}

#[test]
#[ignore = "timing: run with --run-ignored all --no-capture"]
fn store_sealed() {
    let dir = tempfile::tempdir().unwrap();
    let store = shop(&backend::fresh_url(&dir), |n| json!(format!("value-{n}")));
    measure(&store);
}

#[test]
#[ignore = "timing, this branch only: run with --run-ignored all --no-capture"]
fn browser_sealed() {
    use base64::Engine as _;
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    ConfigStore::open(&url, backend::key()).unwrap();
    backend::execute(
        &url,
        "INSERT INTO config_sealed_variables (organization_id) VALUES ('org')",
    );
    let store = shop(&url, |n| {
        let sealed = base64::engine::general_purpose::STANDARD.encode([n as u8; 48]);
        json!({
            "sealedFor": ENVIRONMENT,
            "value": { "sealed": sealed, "scope": { "service": SERVICE }, "key": format!("SECRET_{n}") },
        })
    });
    measure(&store);
}
