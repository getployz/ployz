//! The `sealedVariables` flag and a variable value Edit the browser sealed, through
//! the Store's interface only, on SQLite and on Postgres (see `backend`): the flag
//! is off for every Organization until a row exists, which nothing in the Store
//! inserts, and gone when the Organization is torn down; a sealed value is taken
//! only with it on, only sealed for the variable it lands in, and never read.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

use ployz_core::{EnvValue, RpcError, RpcErrorCode, SealedIdentity, ServiceName};
use ployz_store::{
    Actor, Admit, Change, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
    DiffQuery, Edit, Edited, EnvironmentId, EnvironmentQuery, EnvironmentRef, OrganizationId,
    ProjectId, ProjectName, RunnerId, SealingService, SealingTargetsQuery, SealingTargetsView,
    ServiceLineageId, SettingPath, Trusted,
};
use serde_json::{Value, json};

mod backend;

const SECRET: &str = "s3cr3t-pässwörd 🔑";

fn org() -> OrganizationId {
    OrganizationId::parse("org").unwrap()
}

fn who() -> Actor {
    Actor::system(org())
}

/// Project `shop` with Service `web` in the Store at `store`.
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
    store
        .write(
            &who(),
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
}

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn edit(store: &ConfigStore, path: &str, value: Value) -> Result<Edited, RpcError> {
    store.write(
        &who(),
        &Edit {
            environment: EnvironmentRef::default(),
            expect: None,
            changes: vec![Change::Set {
                path: SettingPath::parse(path).unwrap(),
                value,
            }],
        },
    )
}

fn set(store: &ConfigStore, path: &str, value: Value) {
    edit(store, path, value).unwrap();
}

fn staged(edited: &Edited) -> Vec<String> {
    edited.staged.iter().map(ToString::to_string).collect()
}

/// `web.env.PASSWORD` sealed in the browser for `web` of the default Environment.
fn password(n: u8) -> Value {
    json!({ "secret": backend::client_sealed(&uuid(2), &uuid(3), "PASSWORD", n) })
}

/// `edit` refused as invalid, naming the variable and why, never the ciphertext.
fn refused(store: &ConfigStore, path: &str, value: Value, why: &str) {
    let error = edit(store, path, value).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
    let key = path.rsplit('.').next().unwrap();
    assert!(error.message.starts_with(key), "{error:?}");
    assert!(error.message.contains(why), "{error:?}");
    assert!(!format!("{error:?}").contains("AAAA"), "{error:?}");
}

/// Admit Deployment 101 of the default Environment and claim it, as a runner does.
fn claim(store: &ConfigStore) -> ployz_store::Claimed {
    try_claim(store).unwrap()
}

fn try_claim(store: &ConfigStore) -> Result<ployz_store::Claimed, RpcError> {
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write_trusted(
            &who(),
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    store.claim(&id, &RunnerId::parse("runner").unwrap())
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

#[test]
fn a_fresh_store_has_no_flag_row_and_the_legacy_path_runs_unchanged_with_none() {
    let store = backend::open();
    assert!(!store.sealed_variables(&org()).unwrap());
    shop(&store);
    set(&store, "web.env.PASSWORD", json!({ "secret": SECRET }));
    set(&store, "web.env.URL", json!("pg://${{ PASSWORD }}@db"));
    assert_eq!(value(&store, "web.env.PASSWORD"), json!({ "secret": true }));
    let diff = store.read(&who(), &DiffQuery::default()).unwrap();
    let password = diff
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|row| row.path.as_str() == "web.env.PASSWORD")
        .unwrap();
    assert_eq!(password.after, json!({ "secret": true }));
    let claimed = claim(&store);
    let web = claimed
        .intent
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "web")
        .unwrap();
    let environment = json!(web.container.environment);
    assert_eq!(environment["PASSWORD"], SECRET);
    assert_eq!(environment["URL"], format!("pg://{SECRET}@db"));
    assert!(web.container.sealed_identity.is_none());
    assert!(!store.sealed_variables(&org()).unwrap());
}

#[test]
fn the_flag_is_on_only_with_a_row_and_teardown_of_the_organization_deletes_it() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    let other = OrganizationId::parse("other").unwrap();
    assert!(!store.sealed_variables(&org()).unwrap());
    backend::sealed_variables_on(&url, "org");
    backend::sealed_variables_on(&url, "other");
    assert!(store.sealed_variables(&org()).unwrap());
    assert!(store.sealed_variables(&other).unwrap());
    store.remove_organization(&who()).unwrap();
    assert!(!store.sealed_variables(&org()).unwrap());
    assert!(store.sealed_variables(&other).unwrap());
}

#[test]
fn a_sealed_value_is_taken_only_with_the_flag_on_and_only_sealed_for_its_variable() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    refused(
        &store,
        "web.env.PASSWORD",
        password(1),
        "off for this Organization",
    );
    refused(
        &store,
        "web.env.PASSWORD",
        json!({ "secret": { "sealedFor": uuid(2) } }),
        "sealed in the browser",
    );

    backend::sealed_variables_on(&url, "org");
    let sealed = |environment: u8, service: u8, key: &str| json!({ "secret": backend::client_sealed(&uuid(environment), &uuid(service), key, 1) });
    refused(
        &store,
        "web.env.PASSWORD",
        sealed(9, 3, "PASSWORD"),
        "sealed for another Environment",
    );
    refused(
        &store,
        "web.env.PASSWORD",
        sealed(2, 3, "OTHER"),
        "sealed for another variable",
    );
    refused(
        &store,
        "web.env.PASSWORD",
        sealed(2, 4, "PASSWORD"),
        "sealed for another Service",
    );
    let shared = json!({ "secret": {
        "sealedFor": uuid(2),
        "value": { "sealed": backend::ciphertext(1), "scope": "shared", "key": "PASSWORD" },
    } });
    refused(
        &store,
        "web.env.PASSWORD",
        shared,
        "sealed for every Service",
    );
    let edited = edit(&store, "web.env.PASSWORD", password(1)).unwrap();
    assert_eq!(staged(&edited), ["web.env.PASSWORD"]);
    assert_eq!(value(&store, "web.env.PASSWORD"), json!({ "secret": true }));
    let diff = store.read(&who(), &DiffQuery::default()).unwrap();
    assert!(!format!("{diff:?}").contains("AAAA"), "{diff:?}");
}

#[test]
fn a_sealed_secret_is_kept_by_its_marker_and_never_becomes_plain() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    backend::sealed_variables_on(&url, "org");
    set(&store, "web.env.PASSWORD", password(1));
    for same in [json!({ "secret": true }), password(1)] {
        let edited = edit(&store, "web.env.PASSWORD", same).unwrap();
        assert!(staged(&edited).is_empty(), "{edited:?}");
    }
    let error = edit(&store, "web.env.PASSWORD", json!("plain now")).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert!(error.message.contains("never becomes plain text"));
    // A new secret replaces it, sealed either way.
    for new in [json!({ "secret": SECRET }), password(2)] {
        let edited = edit(&store, "web.env.PASSWORD", new).unwrap();
        assert_eq!(staged(&edited), ["web.env.PASSWORD"]);
    }
    assert_eq!(value(&store, "web.env.PASSWORD"), json!({ "secret": true }));
}

#[test]
fn a_sealed_value_skips_the_text_and_typed_address_checks_that_need_its_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    backend::sealed_variables_on(&url, "org");
    // A plain secret named like an address is checked for one; a sealed one can't be.
    let sealed = json!({ "secret": backend::client_sealed(&uuid(2), &uuid(3), "DATABASE_URL", 1) });
    let edited = edit(&store, "web.env.DATABASE_URL", sealed).unwrap();
    assert_eq!(staged(&edited), ["web.env.DATABASE_URL"]);
    assert!(edited.typed_addresses.is_empty(), "{edited:?}");
    // The length bound still holds, on the plaintext's size the ciphertext implies.
    let long = "A".repeat(4 * 44 * 1024);
    let too_long = json!({ "secret": {
        "sealedFor": uuid(2),
        "value": { "sealed": long, "scope": { "service": uuid(3) }, "key": "PASSWORD" },
    } });
    refused(&store, "web.env.PASSWORD", too_long, "too long");
}

#[test]
fn lowering_carries_a_sealed_value_and_a_template_around_it_to_the_deploy_unopened() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    backend::sealed_variables_on(&url, "org");
    set(&store, "web.env.PASSWORD", password(1));
    set(&store, "web.env.URL", json!("pg://${{ PASSWORD }}@db"));
    set(&store, "web.env.TOKEN", json!({ "secret": SECRET }));
    // The Store's key can't open the browser's ciphertext: a claim that opened it
    // would fail here, so a claim that succeeds never consulted it.
    let claimed = claim(&store);
    let web = claimed
        .intent
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "web")
        .unwrap();
    let sealed = backend::client_sealed(&uuid(2), &uuid(3), "PASSWORD", 1)["value"].clone();
    let environment = &web.container.environment;
    assert!(matches!(environment["PASSWORD"], EnvValue::Sealed(_)));
    assert_eq!(json!(environment["PASSWORD"]), sealed);
    assert!(matches!(environment["URL"], EnvValue::Template(_)));
    assert_eq!(
        json!(environment["URL"]),
        json!({ "template": ["pg://", sealed, "@db"] })
    );
    assert_eq!(environment["TOKEN"], EnvValue::Text(SECRET.into()));
    assert_eq!(
        web.container.sealed_identity,
        Some(SealedIdentity {
            organization: "org".into(),
            environment: uuid(2).parse().unwrap(),
            service: uuid(3).parse().unwrap(),
        })
    );
    let snapshot = claimed.input["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|snapshot| snapshot["serviceId"] == uuid(3))
        .unwrap();
    assert_eq!(snapshot["resolvedEnv"]["PASSWORD"], sealed);
    assert_eq!(
        snapshot["resolvedEnv"]["URL"],
        json!({ "template": ["pg://", sealed, "@db"] })
    );
    assert_eq!(snapshot["resolvedEnv"]["TOKEN"], SECRET);
}

#[test]
fn a_template_that_opens_past_a_containers_variable_limit_is_refused_at_claim() {
    use base64::Engine as _;
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    backend::sealed_variables_on(&url, "org");
    let sealed = base64::engine::general_purpose::STANDARD.encode(vec![7; 70_000 + 48]);
    set(
        &store,
        "web.env.PASSWORD",
        json!({ "secret": {
            "sealedFor": uuid(2),
            "value": { "sealed": sealed, "scope": { "service": uuid(3) }, "key": "PASSWORD" },
        } }),
    );
    set(
        &store,
        "web.env.DOUBLE",
        json!("${{ PASSWORD }}${{ PASSWORD }}"),
    );
    let error = try_claim(&store).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
    assert!(
        error.message.starts_with("web.env.DOUBLE is too long"),
        "{error:?}"
    );
    assert!(!format!("{error:?}").contains("BwcH"), "{error:?}");
}

/// The longest value Linux passes a Container as `PASSWORD=value` with its NUL.
const PASSWORD_MAX: usize = 128 * 1024 - "PASSWORD".len() - 2;

/// `PASSWORD` sealed in the browser so that it opens to `opened` bytes: HPKE adds
/// 48 to the plaintext.
fn sealed_password(opened: usize) -> Value {
    use base64::Engine as _;
    let sealed = base64::engine::general_purpose::STANDARD.encode(vec![7; opened + 48]);
    json!({ "secret": {
        "sealedFor": uuid(2),
        "value": { "sealed": sealed, "scope": { "service": uuid(3) }, "key": "PASSWORD" },
    } })
}

#[test]
fn a_sealed_value_is_taken_up_to_the_limit_at_the_length_it_opens_to() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    backend::sealed_variables_on(&url, "org");
    refused(
        &store,
        "web.env.PASSWORD",
        sealed_password(PASSWORD_MAX + 1),
        "too long",
    );
    set(&store, "web.env.PASSWORD", sealed_password(PASSWORD_MAX));
}

#[test]
fn a_template_is_claimed_up_to_the_limit_at_the_length_its_sealed_parts_open_to() {
    // `COPY=` is 4 bytes shorter than `PASSWORD=`, so 4 bytes of text fill it exactly.
    for (text, fits) in [("abcd", true), ("abcde", false)] {
        let dir = tempfile::tempdir().unwrap();
        let url = backend::fresh_url(&dir);
        let store = ConfigStore::open(&url, backend::key()).unwrap();
        shop(&store);
        backend::sealed_variables_on(&url, "org");
        set(&store, "web.env.PASSWORD", sealed_password(PASSWORD_MAX));
        set(
            &store,
            "web.env.COPY",
            json!(format!("{text}${{{{ PASSWORD }}}}")),
        );
        let claimed = try_claim(&store);
        if fits {
            claimed.unwrap();
            continue;
        }
        let error = claimed.unwrap_err();
        assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
        assert!(
            error.message.starts_with("web.env.COPY is too long"),
            "{error:?}"
        );
        assert!(!format!("{error:?}").contains("BwcH"), "{error:?}");
    }
}

#[test]
fn a_renamed_sealed_variable_reaches_the_deploy_as_a_template_under_the_key_it_was_sealed_for() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    backend::sealed_variables_on(&url, "org");
    set(&store, "web.env.PASSWORD", password(1));
    store
        .write(
            &who(),
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Rename {
                    path: SettingPath::parse("web.env.PASSWORD").unwrap(),
                    to: "DB_PASSWORD".into(),
                    update_references: false,
                }],
            },
        )
        .unwrap();
    let claimed = claim(&store);
    let web = claimed
        .intent
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "web")
        .unwrap();
    let sealed = backend::client_sealed(&uuid(2), &uuid(3), "PASSWORD", 1)["value"].clone();
    assert!(!web.container.environment.contains_key("PASSWORD"));
    assert_eq!(
        json!(web.container.environment["DB_PASSWORD"]),
        json!({ "template": [sealed] })
    );
}

#[test]
fn a_sealed_port_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    backend::sealed_variables_on(&url, "org");
    refused(
        &store,
        "web.env.PORT",
        json!({ "secret": backend::client_sealed(&uuid(2), &uuid(3), "PORT", 1) }),
        "route traffic",
    );
    // A PORT the Store sealed is opened at lowering, so it still routes.
    set(&store, "web.env.PORT", json!({ "secret": "8080" }));
    let claimed = claim(&store);
    let web = claimed
        .intent
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "web")
        .unwrap();
    assert_eq!(
        web.container.environment["PORT"],
        EnvValue::Text("8080".into())
    );
}

#[test]
fn a_sealed_variable_is_never_renamed_to_port() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    backend::sealed_variables_on(&url, "org");
    set(&store, "web.env.PASSWORD", password(1));
    let error = store
        .write(
            &who(),
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Rename {
                    path: SettingPath::parse("web.env.PASSWORD").unwrap(),
                    to: "PORT".into(),
                    update_references: false,
                }],
            },
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
    assert!(error.message.starts_with("PORT"), "{error:?}");
    assert!(error.message.contains("route traffic"), "{error:?}");
    assert!(!format!("{error:?}").contains("AQEB"), "{error:?}");
    assert_eq!(value(&store, "web.env.PASSWORD"), json!({ "secret": true }));
}

#[test]
fn a_port_that_reads_a_sealed_value_is_refused_at_claim() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    backend::sealed_variables_on(&url, "org");
    // PORT reads PASSWORD while it is plain text, and still once it is sealed.
    set(&store, "web.env.PASSWORD", json!("8080"));
    set(&store, "web.env.PORT", json!("${{ PASSWORD }}"));
    set(&store, "web.env.PASSWORD", password(1));
    let error = try_claim(&store).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
    assert!(
        error.message.contains("web.env.PORT can't be sealed"),
        "{error:?}"
    );
    assert!(!format!("{error:?}").contains("AQEB"), "{error:?}");
}

/// `path` set to `value` by the CLI, as Cloud marks a write over HTTPS from it.
fn cli(store: &ConfigStore, changes: Vec<Change>) -> Result<Edited, RpcError> {
    store.write_trusted(
        &who(),
        &Edit {
            environment: EnvironmentRef::default(),
            expect: None,
            changes,
        },
        &Trusted {
            from_cli: true,
            ..Trusted::default()
        },
    )
}

fn set_change(path: &str, value: Value) -> Change {
    Change::Set {
        path: SettingPath::parse(path).unwrap(),
        value,
    }
}

#[test]
fn an_old_cli_s_plain_text_secret_is_refused_only_with_the_flag_on_and_only_a_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    cli(
        &store,
        vec![set_change("web.env.PASSWORD", json!({ "secret": SECRET }))],
    )
    .unwrap();
    assert_eq!(value(&store, "web.env.PASSWORD"), json!({ "secret": true }));

    backend::sealed_variables_on(&url, "org");
    let refused = |changes: Vec<Change>, key: &str| {
        let error = cli(&store, changes).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::Unsupported, "{error:?}");
        assert_eq!(
            error.message,
            format!("{key}: update ployz to set a Sealed value")
        );
        assert_eq!(
            error.details,
            json!({ "variable": key, "next": "curl -fsSL https://ployz.sh | sh" })
        );
        assert!(!format!("{error:?}").contains(SECRET), "{error:?}");
    };
    refused(
        vec![set_change("web.env.TOKEN", json!({ "secret": SECRET }))],
        "TOKEN",
    );
    refused(
        vec![set_change(
            "web.env.TOKEN",
            json!({ "value": { "secret": SECRET }, "exported": true }),
        )],
        "TOKEN",
    );
    refused(
        vec![Change::Patch {
            path: SettingPath::parse("web").unwrap(),
            value: json!({ "env": { "TOKEN": { "secret": SECRET } } }),
        }],
        "TOKEN",
    );
    // A secret set again is a new one too.
    refused(
        vec![set_change(
            "web.env.PASSWORD",
            json!({ "secret": "another" }),
        )],
        "PASSWORD",
    );
    let absent = store
        .read(
            &who(),
            &EnvironmentQuery {
                path: Some(SettingPath::parse("web.env.TOKEN").unwrap()),
                ..EnvironmentQuery::default()
            },
        )
        .unwrap_err();
    assert_eq!(absent.code, RpcErrorCode::NotFound, "{absent:?}");
    // Everything but new plain text passes: a kept secret, a generated one, a registry
    // credential, a value sealed in the browser, and text that isn't a secret.
    for (path, value) in [
        ("web.env.PASSWORD", json!({ "secret": true })),
        (
            "web.env.GENERATED",
            json!({ "secret": { "generate": true } }),
        ),
        (
            "web.registryCredential",
            json!({ "secret": "registry-token" }),
        ),
        ("web.env.PASSWORD", password(1)),
        ("web.env.PLAIN", json!("plain text")),
    ] {
        cli(&store, vec![set_change(path, value)]).unwrap();
    }
    // Not from the CLI, the Store still seals plain text itself.
    set(&store, "web.env.TOKEN", json!({ "secret": SECRET }));
    assert_eq!(value(&store, "web.env.TOKEN"), json!({ "secret": true }));
}

#[test]
fn sealing_targets_name_the_organization_the_environment_and_each_service_by_id() {
    let store = backend::open();
    shop(&store);
    let view = store
        .read(
            &who(),
            &SealingTargetsQuery::Edit {
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap();
    let SealingTargetsView::Edit {
        organization,
        environment,
        services,
    } = view
    else {
        panic!("{view:?}");
    };
    assert_eq!(organization, "org");
    assert_eq!(environment.id.as_str(), uuid(2));
    assert_eq!(environment.project.as_str(), "shop");
    assert_eq!(
        services,
        [SealingService {
            name: ServiceName::parse("web").unwrap(),
            service: uuid(3).parse().unwrap(),
        }]
    );
    let missing = store
        .read(
            &who(),
            &SealingTargetsQuery::Edit {
                environment: EnvironmentRef {
                    project: Some(ProjectName::parse("nothing").unwrap()),
                    environment: None,
                },
            },
        )
        .unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::NotFound, "{missing:?}");
}
