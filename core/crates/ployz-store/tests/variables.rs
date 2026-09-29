//! Variables and sealed secrets through the Store's interface only, on SQLite and on
//! Postgres (see `backend`): what reads show, what edits accept, and that plaintext
//! leaves the Store only through `claim`.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, Change, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
    DeploymentsQuery, DiffQuery, Edit, Edited, EnvironmentId, EnvironmentQuery, EnvironmentRef,
    OrganizationId, PlanQuery, ProjectId, ProjectName, RunnerId, SealingKey, ServiceId,
    SettingPath,
};
use serde_json::{Value, json};

mod backend;

const SECRET: &str = "s3cr3t-pässwörd 🔑";

fn who() -> Actor {
    Actor::system(OrganizationId::parse("org").unwrap())
}

/// Project `shop` with Services `web` and `api` in the Store at `store`.
fn shop(store: &ConfigStore) {
    store
        .create_project(
            &who(),
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    for (n, name) in [(3, "web"), (4, "api")] {
        store
            .create_service(
                &who(),
                &CreateService {
                    id: ServiceId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some("nginx:1".into()),
                },
            )
            .unwrap();
    }
}

fn set(store: &ConfigStore, changes: &[(&str, Value)]) -> Result<Edited, RpcError> {
    store.edit(
        &who(),
        &Edit {
            environment: EnvironmentRef::default(),
            expect: None,
            changes: changes
                .iter()
                .map(|(path, value)| Change::Set {
                    path: SettingPath::parse(path).unwrap(),
                    value: value.clone(),
                })
                .collect(),
        },
    )
}

fn unset(store: &ConfigStore, path: &str) -> Edited {
    store
        .edit(
            &who(),
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse(path).unwrap(),
                }],
            },
        )
        .unwrap()
}

fn get(store: &ConfigStore, path: Option<&str>) -> ployz_store::EnvironmentView {
    store
        .environment(
            &who(),
            &EnvironmentQuery {
                path: path.map(|path| SettingPath::parse(path).unwrap()),
                ..EnvironmentQuery::default()
            },
        )
        .unwrap()
}

fn value(store: &ConfigStore, path: &str) -> Value {
    get(store, Some(path)).settings.remove(0).value
}

fn staged(edited: &Edited) -> Vec<String> {
    edited.staged.iter().map(ToString::to_string).collect()
}

fn admit(store: &ConfigStore, n: u8) -> Result<DeploymentId, RpcError> {
    let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap();
    store.admit(
        &who(),
        &Admit::Deploy(Deploy {
            id: id.clone(),
            environment: EnvironmentRef::default(),
            services: Vec::new(),
            version: None,
            upload: None,
            accept_volume_loss: Vec::new(),
        }),
        &ployz_store::Trusted::default(),
    )?;
    Ok(id)
}

fn environment_of(claimed: &ployz_store::Claimed, service: &str) -> Value {
    let spec = claimed
        .intent
        .target
        .iter()
        .find(|spec| spec.name.as_str() == service)
        .unwrap();
    json!(spec.container.environment)
}

#[test]
fn secrets_never_leave_reads_and_only_claim_unseals_them() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    let edited = set(
        &store,
        &[
            ("web.env.password", json!({ "secret": SECRET })),
            ("web.env.DB_HOST", json!("db")),
            (
                "api.env.DATABASE_URL",
                json!("postgres://${{ web.PASSWORD }}@${{ web.DB_HOST }}/app"),
            ),
            ("api.env.WEB", json!("${{ web.PLOYZ_PRIVATE_DOMAIN }}")),
        ],
    )
    .unwrap();
    assert_eq!(
        staged(&edited),
        [
            "web.env.PASSWORD",
            "web.env.DB_HOST",
            "api.env.DATABASE_URL",
            "api.env.WEB"
        ]
    );
    assert_eq!(value(&store, "web.env.PASSWORD"), json!({ "secret": true }));
    assert_eq!(
        value(&store, "api.env.DATABASE_URL"),
        "postgres://${{ web.PASSWORD }}@${{ web.DB_HOST }}/app"
    );
    let diff = store.diff(&who(), &DiffQuery::default()).unwrap();
    let rows = diff
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .map(|row| (row.path.as_str(), row.after.clone()))
        .collect::<Vec<_>>();
    assert!(rows.contains(&("web.env.PASSWORD", json!({ "secret": true }))));
    assert!(rows.contains(&("web.env.DB_HOST", json!("db"))));
    let plan = store.plan(&who(), &PlanQuery::default()).unwrap();

    let id = admit(&store, 1).unwrap();
    let reads = [
        json!(edited),
        json!(get(&store, None)),
        json!(get(&store, Some("web"))),
        json!(diff),
        json!(plan),
        json!(store.deployment(&who(), &id).unwrap()),
        json!(
            store
                .deployments(&who(), &DeploymentsQuery::default())
                .unwrap()
        ),
    ];
    for read in reads {
        let text = read.to_string();
        assert!(
            !text.contains("s3cr3t") && !text.contains("ciphertext"),
            "{text}"
        );
    }
    if let Some(path) = url.strip_prefix("sqlite:") {
        drop(store);
        let stored = std::fs::read(path).unwrap();
        assert!(!String::from_utf8_lossy(&stored).contains("s3cr3t"));
        let store = ConfigStore::open(&url, backend::key()).unwrap();
        return claim_unseals(&store, &id);
    }
    claim_unseals(&store, &id);
}

fn claim_unseals(store: &ConfigStore, id: &DeploymentId) {
    let claimed = store
        .claim(id, &RunnerId::parse("runner").unwrap())
        .unwrap();
    assert_eq!(environment_of(&claimed, "web")["PASSWORD"], SECRET);
    let api = environment_of(&claimed, "api");
    assert_eq!(api["DATABASE_URL"], format!("postgres://{SECRET}@db/app"));
    assert_eq!(api["WEB"], "web.internal");
    // A Service waits for the Services its variables reference.
    let api = ServiceName::parse("api").unwrap();
    assert_eq!(
        claimed.intent.dependencies()[&api][0].service.as_str(),
        "web"
    );
}

#[test]
fn a_store_with_another_key_cannot_unseal() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    set(&store, &[("web.env.TOKEN", json!({ "secret": SECRET }))]).unwrap();
    let id = admit(&store, 1).unwrap();
    let other = ConfigStore::open(&url, SealingKey::new(b"another secret").unwrap()).unwrap();
    let error = other
        .claim(&id, &RunnerId::parse("runner").unwrap())
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Internal);
    assert!(!error.message.contains("s3cr3t"));
}

#[test]
fn a_secret_is_kept_by_its_marker_and_never_becomes_plain() {
    let store = backend::open();
    shop(&store);
    set(&store, &[("web.env.TOKEN", json!({ "secret": SECRET }))]).unwrap();
    // Sending back what reads show keeps it, as does sealing the same value again.
    for same in [json!({ "secret": true }), json!({ "secret": SECRET })] {
        assert!(staged(&set(&store, &[("web.env.TOKEN", same)]).unwrap()).is_empty());
    }
    let refused = set(&store, &[("web.env.TOKEN", json!("plain now"))]).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert!(refused.message.contains("--secret"));
    assert!(!refused.to_string().contains("plain now"));
    // Nothing to keep on a plain or new variable.
    set(&store, &[("web.env.PLAIN", json!("x"))]).unwrap();
    for path in ["web.env.PLAIN", "web.env.NEW"] {
        let error = set(&store, &[(path, json!({ "secret": true }))]).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    }
    // A plain variable may become secret; a new secret replaces the old.
    let edited = set(
        &store,
        &[
            ("web.env.PLAIN", json!({ "secret": "now sealed" })),
            ("web.env.TOKEN", json!({ "secret": "rotated" })),
        ],
    )
    .unwrap();
    assert_eq!(staged(&edited), ["web.env.PLAIN", "web.env.TOKEN"]);
    for bad in [
        json!(null),
        json!(3),
        json!({ "secret": "" }),
        json!({ "secret": false }),
    ] {
        let error = set(&store, &[("web.env.TOKEN", bad)]).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    }
    assert_eq!(staged(&unset(&store, "web.env.TOKEN")), ["web.env.TOKEN"]);
    assert!(staged(&unset(&store, "web.env.TOKEN")).is_empty());
    let missing = store
        .environment(
            &who(),
            &EnvironmentQuery {
                path: Some(SettingPath::parse("web.env.PLAN").unwrap()),
                ..EnvironmentQuery::default()
            },
        )
        .unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::NotFound);
    assert_eq!(missing.details["did_you_mean"], "PLAIN");
}

#[test]
fn references_and_exports_round_trip_through_get_and_patch() {
    let store = backend::open();
    shop(&store);
    let error = set(&store, &[("api.env.URL", json!("${{ wbe.HOST }}"))]).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(error.details["did_you_mean"], "web");
    set(
        &store,
        &[
            ("web.env.HOST", json!("web.internal")),
            ("web.env.HOST.exported", json!("true")),
            ("web.env.KEY", json!({ "secret": SECRET })),
            (
                "api.env.URL",
                json!("http://${{ web.HOST }}/ $${{ not.A_REF }}"),
            ),
        ],
    )
    .unwrap();
    let web = get(&store, Some("web"));
    let values = web.values.clone().unwrap();
    assert_eq!(
        values["env"],
        json!({
            "HOST": { "value": "web.internal", "exported": true },
            "KEY": { "secret": true },
        })
    );
    assert_eq!(value(&store, "web.env.HOST.exported"), true);
    assert_eq!(
        value(&store, "api.env.URL"),
        "http://${{ web.HOST }}/ $${{ not.A_REF }}"
    );
    // get SERVICE → set SERVICE --patch → get changes nothing.
    let revision = web.environment.revision;
    let patched = store
        .edit(
            &who(),
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Patch {
                    path: SettingPath::parse("web").unwrap(),
                    value: Value::Object(values),
                }],
            },
        )
        .unwrap();
    assert!(patched.staged.is_empty());
    assert_eq!(patched.environment.revision, revision);
    assert_eq!(get(&store, Some("web")), web);
    assert_eq!(
        staged(&unset(&store, "web.env.HOST.exported")),
        ["web.env.HOST.exported"]
    );
    // A cycle can't deploy, and names the variables in it.
    set(
        &store,
        &[
            ("web.env.A", json!("${{ api.B }}")),
            ("api.env.B", json!("${{ web.A }}")),
        ],
    )
    .unwrap();
    let error = admit(&store, 1).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(error.message.contains("web.env.A"), "{}", error.message);
}
