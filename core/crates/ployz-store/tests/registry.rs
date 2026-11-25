//! Registry logins through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): a login arrives only sealed, a new one applies at once, turning
//! credentials on or off is staged, a Deployment pulls with what it was admitted with,
//! and the sealed login leaves the Store only through `claim`.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

use ployz_core::{RpcError, RpcErrorCode, SealedLogin, ServiceName};
use ployz_store::{
    Actor, Admit, Change, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
    DeploymentsQuery, DiffQuery, Discard, Edit, Edited, EnvironmentId, EnvironmentQuery,
    EnvironmentRef, OrganizationId, PlanQuery, ProjectId, ProjectName, Retry, RunEvidence,
    RunnerId, ServiceLineageId, SettingPath,
};
use serde_json::{Value, json};

use ployz_store::testing::{backend, login_for, sealed_login_for};

/// A login for web's registry, ghcr.io.
fn login(tag: &str) -> SealedLogin {
    login_for("ghcr.io", tag)
}

/// [`login`] as a client sends it.
fn sealed_login(tag: &str) -> Value {
    sealed_login_for("ghcr.io", tag)
}

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
                region: None,
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
    try_admit(store, n).unwrap()
}

fn try_admit(store: &ConfigStore, n: u8) -> Result<DeploymentId, RpcError> {
    let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap();
    store.write_trusted(
        &who(),
        &Admit::Deploy(Deploy {
            id: id.clone(),
            environment: EnvironmentRef::default(),
            services: Vec::new(),
            version: None,
            // `blank` has no source of its own: it builds this upload.
            upload: Some(ployz_store::UploadedSource {
                digest: ployz_core::UploadDigest::parse("d".repeat(64)).unwrap(),
                base: None,
                uploader: None,
            }),
            accept_volume_loss: Vec::new(),
            message: None,
            prune: false,
            no_prune: false,
            versions: Vec::new(),
            copies: Vec::new(),
            recreate: Vec::new(),
            rule: None,
        }),
        &ployz_store::Trusted::default(),
    )?;
    Ok(id)
}

fn retry(store: &ConfigStore, of: DeploymentId) -> Result<DeploymentId, RpcError> {
    store.record(
        &of,
        &RunnerId::parse("runner").unwrap(),
        RunEvidence::Abandoned,
    )?;
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000199").unwrap();
    store.write_trusted(
        &who(),
        &Admit::Retry(Retry {
            id: id.clone(),
            deployment: of,
            rolls_back: None,
            version: None,
            accept_volume_loss: Vec::new(),
            versions: Vec::new(),
            copies: Vec::new(),
        }),
        &ployz_store::Trusted::default(),
    )?;
    Ok(id)
}

fn pulls_with(store: &ConfigStore, id: &DeploymentId) -> Vec<(String, SealedLogin)> {
    store
        .claim(id, &RunnerId::parse("runner").unwrap())
        .unwrap()
        .intent
        .registry_logins
        .into_iter()
        .map(|(service, login)| (service.to_string(), login))
        .collect()
}

#[test]
fn a_new_login_applies_at_once_and_admission_freezes_what_a_deployment_pulls_with() {
    let store = backend::open();
    shop(&store);
    let edited = set(&store, PATH, sealed_login("first")).unwrap();
    // Turning credentials on is staged; the secret itself is stored at once.
    assert_eq!(paths(&edited.staged), [PATH]);
    assert_eq!(paths(&edited.immediate), [PATH]);
    assert_eq!(value(&store, PATH), json!({ "secret": true }));
    // Sending back what reads show, as an object or as `set`'s JSON text, changes nothing.
    for same in [json!({ "secret": true }), json!(r#"{"secret":true}"#)] {
        let edited = set(&store, PATH, same).unwrap();
        assert!(edited.staged.is_empty() && edited.immediate.is_empty());
    }
    // Only the keep object passes as text, even with a credential to keep.
    for bad in [
        json!(r#"{"secret":"plain-token","secret":true}"#),
        json!("[true]"),
    ] {
        let error = set(&store, PATH, bad.clone()).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{bad}");
        assert!(!error.to_string().contains("plain-token"), "{error:?}");
    }
    let diff = store.read(&who(), &DiffQuery::default()).unwrap();
    let row = diff
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|row| row.path == "web.source")
        .unwrap();
    assert_eq!(
        (row.before.clone(), row.after.clone()),
        (
            json!({ "type": "image", "image": "ghcr.io/acme/web:1", "credentials": false }),
            json!({ "type": "image", "image": "ghcr.io/acme/web:1", "credentials": true })
        )
    );
    let plan = store.read(&who(), &PlanQuery::default()).unwrap();
    let one = admit(&store, 1);
    assert_eq!(pulls_with(&store, &one), [("web".into(), login("first"))]);

    // Rotation stages nothing and never reaches an admitted Deployment.
    let rotated = set(&store, PATH, sealed_login("second")).unwrap();
    assert!(rotated.staged.is_empty());
    assert_eq!(paths(&rotated.immediate), [PATH]);
    assert_eq!(
        pulls_with(&store, &one),
        [("web".into(), login("first"))],
        "claiming again keeps the admitted credential"
    );
    // A retry of it ships the credential it froze, not the rotated one.
    let retried = retry(&store, one).unwrap();
    assert_eq!(
        pulls_with(&store, &retried),
        [("web".into(), login("first"))]
    );
    // Each Service's credential is its own.
    set(
        &store,
        "api.registryCredential",
        sealed_login_for("docker.io", "api"),
    )
    .unwrap();
    let two = admit(&store, 2);
    assert_eq!(
        pulls_with(&store, &two),
        [
            ("api".into(), login_for("docker.io", "api")),
            ("web".into(), login("second")),
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
                .read(&who(), &ployz_store::DeploymentQuery { id: two })
                .unwrap()
        ),
        json!(store.read(&who(), &DeploymentsQuery::default()).unwrap()),
    ];
    let sealed = String::from(login("second").sealed);
    for read in reads {
        let text = read.to_string();
        assert!(!text.contains(&sealed), "{text}");
    }
}

#[test]
fn unset_keeps_the_stored_secret_and_discard_returns_to_the_introduction() {
    let store = backend::open();
    shop(&store);
    set(&store, PATH, sealed_login("kept")).unwrap();
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
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse(PATH).unwrap()),
                version: None,
            },
        )
        .unwrap();
    assert_eq!(value(&store, PATH), Value::Null);
    set(&store, PATH, json!({ "secret": true })).unwrap();
    assert_eq!(
        pulls_with(&store, &admit(&store, 1)),
        [("web".into(), login("kept"))]
    );
}

#[test]
fn a_login_arrives_only_sealed_and_is_never_echoed() {
    let store = backend::open();
    shop(&store);
    let mut unknown = sealed_login("extra");
    unknown["username"] = json!("octocat");
    for (path, bad) in [
        (PATH, json!("plain-token")),
        (PATH, json!({ "secret": "plain-token" })),
        (
            PATH,
            json!({ "username": "octocat", "secret": "plain-token" }),
        ),
        (PATH, json!({ "host": "ghcr.io", "sealed": "plain-token" })),
        (PATH, unknown),
        (PATH, json!({ "secret": "" })),
        (PATH, json!(r#"{"secret":"plain-token"}"#)),
        (
            PATH,
            json!(r#"{"username":"octocat","secret":"plain-token"}"#),
        ),
        (PATH, json!({ "secret": false })),
        (PATH, json!({ "password": "plain-token" })),
        (PATH, json!({ "username": "octocat", "secret": true })),
        (PATH, json!({ "username": 3, "secret": "plain-token" })),
        (PATH, json!(null)),
        // Nothing stored to keep.
        (PATH, json!({ "secret": true })),
        // Only an image Service pulls one.
        ("blank.registryCredential", sealed_login("blank")),
    ] {
        let error = set(&store, path, bad.clone()).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{bad}");
        assert_eq!(error.details["setting"], "registryCredential");
        assert!(!error.to_string().contains("plain-token"), "{error:?}");
    }
    // A login's plain text never reaches the Store: it is sealed on the person's device.
    let refused = set(&store, PATH, json!({ "secret": "plain-token" })).unwrap_err();
    assert!(
        refused.message.contains("sealed on your device"),
        "{refused:?}"
    );
    // A patch takes the same shape, and what `get` prints sends back unchanged.
    let patched = edit(
        &store,
        Change::Patch {
            path: SettingPath::parse("web").unwrap(),
            value: json!({ "registryCredential": sealed_login("patched") }),
        },
    )
    .unwrap();
    assert_eq!(paths(&patched.immediate), [PATH]);
    let schema = ployz_store::catalog::schema(Some(PATH)).unwrap();
    assert_eq!(schema["x-ployz-secret"], true);
    assert_eq!(schema["x-ployz-apply"], "immediate");
}

#[test]
fn a_credential_lost_from_the_store_says_to_set_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    set(&store, PATH, sealed_login("first")).unwrap();
    backend::execute(&url, "DELETE FROM config_registry_login");
    let refused = store
        .write_trusted(
            &who(),
            &Admit::Deploy(Deploy {
                id: DeploymentId::parse("00000000-0000-4000-8000-000000000199").unwrap(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                copies: Vec::new(),
                recreate: Vec::new(),
                rule: None,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap_err();
    assert_eq!(
        refused.details["next"],
        "ployz set web.registryCredential --secret"
    );
}

/// A Deployment admitted before logins were sealed froze none, so redeploying it would
/// pull its private image with no login, again and again: it says to deploy instead.
#[test]
fn a_deployment_that_froze_no_login_is_deployed_again_rather_than_redeployed() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    set(&store, PATH, sealed_login("first")).unwrap();
    let one = admit(&store, 1);
    pulls_with(&store, &one);
    // What the migration left a Deployment admitted with the Store's own credentials.
    backend::execute(&url, "UPDATE config_deployment SET logins = '{}'");
    let refused = retry(&store, one).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert!(
        refused
            .message
            .contains("deploy again rather than redeploy"),
        "{refused:?}"
    );
    assert_eq!(
        refused.details["next"],
        "ployz set web.registryCredential --secret"
    );
    assert_eq!(
        pulls_with(&store, &admit(&store, 2)),
        [("web".into(), login("first"))]
    );
}

/// A login opens only for the registry it was sealed for, so once the image moves to
/// another, admission says to enter it again rather than let the pull fail.
#[test]
fn a_login_for_another_registry_is_entered_again_before_admission() {
    let store = backend::open();
    shop(&store);
    set(&store, PATH, sealed_login("first")).unwrap();
    set(&store, "web.image", json!("quay.io/acme/web:1")).unwrap();
    let refused = try_admit(&store, 1).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert!(
        refused
            .message
            .contains("are for ghcr.io, and the image is on quay.io"),
        "{refused:?}"
    );
    assert_eq!(
        refused.details["next"],
        "ployz set web.registryCredential --secret"
    );
}
