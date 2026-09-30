//! Registry credentials through the Store's interface only, on SQLite and on
//! Postgres (see `backend`): a new secret applies at once, turning credentials on
//! or off is staged, a Deployment pulls with what it was admitted with, and the
//! secret leaves the Store only through `claim`.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

use ployz_core::{RegistryAuth, RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, Change, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
    DeploymentsQuery, DiffQuery, Discard, Edit, Edited, EnvironmentId, EnvironmentQuery,
    EnvironmentRef, OrganizationId, PlanQuery, ProjectId, ProjectName, Retry, RunEvidence,
    RunnerId, ServiceLineageId, SettingPath,
};
use serde_json::{Value, json};

mod backend;

const PATH: &str = "web.registryCredential";

fn who() -> Actor {
    Actor::system(OrganizationId::parse("org").unwrap())
}

/// Project `shop` with image Services `web` and `api` and an empty one, `blank`.
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
    for (n, name, image) in [
        (3, "web", Some("ghcr.io/acme/web:1")),
        (4, "api", Some("nginx:1")),
        (5, "blank", None),
    ] {
        store
            .write(
                &who(),
                &CreateService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: image.map(Into::into),
                    template: None,
                },
            )
            .unwrap();
    }
}

fn edit(store: &ConfigStore, change: Change) -> Result<Edited, RpcError> {
    store.write(
        &who(),
        &Edit {
            environment: EnvironmentRef::default(),
            expect: None,
            changes: vec![change],
        },
    )
}

fn set(store: &ConfigStore, path: &str, value: Value) -> Result<Edited, RpcError> {
    edit(
        store,
        Change::Set {
            path: SettingPath::parse(path).unwrap(),
            value,
        },
    )
}

fn unset(store: &ConfigStore, path: &str) -> Edited {
    edit(
        store,
        Change::Unset {
            path: SettingPath::parse(path).unwrap(),
        },
    )
    .unwrap()
}

fn paths(paths: &[SettingPath]) -> Vec<String> {
    paths.iter().map(ToString::to_string).collect()
}

fn value(store: &ConfigStore, path: &str) -> Value {
    store
        .read(
            &who(),
            &EnvironmentQuery {
                path: Some(SettingPath::parse(path).unwrap()),
                ..EnvironmentQuery::default()
            },
        )
        .unwrap()
        .settings
        .remove(0)
        .value
}

fn admit(store: &ConfigStore, n: u8) -> DeploymentId {
    let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap();
    store
        .write_trusted(
            &who(),
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                // `blank` has no source of its own: it builds this upload.
                upload: Some(ployz_store::UploadedSource {
                    digest: "d".repeat(64),
                    base: None,
                    uploader: None,
                }),
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    id
}

fn pulls_with(store: &ConfigStore, id: &DeploymentId) -> Vec<(String, RegistryAuth)> {
    store
        .claim(id, &RunnerId::parse("runner").unwrap())
        .unwrap()
        .intent
        .registry_auth
        .into_iter()
        .map(|(service, auth)| (service.to_string(), auth))
        .collect()
}

fn auth(username: Option<&str>, password: &str) -> RegistryAuth {
    RegistryAuth {
        username: username.map(Into::into),
        password: password.into(),
    }
}

#[test]
fn a_new_secret_applies_at_once_and_admission_freezes_what_a_deployment_pulls_with() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    let first = json!({ "username": "octocat", "secret": "first-token" });
    let edited = set(&store, PATH, first.clone()).unwrap();
    // Turning credentials on is staged; the secret itself is stored at once.
    assert_eq!(paths(&edited.staged), [PATH]);
    assert_eq!(paths(&edited.immediate), [PATH]);
    assert_eq!(value(&store, PATH), json!({ "secret": true }));
    // Sending back what reads show, or the same credential, changes nothing.
    for same in [json!({ "secret": true }), first] {
        let edited = set(&store, PATH, same).unwrap();
        assert!(edited.staged.is_empty() && edited.immediate.is_empty());
    }
    let diff = store.read(&who(), &DiffQuery::default()).unwrap();
    let row = diff
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|row| row.path == PATH)
        .unwrap();
    assert_eq!(
        (row.before.clone(), row.after.clone()),
        (Value::Null, json!({ "secret": true }))
    );
    let plan = store.read(&who(), &PlanQuery::default()).unwrap();
    let one = admit(&store, 1);
    assert_eq!(
        pulls_with(&store, &one),
        [("web".into(), auth(Some("octocat"), "first-token"))]
    );

    // Rotation stages nothing and never reaches an admitted Deployment.
    let rotated = set(&store, PATH, json!({ "secret": "second-token" })).unwrap();
    assert!(rotated.staged.is_empty());
    assert_eq!(paths(&rotated.immediate), [PATH]);
    assert_eq!(
        pulls_with(&store, &one),
        [("web".into(), auth(Some("octocat"), "first-token"))],
        "claiming again keeps the admitted credential"
    );
    // A retry of it ships the credential it froze, not the rotated one.
    store
        .record(
            &one,
            &RunnerId::parse("runner").unwrap(),
            RunEvidence::Abandoned,
        )
        .unwrap();
    let retried = DeploymentId::parse("00000000-0000-4000-8000-000000000199").unwrap();
    store
        .write_trusted(
            &who(),
            &Admit::Retry(Retry {
                id: retried.clone(),
                deployment: one.clone(),
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    assert_eq!(
        pulls_with(&store, &retried),
        [("web".into(), auth(Some("octocat"), "first-token"))]
    );
    // Each Service's credential is its own.
    set(
        &store,
        "api.registryCredential",
        json!({ "secret": "api-token" }),
    )
    .unwrap();
    let two = admit(&store, 2);
    assert_eq!(
        pulls_with(&store, &two),
        [
            ("api".into(), auth(None, "api-token")),
            ("web".into(), auth(None, "second-token")),
        ]
    );

    let reads = [
        json!(edited),
        json!(rotated),
        json!(store.read(&who(), &EnvironmentQuery::default()).unwrap()),
        json!(diff),
        json!(plan),
        json!(
            store
                .read(&who(), &ployz_store::DeploymentQuery { id: two.clone() })
                .unwrap()
        ),
        json!(store.read(&who(), &DeploymentsQuery::default()).unwrap()),
    ];
    for read in reads {
        let text = read.to_string();
        assert!(
            !text.contains("-token") && !text.contains("ciphertext"),
            "{text}"
        );
    }
    if let Some(path) = url.strip_prefix("sqlite:") {
        drop(store);
        let stored = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
        assert!(!stored.contains("-token") && !stored.contains("octocat"));
    }
}

#[test]
fn unset_keeps_the_stored_secret_and_discard_returns_to_the_introduction() {
    let store = backend::open();
    shop(&store);
    set(&store, PATH, json!({ "secret": "kept-token" })).unwrap();
    // Turning credentials off is staged and keeps the secret for turning them back on.
    let off = unset(&store, PATH);
    assert_eq!(paths(&off.staged), [PATH]);
    assert!(off.immediate.is_empty());
    assert_eq!(value(&store, PATH), Value::Null);
    let on = set(&store, PATH, json!({ "secret": true })).unwrap();
    assert_eq!(paths(&on.staged), [PATH]);
    assert!(on.immediate.is_empty());

    // A new Service's Setting discards to its Node Introduction, which never held a
    // credential; the stored secret stays.
    store
        .write(
            &who(),
            &Discard {
                environment: EnvironmentRef::default(),
                path: Some(SettingPath::parse(PATH).unwrap()),
                version: None,
            },
        )
        .unwrap();
    assert_eq!(value(&store, PATH), Value::Null);
    set(&store, PATH, json!({ "secret": true })).unwrap();
    assert_eq!(
        pulls_with(&store, &admit(&store, 1)),
        [("web".into(), auth(None, "kept-token"))]
    );
}

#[test]
fn a_credential_arrives_only_as_a_secret_and_is_never_echoed() {
    let store = backend::open();
    shop(&store);
    for (path, bad) in [
        (PATH, json!("plain-token")),
        (PATH, json!({ "secret": "" })),
        (PATH, json!({ "secret": false })),
        (PATH, json!({ "password": "plain-token" })),
        (PATH, json!({ "username": "octocat", "secret": true })),
        (PATH, json!({ "username": 3, "secret": "plain-token" })),
        (PATH, json!(null)),
        // Nothing stored to keep.
        (PATH, json!({ "secret": true })),
        // Only an image Service pulls one.
        (
            "blank.registryCredential",
            json!({ "secret": "plain-token" }),
        ),
    ] {
        let error = set(&store, path, bad.clone()).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{bad}");
        assert_eq!(error.details["setting"], "registryCredential");
        assert!(!error.to_string().contains("plain-token"), "{error:?}");
    }
    // A patch takes the same shape, and what `get` prints sends back unchanged.
    let patched = edit(
        &store,
        Change::Patch {
            path: SettingPath::parse("web").unwrap(),
            value: json!({ "registryCredential": { "username": "u", "secret": "patched-token" } }),
        },
    )
    .unwrap();
    assert_eq!(paths(&patched.immediate), [PATH]);
    let schema = ployz_store::catalog::schema(Some(PATH)).unwrap();
    assert_eq!(schema["x-ployz-secret"], true);
    assert_eq!(schema["x-ployz-apply"], "immediate");
}
