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
    OrganizationId, PlanQuery, ProjectId, ProjectName, RunnerId, SealingKey, ServiceLineageId,
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
        .write(
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
            .write(
                &who(),
                &CreateService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some("nginx:1".into()),
                    template: None,
                },
            )
            .unwrap();
    }
}

fn set(store: &ConfigStore, changes: &[(&str, Value)]) -> Result<Edited, RpcError> {
    store.write(
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
        .write(
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
        .read(
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
    store.write_trusted(
        &who(),
        &Admit::Deploy(Deploy {
            id: id.clone(),
            environment: EnvironmentRef::default(),
            services: Vec::new(),
            version: None,
            upload: None,
            accept_volume_loss: Vec::new(),
            message: None,
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
fn null_environment_values_are_refused_before_any_setting_changes() {
    let store = backend::open();
    shop(&store);
    set(&store, &[("web.env.VALID", json!("café\nbeta"))]).unwrap();
    assert_eq!(value(&store, "web.env.VALID"), "café\nbeta");
    let before = get(&store, None);
    for value in [
        json!("private\0value"),
        json!({ "secret": "private\0value" }),
        json!({ "value": "private\0value", "exported": true }),
    ] {
        let error = store
            .write(
                &who(),
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Patch {
                        path: SettingPath::parse("web").unwrap(),
                        value: json!({ "cpuLimit": 0.5, "env": { "BAD": value } }),
                    }],
                },
            )
            .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        assert!(error.message.contains("null characters"));
        assert!(!error.message.contains("private"));
        assert_eq!(get(&store, None), before);
    }
}

#[test]
fn a_typed_private_address_answers_with_its_reference() {
    let store = backend::open();
    shop(&store);
    let reference = |value: &str| json!({ "kind": "reference", "value": value });
    let edited = set(
        &store,
        &[
            ("web.env.API_URL", json!("http://api.internal:8080/v1")),
            // A bare name where only a host can stand: a URL's host, a host key's value.
            ("web.env.API_DB", json!("postgres://app:pw@API:5432/app")),
            ("web.env.API_HOST", json!("api")),
            ("web.env.API_ADDR", json!("api:8080")),
            // A reference in the user info leaves the host a URL's host, not the user.
            ("web.env.DB_PASSWORD", json!({ "secret": SECRET })),
            (
                "web.env.API_LOGIN",
                json!("postgres://api:${{ DB_PASSWORD }}@api:5432/app"),
            ),
            // Elsewhere a bare name is just a word, or an image.
            ("web.env.PROCESS_TYPE", json!("api")),
            ("web.env.IMAGE", json!("api:16")),
            (
                "web.env.API_SECRET",
                json!({ "secret": format!("postgres://app:{SECRET}@api.internal:5432/app") }),
            ),
        ],
    )
    .unwrap();
    let answered = serde_json::to_string(&edited.typed_addresses).unwrap();
    assert!(!answered.contains(SECRET));
    assert_eq!(
        serde_json::to_value(&edited.typed_addresses).unwrap(),
        json!([
            {
                "path": "web.env.API_URL",
                "services": ["api"],
                "instead": reference("http://${{ api.PLOYZ_PRIVATE_DOMAIN }}:8080/v1"),
            },
            {
                "path": "web.env.API_DB",
                "services": ["api"],
                "instead": reference("postgres://app:pw@${{ api.PLOYZ_PRIVATE_DOMAIN }}:5432/app"),
            },
            {
                "path": "web.env.API_HOST",
                "services": ["api"],
                "instead": reference("${{ api.PLOYZ_PRIVATE_DOMAIN }}"),
            },
            {
                "path": "web.env.API_ADDR",
                "services": ["api"],
                "instead": reference("${{ api.PLOYZ_PRIVATE_DOMAIN }}:8080"),
            },
            {
                "path": "web.env.API_LOGIN",
                "services": ["api"],
                "instead": reference(
                    "postgres://api:${{ DB_PASSWORD }}@${{ api.PLOYZ_PRIVATE_DOMAIN }}:5432/app"
                ),
            },
            // A secret can't hold a reference: nothing to show.
            {
                "path": "web.env.API_SECRET",
                "services": ["api"],
                "instead": { "kind": "sealed" },
            },
        ])
    );
    // Never rewritten: the value stays as typed.
    assert_eq!(
        value(&store, "web.env.API_URL"),
        "http://api.internal:8080/v1"
    );

    let quiet = set(
        &store,
        &[
            (
                "web.env.REFERENCED",
                json!("http://${{ api.PLOYZ_PRIVATE_DOMAIN }}:8080"),
            ),
            (
                "web.env.PINNED",
                json!("http://api.shop-production.internal"),
            ),
            (
                "web.env.EXTERNAL",
                json!("https://api.internal.example.com"),
            ),
            ("web.env.SELF", json!("http://web.internal:8080")),
            // Only the last value set to a variable is answered.
            ("web.env.API_DB", json!("http://api.internal")),
            (
                "web.env.API_DB",
                json!("http://${{ api.PLOYZ_PRIVATE_DOMAIN }}"),
            ),
        ],
    )
    .unwrap();
    assert!(quiet.typed_addresses.is_empty());

    // Setting what it answered stores the reference, which types nothing.
    let suggestion = "http://${{ api.PLOYZ_PRIVATE_DOMAIN }}:8080/v1";
    let fixed = set(&store, &[("web.env.API_URL", json!(suggestion))]).unwrap();
    assert!(fixed.typed_addresses.is_empty());
    assert_eq!(value(&store, "web.env.API_URL"), suggestion);
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
    let diff = store.read(&who(), &DiffQuery::default()).unwrap();
    let rows = diff
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .map(|row| (row.path.as_str(), row.after.clone()))
        .collect::<Vec<_>>();
    assert!(rows.contains(&("web.env.PASSWORD", json!({ "secret": true }))));
    assert!(rows.contains(&("web.env.DB_HOST", json!("db"))));
    let plan = store.read(&who(), &PlanQuery::default()).unwrap();

    let id = admit(&store, 1).unwrap();
    let reads = [
        json!(edited),
        json!(get(&store, None)),
        json!(get(&store, Some("web"))),
        json!(diff),
        json!(plan),
        json!(
            store
                .read(&who(), &ployz_store::DeploymentQuery { id: id.clone() })
                .unwrap()
        ),
        json!(store.read(&who(), &DeploymentsQuery::default()).unwrap()),
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
        .read(
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
    let error = set(&store, &[("api.env.URL", json!("${{ web.HOST"))]).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(error.message.contains("$${{"), "{}", error.message);
    // A variable web doesn't have is refused, naming what it has; a built-in isn't.
    let error = set(&store, &[("api.env.URL", json!("${{ web.HOST }}"))]).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(
        error.message.contains("PLOYZ_PRIVATE_DOMAIN"),
        "{}",
        error.message
    );
    assert_eq!(error.details["service"], "web");
    set(
        &store,
        &[("api.env.PEER", json!("${{ web.PLOYZ_PRIVATE_DOMAIN }}"))],
    )
    .unwrap();
    // One edit may reference a variable it sets later.
    set(
        &store,
        &[
            (
                "api.env.URL",
                json!("http://${{ web.HOST }}/ $${{ not.A_REF }}"),
            ),
            ("web.env.HOST", json!("web.internal")),
            ("web.env.HOST.exported", json!("true")),
            ("web.env.KEY", json!({ "secret": SECRET })),
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
        .write(
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
    // A cycle can't deploy, so it isn't staged, and the refusal names its variables.
    let error = set(
        &store,
        &[
            ("web.env.A", json!("${{ api.B }}")),
            ("api.env.B", json!("${{ web.A }}")),
        ],
    )
    .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        error.message,
        "api.B and web.A reference each other in a cycle"
    );
}

#[test]
fn diff_and_plan_show_a_plain_template_opener_escaped_as_get_does() {
    let store = backend::open();
    shop(&store);
    set(&store, &[("web.env.RAW", json!("echo $${{ HOME }}"))]).unwrap();
    assert_eq!(value(&store, "web.env.RAW"), "echo $${{ HOME }}");
    let after = |read: Value| {
        read["changes"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|change| change["settings"].as_array().unwrap())
            .find(|row| row["path"] == "web.env.RAW")
            .map(|row| row["after"].clone())
    };
    let diff = json!(store.read(&who(), &DiffQuery::default()).unwrap());
    assert_eq!(after(diff), Some(json!("echo $${{ HOME }}")));
    let plan = json!(store.read(&who(), &PlanQuery::default()).unwrap());
    assert!(plan.to_string().contains("echo $${{ HOME }}"), "{plan}");
    assert!(!plan.to_string().contains("echo ${{ HOME }}"), "{plan}");
}

#[test]
fn ployz_built_ins_cannot_be_set_but_port_can() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    for name in [
        "PLOYZ_PRIVATE_DOMAIN",
        "ployz_service_name",
        "PLOYZ_SERVICE_ID",
    ] {
        let refused = set(&store, &[(&format!("web.env.{name}"), json!("evil"))]).unwrap_err();
        assert!(
            refused.message.contains("is set by Ployz"),
            "{name}: {refused:?}"
        );
    }
    set(&store, &[("web.env.PORT", json!("8080"))]).unwrap();
    assert_eq!(value(&store, "web.env.PORT"), json!("8080"));
}
