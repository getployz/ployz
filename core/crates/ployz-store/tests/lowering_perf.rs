//! How long lowering an Environment with 50 secret variables sealed on the client
//! takes. Ignored; run it with `--ignored` at trunk and at head and compare the
//! median it prints. Trunk's copy also times Store-sealed values, which head refuses.
//!
//! ```text
//! cargo nextest run -p ployz-store -E 'binary(lowering_perf)' --run-ignored all --no-capture
//! ```

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
fn client_sealed() {
    use base64::Engine as _;
    let dir = tempfile::tempdir().unwrap();
    let store = shop(&backend::fresh_url(&dir), |n| {
        let sealed = base64::engine::general_purpose::STANDARD.encode([n as u8; 48]);
        json!({
            "sealedFor": ENVIRONMENT,
            "value": { "sealed": sealed, "scope": { "service": SERVICE }, "key": format!("SECRET_{n}") },
        })
    });
    measure(&store);
}
