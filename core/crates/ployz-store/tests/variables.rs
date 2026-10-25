//! Variables and sealed secrets through the Store's interface only, on SQLite and on
//! Postgres (see `backend`): what reads show, what edits accept, and that a sealed
//! value reaches a claim as the browser sealed it.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

use ployz_core::{ConfigFileName, ConfigName, RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, Change, ConfigId, ConfigMountAt, ConfigStore, ConfigsQuery, CreateConfig,
    CreateProject, CreateService, Deploy, DeploymentId, DeploymentsQuery, DiffQuery, Edit, Edited,
    EnvironmentId, EnvironmentQuery, EnvironmentRef, OrganizationId, PlanQuery, ProjectId,
    ProjectName, PutConfigFile, RunnerId, SealedNamesQuery, SealingKey, ServiceLineageId,
    SettingPath,
};
use serde_json::{Value, json};

mod backend;

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

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

/// Variable `key` of Service `service` (3 is `web`, 4 is `api`) sealed in the browser
/// for the default Environment, holding ciphertext `n`.
fn sealed(service: u8, key: &str, n: u8) -> Value {
    backend::sealed_secret(&uuid(2), &uuid(service), key, n)
}

/// What a claim carries for [`sealed`]: the value without the Environment it was sealed for.
fn carried(service: u8, key: &str, n: u8) -> Value {
    backend::client_sealed(&uuid(2), &uuid(service), key, n)["value"].clone()
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

/// Rename variable `path` to `to`, after setting `changes`, in one edit.
fn rename(
    store: &ConfigStore,
    changes: &[(&str, Value)],
    path: &str,
    to: &str,
) -> Result<Edited, RpcError> {
    rename_with(store, changes, path, to, false)
}

/// [`rename`], switching the variables that reference it with `update_references`.
fn rename_with(
    store: &ConfigStore,
    changes: &[(&str, Value)],
    path: &str,
    to: &str,
    update_references: bool,
) -> Result<Edited, RpcError> {
    let mut changes = changes
        .iter()
        .map(|(path, value)| Change::Set {
            path: SettingPath::parse(path).unwrap(),
            value: value.clone(),
        })
        .collect::<Vec<_>>();
    changes.push(Change::Rename {
        path: SettingPath::parse(path).unwrap(),
        to: to.to_owned(),
        update_references,
    });
    store.write(
        &who(),
        &Edit {
            environment: EnvironmentRef::default(),
            expect: None,
            changes,
        },
    )
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
            prune: false,
            no_prune: false,
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
fn a_value_too_long_for_the_environment_is_refused_when_set() {
    let store = backend::open();
    shop(&store);
    // Linux passes each `KEY=value` string, with its NUL, in at most 128 KiB.
    let fits = "x".repeat(128 * 1024 - "BIG=".len() - 1);
    set(&store, &[("web.env.BIG", json!(fits))]).unwrap();
    let before = get(&store, None);
    let over = format!("{fits}x");
    let error = set(&store, &[("web.env.BIG", json!(over))]).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        error.message,
        "BIG: too long for a Container's environment: Linux passes at most 128 KiB per variable, name included"
    );
    assert_eq!(get(&store, None), before);
}

/// What `KEY=value` takes of a program's environment: the string with its NUL, and
/// an 8-byte pointer to it.
fn env_bytes(keys: impl Iterator<Item = String>, value: usize) -> usize {
    keys.map(|key| key.len() + 1 + value + 1 + 8).sum()
}

fn too_big(service: &str, kib: usize) -> String {
    format!(
        "{service}: too big for a Container's environment: its variables take {kib} KiB, and Linux passes at most 6144 KiB of variables and arguments together, whatever the stack limit"
    )
}

#[test]
fn an_environment_no_container_takes_is_refused_when_set() {
    let store = backend::open();
    shop(&store);
    let big = "x".repeat(64 * 1024);
    let paths: Vec<String> = (0..100).map(|n| format!("web.env.V{n}")).collect();
    let changes: Vec<(&str, Value)> = paths
        .iter()
        .map(|path| (path.as_str(), json!(big)))
        .collect();
    let before = get(&store, None);
    let error = set(&store, &changes).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    let kib = env_bytes((0..100).map(|n| format!("V{n}")), 64 * 1024).div_ceil(1024);
    assert_eq!(error.message, too_big("web", kib));
    assert_eq!(get(&store, None), before);

    // Past what Docker's default stack lets Linux pass, a set lands with a note.
    let edited = set(&store, &changes[..40]).unwrap();
    let kib = env_bytes((0..40).map(|n| format!("V{n}")), 64 * 1024).div_ceil(1024);
    assert_eq!(
        edited
            .notes
            .iter()
            .map(|noted| noted.note.as_str())
            .collect::<Vec<_>>(),
        [format!(
            "web's variables take {kib} KiB. With Docker's default 8 MiB stack, Linux passes a Container at most 2048 KiB of variables and arguments, so it won't start unless the Server raises that limit"
        )]
    );

    // What fits is judged where the whole edit leaves it.
    set(&store, &changes[..95]).unwrap();
    let mut swap = vec![(changes[0].0, json!(""))];
    swap.extend(changes[95..96].iter().cloned());
    set(&store, &swap).unwrap();

    // A template counts only at deploy: a broken reference leaves its variable out.
    set(
        &store,
        &[(
            "web.env.T",
            json!(format!("{}${{{{ MISSING }}}}", "x".repeat(90 * 1024))),
        )],
    )
    .unwrap();
}

#[test]
fn references_that_resolve_past_any_container_refuse_only_a_deploy_that_starts_them() {
    let store = backend::open();
    shop(&store);
    let paths: Vec<String> = (0..65).map(|n| format!("web.env.R{n}")).collect();
    let mut changes: Vec<(&str, Value)> = paths
        .iter()
        .map(|path| (path.as_str(), json!("${{ api.HALF }}")))
        .collect();
    changes.push(("api.env.HALF", json!("x".repeat(100 * 1024))));
    let edited = set(&store, &changes).unwrap();
    assert!(edited.notes.is_empty(), "{:?}", edited.notes);
    let error = admit(&store, 1).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    let kib = env_bytes((0..65).map(|n| format!("R{n}")), 100 * 1024).div_ceil(1024);
    assert_eq!(error.message, too_big("web", kib));

    // A Deployment of api alone starts nothing web holds.
    store
        .write_trusted(
            &who(),
            &Admit::Deploy(Deploy {
                id: DeploymentId::parse("00000000-0000-4000-8000-000000000102").unwrap(),
                environment: EnvironmentRef::default(),
                services: vec![ServiceName::parse("api").unwrap()],
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
}

#[test]
fn a_reference_that_resolves_too_long_refuses_the_deploy() {
    let store = backend::open();
    shop(&store);
    let half = "x".repeat(70 * 1024);
    set(
        &store,
        &[
            ("api.env.HALF", json!(half)),
            ("web.env.BOTH", json!("${{ api.HALF }}${{ api.HALF }}")),
        ],
    )
    .unwrap();
    let error = admit(&store, 1).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        error.message,
        "web.env.BOTH is too long for a Container's environment: Linux passes at most 128 KiB per variable, name included"
    );
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
            ("web.env.DB_PASSWORD", sealed(3, "DB_PASSWORD", 1)),
            (
                "web.env.API_LOGIN",
                json!("postgres://api:${{ DB_PASSWORD }}@api:5432/app"),
            ),
            // Elsewhere a bare name is just a word, or an image.
            ("web.env.PROCESS_TYPE", json!("api")),
            ("web.env.IMAGE", json!("api:16")),
            // A sealed value can't be read for one.
            ("web.env.API_SECRET", sealed(3, "API_SECRET", 2)),
        ],
    )
    .unwrap();
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
fn secrets_never_leave_reads_and_reach_a_claim_as_sealed() {
    let store = backend::open();
    shop(&store);
    let edited = set(
        &store,
        &[
            ("web.env.password", sealed(3, "PASSWORD", 1)),
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
        assert!(!text.contains(&backend::ciphertext(1)), "{text}");
    }
    let claimed = store
        .claim(&id, &RunnerId::parse("runner").unwrap())
        .unwrap();
    let sealed = carried(3, "PASSWORD", 1);
    assert_eq!(environment_of(&claimed, "web")["PASSWORD"], sealed);
    let api = environment_of(&claimed, "api");
    assert_eq!(
        api["DATABASE_URL"],
        json!({ "template": ["postgres://", sealed, "@db/app"] })
    );
    assert_eq!(api["WEB"], "web.internal");
    assert_eq!(
        claimed
            .sealed_names
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["DATABASE_URL", "PASSWORD", "WEB"]
    );
    // A Service waits for the Services its variables reference.
    let api = ServiceName::parse("api").unwrap();
    assert_eq!(
        claimed.intent.dependencies()[&api][0].service.as_str(),
        "web"
    );
}

#[test]
fn a_claim_carries_a_sealed_value_without_the_key_the_store_was_opened_with() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    set(&store, &[("web.env.TOKEN", sealed(3, "TOKEN", 1))]).unwrap();
    let id = admit(&store, 1).unwrap();
    let other = ConfigStore::open(&url, SealingKey::new(b"another secret").unwrap()).unwrap();
    let claimed = other
        .claim(&id, &RunnerId::parse("runner").unwrap())
        .unwrap();
    assert_eq!(
        environment_of(&claimed, "web")["TOKEN"],
        carried(3, "TOKEN", 1)
    );
}

#[test]
fn a_secret_is_kept_by_its_marker_and_never_becomes_plain() {
    let store = backend::open();
    shop(&store);
    set(&store, &[("web.env.TOKEN", sealed(3, "TOKEN", 1))]).unwrap();
    // Sending back what reads show keeps it, as does the same sealed value again.
    for same in [json!({ "secret": true }), sealed(3, "TOKEN", 1)] {
        assert!(staged(&set(&store, &[("web.env.TOKEN", same)]).unwrap()).is_empty());
    }
    let refused = set(&store, &[("web.env.TOKEN", json!("plain now"))]).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert!(refused.message.contains("never becomes plain text"));
    assert_eq!(value(&store, "web.env.TOKEN"), json!({ "secret": true }));
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
            ("web.env.PLAIN", sealed(3, "PLAIN", 2)),
            ("web.env.TOKEN", sealed(3, "TOKEN", 3)),
        ],
    )
    .unwrap();
    assert_eq!(staged(&edited), ["web.env.PLAIN", "web.env.TOKEN"]);
    for bad in [json!(null), json!(3), json!({ "secret": "" })] {
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
    // A variable web doesn't have is taken and reported broken; a built-in isn't broken.
    set(&store, &[("api.env.URL", json!("${{ web.HOST }}"))]).unwrap();
    let diff = store.read(&who(), &DiffQuery::default()).unwrap();
    let broken: Vec<String> = diff.broken.iter().map(ToString::to_string).collect();
    assert_eq!(
        broken,
        ["api.env.URL references web.HOST, which doesn't exist"]
    );
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
            ("web.env.KEY", sealed(3, "KEY", 1)),
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
fn a_renamed_secret_stays_sealed_and_keeps_its_value() {
    let store = backend::open();
    shop(&store);
    set(
        &store,
        &[
            ("web.env.TOKEN", sealed(3, "TOKEN", 1)),
            (
                "web.env.API_URL",
                json!({ "value": "https://api", "exported": true }),
            ),
        ],
    )
    .unwrap();
    let edited = rename(&store, &[], "web.env.TOKEN", "api_token").unwrap();
    assert_eq!(staged(&edited), ["web.env.API_TOKEN", "web.env.TOKEN"]);
    assert!(!json!(edited).to_string().contains(&backend::ciphertext(1)));
    assert_eq!(
        value(&store, "web.env.API_TOKEN"),
        json!({ "secret": true })
    );
    let gone = store
        .read(
            &who(),
            &EnvironmentQuery {
                path: Some(SettingPath::parse("web.env.TOKEN").unwrap()),
                ..EnvironmentQuery::default()
            },
        )
        .unwrap_err();
    assert_eq!(gone.code, RpcErrorCode::NotFound);
    // A retry, or a rename to its own name, changes nothing.
    assert!(staged(&rename(&store, &[], "web.env.TOKEN", "API_TOKEN").unwrap()).is_empty());
    assert!(staged(&rename(&store, &[], "web.env.API_TOKEN", "API_TOKEN").unwrap()).is_empty());
    // Exported stays exported.
    rename(&store, &[], "web.env.API_URL", "PUBLIC_API_URL").unwrap();
    assert_eq!(
        value(&store, "web.env.PUBLIC_API_URL.exported"),
        json!(true)
    );
    let id = admit(&store, 1).unwrap();
    let claimed = store
        .claim(&id, &RunnerId::parse("runner").unwrap())
        .unwrap();
    let web = environment_of(&claimed, "web");
    // Renamed, it reaches the Service under its new name, sealed for the old one.
    assert_eq!(
        web["API_TOKEN"],
        json!({ "template": [carried(3, "TOKEN", 1)] })
    );
    assert_eq!(web["PUBLIC_API_URL"], "https://api");
    assert!(web.get("TOKEN").is_none() && web.get("API_URL").is_none());
}

#[test]
fn sealed_names_keep_every_name_ever_sealed_and_every_template() {
    let store = backend::open();
    shop(&store);
    set(
        &store,
        &[
            ("web.env.TOKEN", sealed(3, "TOKEN", 1)),
            ("web.env.OLD", sealed(3, "OLD", 2)),
            ("web.env.PLAIN", json!("hello-plain")),
            ("api.env.URL", json!("http://${{ web.PLAIN }}")),
        ],
    )
    .unwrap();
    rename(&store, &[], "web.env.TOKEN", "API_TOKEN").unwrap();
    unset(&store, "web.env.OLD");

    let names = |store: &ConfigStore| {
        store
            .read(&who(), &SealedNamesQuery::default())
            .unwrap()
            .names
            .into_iter()
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&store), ["API_TOKEN", "OLD", "TOKEN", "URL"]);
    let id = admit(&store, 1).unwrap();
    let claimed = store
        .claim(&id, &RunnerId::parse("runner").unwrap())
        .unwrap();
    assert_eq!(
        claimed.sealed_names.into_iter().collect::<Vec<_>>(),
        ["API_TOKEN", "OLD", "TOKEN", "URL"]
    );
    assert!(
        !json!(store.read(&who(), &SealedNamesQuery::default()).unwrap())
            .to_string()
            .contains(&backend::ciphertext(1))
    );
}

#[test]
fn a_rename_onto_a_name_in_use_changes_neither() {
    let store = backend::open();
    shop(&store);
    set(
        &store,
        &[
            ("web.env.API_URL", json!("https://api")),
            ("web.env.LOG_LEVEL", json!("info")),
        ],
    )
    .unwrap();
    // All or none: the set before the refused rename does not land either.
    let refused = rename(
        &store,
        &[("web.env.OTHER", json!("set"))],
        "web.env.API_URL",
        "LOG_LEVEL",
    )
    .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(refused.details["variable"], "LOG_LEVEL");
    assert_eq!(value(&store, "web.env.API_URL"), json!("https://api"));
    assert_eq!(value(&store, "web.env.LOG_LEVEL"), json!("info"));
    assert!(
        get(&store, Some("web")).values.unwrap()["env"]
            .get("OTHER")
            .is_none()
    );

    let missing = rename(&store, &[], "web.env.API_UR", "OTHER").unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::NotFound);
    assert_eq!(missing.details["did_you_mean"], "API_URL");
    let reserved = rename(&store, &[], "web.env.API_URL", "PLOYZ_SERVICE_ID").unwrap_err();
    assert_eq!(reserved.code, RpcErrorCode::InvalidArgument);
    for (path, to) in [("web.replicas", "REPLICAS"), ("web.env.API_URL", "9LIVES")] {
        let error = rename(&store, &[], path, to).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{path} → {to}");
    }
    assert_eq!(value(&store, "web.env.API_URL"), json!("https://api"));
}

#[test]
fn a_rename_and_another_change_to_where_it_moves_from_or_to_are_refused() {
    let store = backend::open();
    shop(&store);
    set(&store, &[("web.env.A", json!("moved"))]).unwrap();
    let rename = |path: &str, to: &str| Change::Rename {
        path: SettingPath::parse(path).unwrap(),
        to: to.to_owned(),
        update_references: false,
    };
    let unset = |path: &str| Change::Unset {
        path: SettingPath::parse(path).unwrap(),
    };
    let set = |path: &str, value: Value| Change::Set {
        path: SettingPath::parse(path).unwrap(),
        value,
    };
    let before = get(&store, Some("web")).values;
    for (changes, path) in [
        (
            vec![rename("web.env.A", "B"), unset("web.env.B")],
            "web.env.B",
        ),
        (
            vec![rename("web.env.A", "B"), set("web.env.B", json!("other"))],
            "web.env.B",
        ),
        (
            vec![set("web.env.B", json!("other")), rename("web.env.A", "B")],
            "web.env.B",
        ),
        (
            vec![rename("web.env.A", "B"), rename("web.env.B", "C")],
            "web.env.B",
        ),
        (
            vec![rename("web.env.A", "B"), unset("web.env.A")],
            "web.env.A",
        ),
    ] {
        let refused = store
            .write(
                &who(),
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: changes.clone(),
                },
            )
            .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{changes:?}");
        assert_eq!(
            refused.message,
            format!("{path} is changed twice in one edit; change it once"),
            "{changes:?}"
        );
        assert_eq!(get(&store, Some("web")).values, before, "{changes:?}");
    }
}

#[test]
fn a_variable_object_with_exported_and_a_change_to_exported_are_refused() {
    let store = backend::open();
    shop(&store);
    set(&store, &[("web.env.X", json!("x"))]).unwrap();
    let set = |path: &str, value: Value| Change::Set {
        path: SettingPath::parse(path).unwrap(),
        value,
    };
    let unset = |path: &str| Change::Unset {
        path: SettingPath::parse(path).unwrap(),
    };
    let patch = |value: Value| Change::Patch {
        path: SettingPath::parse("web").unwrap(),
        value,
    };
    let edit = |changes: Vec<Change>| {
        store.write(
            &who(),
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes,
            },
        )
    };
    let before = get(&store, Some("web")).values;
    for changes in [
        vec![
            set("web.env.X", json!({ "exported": true })),
            set("web.env.X.exported", json!(false)),
        ],
        vec![
            set("web.env.X.exported", json!(false)),
            set("web.env.X", json!({ "value": "y", "exported": true })),
        ],
        vec![
            patch(json!({ "env": { "X": { "exported": true } } })),
            unset("web.env.X.exported"),
        ],
    ] {
        let refused = edit(changes.clone()).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{changes:?}");
        assert_eq!(
            refused.message, "web.env.X.exported is changed twice in one edit; change it once",
            "{changes:?}"
        );
        assert_eq!(get(&store, Some("web")).values, before, "{changes:?}");
    }

    // A value with no exported field leaves the flag to the other change.
    edit(vec![
        set("web.env.X", json!({ "value": "y" })),
        set("web.env.X.exported", json!(true)),
    ])
    .unwrap();
    assert_eq!(value(&store, "web.env.X"), json!("y"));
    assert_eq!(value(&store, "web.env.X.exported"), true);
}

/// Services `web`, `api` and `worker`, where `api` and `worker` reference web's
/// API_URL in both forms, beside text that only looks like it.
fn referenced(store: &ConfigStore) {
    shop(store);
    store
        .write(
            &who(),
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("worker").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        store,
        &[
            ("web.env.API_URL", json!("https://api")),
            ("api.env.API_URL", json!("https://own")),
            ("web.env.UPSTREAM", json!("${{ API_URL }}/v1")),
            (
                "worker.env.BACKEND",
                json!("${{ web.API_URL }} ${{ api.API_URL }}"),
            ),
            ("worker.env.LITERAL", json!("$${{ web.API_URL }} API_URL")),
            ("api.env.SELF", json!("${{ API_URL }}")),
            ("web.env.TOKEN", sealed(3, "TOKEN", 1)),
        ],
    )
    .unwrap();
}

#[test]
fn a_referenced_variable_is_renamed_only_with_its_references() {
    let store = backend::open();
    referenced(&store);
    let before = get(&store, None);

    let refused = rename(&store, &[], "web.env.API_URL", "PUBLIC_API_URL").unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(
        refused.details["variables"],
        json!(["web.UPSTREAM", "worker.BACKEND"])
    );
    let next = refused.details["next"].as_str().unwrap();
    assert!(
        next.starts_with("ployz set web.env.API_URL --rename PUBLIC_API_URL --update-references"),
        "{next}"
    );
    assert!(
        refused.message.contains("worker.BACKEND"),
        "{}",
        refused.message
    );
    assert_eq!(
        get(&store, None),
        before,
        "a refused rename changes nothing"
    );

    let edited = rename_with(&store, &[], "web.env.API_URL", "PUBLIC_API_URL", true).unwrap();
    assert_eq!(
        staged(&edited),
        [
            "web.env.PUBLIC_API_URL",
            "web.env.API_URL",
            "web.env.UPSTREAM",
            "worker.env.BACKEND"
        ]
    );
    assert_eq!(
        value(&store, "web.env.UPSTREAM"),
        json!("${{ PUBLIC_API_URL }}/v1")
    );
    assert_eq!(
        value(&store, "worker.env.BACKEND"),
        json!("${{ web.PUBLIC_API_URL }} ${{ api.API_URL }}")
    );
    // Text that only looks like a reference, and api's own API_URL, stay.
    assert_eq!(
        value(&store, "worker.env.LITERAL"),
        json!("$${{ web.API_URL }} API_URL")
    );
    assert_eq!(value(&store, "api.env.SELF"), json!("${{ API_URL }}"));
    assert_eq!(value(&store, "api.env.API_URL"), json!("https://own"));

    // A retry changes nothing.
    let retried = rename_with(&store, &[], "web.env.API_URL", "PUBLIC_API_URL", true).unwrap();
    assert!(staged(&retried).is_empty());
    assert_eq!(retried.environment.revision, edited.environment.revision);

    let id = admit(&store, 1).unwrap();
    let claimed = store
        .claim(&id, &RunnerId::parse("runner").unwrap())
        .unwrap();
    assert_eq!(
        environment_of(&claimed, "web")["UPSTREAM"],
        "https://api/v1"
    );
    assert_eq!(
        environment_of(&claimed, "worker")["BACKEND"],
        "https://api https://own"
    );
    // A secret holds a sealed value, never a reference: it is neither read nor rewritten.
    assert_eq!(
        environment_of(&claimed, "web")["TOKEN"],
        carried(3, "TOKEN", 1)
    );
}

#[test]
fn a_referenced_secret_is_renamed_and_stays_sealed() {
    let store = backend::open();
    shop(&store);
    set(
        &store,
        &[
            ("web.env.TOKEN", sealed(3, "TOKEN", 1)),
            ("api.env.AUTH", json!("Bearer ${{ web.TOKEN }}")),
        ],
    )
    .unwrap();
    let edited = rename_with(&store, &[], "web.env.TOKEN", "API_TOKEN", true).unwrap();
    assert_eq!(
        staged(&edited),
        ["web.env.API_TOKEN", "web.env.TOKEN", "api.env.AUTH"]
    );
    assert!(!json!(edited).to_string().contains(&backend::ciphertext(1)));
    assert_eq!(
        value(&store, "web.env.API_TOKEN"),
        json!({ "secret": true })
    );
    assert_eq!(
        value(&store, "api.env.AUTH"),
        json!("Bearer ${{ web.API_TOKEN }}")
    );
    let id = admit(&store, 1).unwrap();
    let claimed = store
        .claim(&id, &RunnerId::parse("runner").unwrap())
        .unwrap();
    assert_eq!(
        environment_of(&claimed, "web")["API_TOKEN"],
        json!({ "template": [carried(3, "TOKEN", 1)] })
    );
    assert_eq!(
        environment_of(&claimed, "api")["AUTH"],
        json!({ "template": ["Bearer ", carried(3, "TOKEN", 1)] })
    );
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

#[test]
fn a_config_file_never_takes_a_sealed_value_and_warns_of_what_may_break_it() {
    let store = backend::open();
    shop(&store);
    set(
        &store,
        &[
            ("web.env.PASSWORD", sealed(3, "PASSWORD", 1)),
            ("web.env.HOSTS", json!("a\nb")),
        ],
    )
    .unwrap();
    store
        .write(
            &who(),
            &CreateConfig {
                id: ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("sentry").unwrap(),
                mounts: vec![ConfigMountAt {
                    service: ServiceName::parse("api").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    let put = |content: &str| {
        store
            .write(
                &who(),
                &PutConfigFile {
                    environment: EnvironmentRef::default(),
                    config: ConfigName::parse("sentry").unwrap(),
                    file: ConfigFileName::parse("config.yml").unwrap(),
                    content: content.into(),
                    mode: None,
                    uid: None,
                    gid: None,
                },
            )
            .unwrap();
    };
    // A file can't carry a sealed value: the Store can't open it, and the file is
    // rendered here. The refusal says which file reads which variable, never the value.
    put("password: \"${{ web.PASSWORD }}\"\nhosts: ${{ web.HOSTS }}\nraw: $${{ web.PASSWORD }}\n");
    let configs = store
        .read(
            &who(),
            &ConfigsQuery {
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap();
    assert!(
        !json!(configs).to_string().contains(&backend::ciphertext(1)),
        "{configs:?}"
    );
    let error = admit(&store, 1).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
    assert!(error.message.contains("sentry"), "{error:?}");
    assert!(error.message.contains("config.yml"), "{error:?}");
    assert!(error.message.contains("reads web.PASSWORD"), "{error:?}");
    assert!(
        !format!("{error:?}").contains(&backend::ciphertext(1)),
        "{error:?}"
    );

    put("hosts: ${{ web.HOSTS }}\nraw: $${{ web.PASSWORD }}\n");
    let id = admit(&store, 2).unwrap();
    let claimed = store
        .claim(&id, &RunnerId::parse("runner").unwrap())
        .unwrap();
    let api = claimed
        .intent
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "api")
        .unwrap();
    let config = api.configs().first().unwrap();
    assert_eq!(config.name, "sentry/config.yml");
    assert_eq!(
        String::from_utf8(config.content.clone()).unwrap(),
        "hosts: a\nb\nraw: ${{ web.PASSWORD }}\n"
    );
    let api_name = ServiceName::parse("api").unwrap();
    assert_eq!(
        claimed.intent.dependencies()[&api_name][0].service.as_str(),
        "web"
    );
    let warnings = json!(claimed.deployment.warnings);
    assert_eq!(
        warnings,
        json!([{ "config": "sentry", "file": "config.yml", "variable": "web.HOSTS" }])
    );
    let read = store
        .read(&who(), &ployz_store::DeploymentQuery { id })
        .unwrap();
    assert_eq!(json!(read.deployment.warnings), warnings);
    assert!(!json!(read).to_string().contains(&backend::ciphertext(1)));
}

#[test]
fn a_deploy_leaves_out_what_reads_a_broken_reference_and_warns() {
    let store = backend::open();
    shop(&store);
    set(
        &store,
        &[
            ("web.env.PASS", json!("pw")),
            ("api.env.DB", json!("${{ web.PASS }}")),
            ("api.env.URL", json!("db://${{ DB }}")),
            ("api.env.MODE", json!("fast")),
        ],
    )
    .unwrap();
    store
        .write(
            &who(),
            &CreateConfig {
                id: ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("sentry").unwrap(),
                mounts: vec![ConfigMountAt {
                    service: ServiceName::parse("api").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    store
        .write(
            &who(),
            &PutConfigFile {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("sentry").unwrap(),
                file: ConfigFileName::parse("config.yml").unwrap(),
                content: "pass: ${{ web.PASS }}\nmode: ${{ api.MODE }}\nurl: ${{ api.URL }}\n"
                    .into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
    unset(&store, "web.env.PASS");
    let id = admit(&store, 1).unwrap();
    let claimed = store
        .claim(&id, &RunnerId::parse("runner").unwrap())
        .unwrap();
    let api = environment_of(&claimed, "api");
    assert_eq!(api["MODE"], json!("fast"), "{api}");
    assert!(api.get("DB").is_none() && api.get("URL").is_none(), "{api}");
    let spec = claimed
        .intent
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "api")
        .unwrap();
    let config = spec.configs().first().unwrap();
    assert_eq!(
        String::from_utf8(config.content.clone()).unwrap(),
        "pass: \nmode: fast\nurl: \n"
    );
    let warnings = json!(claimed.deployment.warnings);
    assert_eq!(
        warnings,
        json!([
            { "service": "api", "variable": "DB", "broken": true },
            { "service": "api", "variable": "URL", "broken": true },
            { "config": "sentry", "file": "config.yml", "secret": "web.PASS", "broken": true },
            { "config": "sentry", "file": "config.yml", "secret": "api.URL", "broken": true },
        ])
    );
    let before: Vec<WarningBefore> = serde_json::from_value(warnings).unwrap();
    assert_eq!(
        before.iter().map(WarningBefore::text).collect::<Vec<_>>(),
        [
            "DB not set in api",
            "URL not set in api",
            "web.PASS not set in sentry/config.yml",
            "api.URL not set in sentry/config.yml",
        ]
    );
}

#[test]
fn editing_a_broken_variable_keeps_its_reference() {
    let store = backend::open();
    shop(&store);
    set(
        &store,
        &[
            ("web.env.PASS", json!("pw")),
            ("api.env.URL", json!("db://${{ web.PASS }}")),
        ],
    )
    .unwrap();
    store
        .write(
            &who(),
            &ployz_store::RemoveService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        value(&store, "api.env.URL"),
        json!("db://${{ <deleted web>.PASS }}")
    );
    set(
        &store,
        &[("api.env.URL", json!("pg://${{ <deleted web>.PASS }}"))],
    )
    .unwrap();
    assert_eq!(
        value(&store, "api.env.URL"),
        json!("pg://${{ <deleted web>.PASS }}")
    );
    let refused = set(
        &store,
        &[("api.env.URL", json!("${{ <deleted web>.OTHER }}"))],
    )
    .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    let diff = store
        .read(
            &who(),
            &DiffQuery {
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap();
    let broken: Vec<String> = diff.broken.iter().map(ToString::to_string).collect();
    assert_eq!(broken, ["api.env.URL references web, which was removed"]);
}

#[test]
fn removed_services_shown_alike_are_told_apart_by_number() {
    let store = backend::open();
    shop(&store);
    let remove = |name: &str| {
        store
            .write(
                &who(),
                &ployz_store::RemoveService {
                    environment: EnvironmentRef::default(),
                    service: ServiceName::parse(name).unwrap(),
                },
            )
            .unwrap();
    };
    set(
        &store,
        &[
            ("web.env.URL", json!("w")),
            ("api.env.HOSTS", json!("${{ web.URL }}")),
        ],
    )
    .unwrap();
    remove("web");
    store
        .write(
            &who(),
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        &store,
        &[
            ("web.env.URL", json!("w")),
            (
                "api.env.HOSTS",
                json!("${{ <deleted web>.URL }},${{ web.URL }}"),
            ),
        ],
    )
    .unwrap();
    remove("web");
    assert_eq!(
        value(&store, "api.env.HOSTS"),
        json!("${{ <deleted web>.URL }},${{ <deleted web#2>.URL }}")
    );
    set(
        &store,
        &[("api.env.HOSTS", json!("${{ <deleted web#2>.URL }}"))],
    )
    .unwrap();
    assert_eq!(
        value(&store, "api.env.HOSTS"),
        json!("${{ <deleted web#2>.URL }}")
    );
}

/// `web` removed twice, `web.env.FIRST` referencing the first and `api.env.SECOND`
/// the second.
fn two_removed_webs(store: &ConfigStore) {
    let remove = || {
        store
            .write(
                &who(),
                &ployz_store::RemoveService {
                    environment: EnvironmentRef::default(),
                    service: ServiceName::parse("web").unwrap(),
                },
            )
            .unwrap();
    };
    set(
        store,
        &[
            ("web.env.URL", json!("w")),
            ("api.env.FIRST", json!("${{ web.URL }}")),
        ],
    )
    .unwrap();
    remove();
    store
        .write(
            &who(),
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        store,
        &[
            ("web.env.URL", json!("w")),
            ("api.env.SECOND", json!("${{ web.URL }}")),
        ],
    )
    .unwrap();
    remove();
}

#[test]
fn a_bulk_edit_dropping_one_removed_service_keeps_the_others_number() {
    let store = backend::open();
    shop(&store);
    two_removed_webs(&store);
    assert_eq!(
        value(&store, "api.env.SECOND"),
        json!("${{ <deleted web#2>.URL }}")
    );

    set(
        &store,
        &[
            ("api.env.FIRST", json!("plain")),
            ("api.env.SECOND", json!("${{ <deleted web#2>.URL }}/v2")),
        ],
    )
    .unwrap();
    assert_eq!(
        value(&store, "api.env.SECOND"),
        json!("${{ <deleted web#2>.URL }}/v2")
    );
}

#[test]
fn discard_brings_a_reference_back_on_the_removed_service_it_read() {
    let store = backend::open();
    shop(&store);
    two_removed_webs(&store);
    store
        .write_trusted(
            &who(),
            &ployz_store::Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &ployz_store::Trusted {
                approval: ployz_store::Approval::NotRequired,
                ..ployz_store::Trusted::default()
            },
        )
        .unwrap();
    set(
        &store,
        &[
            ("api.env.FIRST", json!("plain")),
            ("api.env.SECOND", json!("plain")),
        ],
    )
    .unwrap();

    store
        .write(
            &who(),
            &ployz_store::Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Saved),
                path: None,
                version: None,
            },
        )
        .unwrap();
    assert_eq!(
        (
            value(&store, "api.env.FIRST"),
            value(&store, "api.env.SECOND")
        ),
        (
            json!("${{ <deleted web>.URL }}"),
            json!("${{ <deleted web#2>.URL }}")
        )
    );
}

#[test]
fn an_edit_keeps_each_reference_on_the_removed_service_it_names() {
    let store = backend::open();
    shop(&store);
    store
        .write(
            &who(),
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("jobs").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        &store,
        &[
            ("web.env.URL", json!("w")),
            ("api.env.URL", json!("a")),
            ("jobs.env.HOSTS", json!("${{ web.URL }},${{ api.URL }}")),
        ],
    )
    .unwrap();
    store
        .write(
            &who(),
            &CreateConfig {
                id: ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("db").unwrap(),
                mounts: vec![ConfigMountAt {
                    service: ServiceName::parse("jobs").unwrap(),
                    dir: "/etc/db".into(),
                }],
            },
        )
        .unwrap();
    let put = |content: &str| {
        store.write(
            &who(),
            &PutConfigFile {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("db").unwrap(),
                file: ConfigFileName::parse("db.yml").unwrap(),
                content: content.into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
    };
    put("primary: ${{ web.URL }}\nreplica: ${{ api.URL }}").unwrap();
    admit(&store, 1).unwrap();
    for gone in ["web", "api"] {
        store
            .write(
                &who(),
                &ployz_store::RemoveService {
                    environment: EnvironmentRef::default(),
                    service: ServiceName::parse(gone).unwrap(),
                },
            )
            .unwrap();
    }
    let file = || {
        store
            .read(
                &who(),
                &ployz_store::ConfigItemQuery {
                    environment: EnvironmentRef::default(),
                    config: ConfigName::parse("db").unwrap().into(),
                },
            )
            .unwrap()
            .contents[&ConfigFileName::parse("db.yml").unwrap()]
            .clone()
    };

    assert_eq!(
        value(&store, "jobs.env.HOSTS"),
        json!("${{ <deleted web>.URL }},${{ <deleted api>.URL }}")
    );
    set(
        &store,
        &[("jobs.env.HOSTS", json!("${{ <deleted api>.URL }}"))],
    )
    .unwrap();
    assert_eq!(
        value(&store, "jobs.env.HOSTS"),
        json!("${{ <deleted api>.URL }}")
    );
    let invented = set(
        &store,
        &[("jobs.env.HOSTS", json!("${{ <deleted web>.URL }}"))],
    );
    assert_eq!(invented.unwrap_err().code, RpcErrorCode::InvalidArgument);

    assert_eq!(
        file(),
        "primary: ${{ <deleted web>.URL }}\nreplica: ${{ <deleted api>.URL }}"
    );
    put("replica: ${{ <deleted api>.URL }}").unwrap();
    assert_eq!(file(), "replica: ${{ <deleted api>.URL }}");
    let invented = put("primary: ${{ <deleted web>.URL }}");
    assert_eq!(invented.unwrap_err().code, RpcErrorCode::InvalidArgument);
}

#[test]
fn a_service_removed_before_names_were_recorded_shows_by_its_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    shop(&store);
    store
        .write(
            &who(),
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("jobs").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        &store,
        &[
            ("web.env.URL", json!("w")),
            ("jobs.env.URL", json!("j")),
            ("api.env.FIRST", json!("${{ web.URL }}")),
            ("api.env.SECOND", json!("${{ jobs.URL }}")),
        ],
    )
    .unwrap();
    store
        .write_trusted(
            &who(),
            &ployz_store::Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &ployz_store::Trusted {
                approval: ployz_store::Approval::NotRequired,
                ..ployz_store::Trusted::default()
            },
        )
        .unwrap();
    let remove_both = || {
        for gone in ["web", "jobs"] {
            store
                .write(
                    &who(),
                    &ployz_store::RemoveService {
                        environment: EnvironmentRef::default(),
                        service: ServiceName::parse(gone).unwrap(),
                    },
                )
                .unwrap();
        }
    };
    remove_both();
    execute(&url, "UPDATE config_environment SET removed = '{}'");
    set(&store, &[("api.env.OTHER", json!("x"))]).unwrap();
    let shown = || {
        (
            value(&store, "api.env.FIRST"),
            value(&store, "api.env.SECOND"),
        )
    };
    let nameless = (
        json!("${{ <deleted #00000003>.URL }}"),
        json!("${{ <deleted #00000005>.URL }}"),
    );
    assert_eq!(shown(), nameless);

    set(
        &store,
        &[("api.env.SECOND", json!("${{ <deleted #00000005>.URL }}/v2"))],
    )
    .unwrap();
    assert_eq!(
        shown(),
        (nameless.0, json!("${{ <deleted #00000005>.URL }}/v2"))
    );
}

/// Run `sql` on the database at `url`, beside the Store.
fn execute(url: &str, sql: &str) {
    match url.strip_prefix("sqlite:") {
        Some(path) => {
            rusqlite::Connection::open(path)
                .unwrap()
                .execute(sql, [])
                .unwrap();
        }
        None => {
            postgres::Client::connect(url, postgres::NoTls)
                .unwrap()
                .execute(sql, &[])
                .unwrap();
        }
    }
}

/// `DeploymentWarning` before broken references, as older clients still read it.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum WarningBefore {
    Fragile {
        config: String,
        file: String,
        variable: String,
    },
    NotSet {
        service: String,
        variable: String,
    },
    NotSetInFile {
        config: String,
        file: String,
        secret: String,
    },
    IngressNotServing {
        server: String,
        ingress: Value,
    },
    IngressUnchecked {
        cause: String,
    },
}

impl WarningBefore {
    fn text(&self) -> String {
        match self {
            Self::Fragile {
                config,
                file,
                variable,
            } => format!("{variable} may break {config}/{file}"),
            Self::NotSet { service, variable } => format!("{variable} not set in {service}"),
            Self::NotSetInFile {
                config,
                file,
                secret,
            } => format!("{secret} not set in {config}/{file}"),
            Self::IngressNotServing { server, ingress } => format!("{server}: {ingress}"),
            Self::IngressUnchecked { cause } => cause.clone(),
        }
    }
}

#[test]
fn a_config_file_reference_still_blocks_a_rename() {
    let store = backend::open();
    shop(&store);
    set(&store, &[("web.env.HOSTS", json!("a"))]).unwrap();
    store
        .write(
            &who(),
            &CreateConfig {
                id: ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("sentry").unwrap(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
    store
        .write(
            &who(),
            &PutConfigFile {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("sentry").unwrap(),
                file: ConfigFileName::parse("config.yml").unwrap(),
                content: "hosts: ${{ web.HOSTS }}\n".into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
    let refused = rename_with(&store, &[], "web.env.HOSTS", "ALL_HOSTS", true).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert!(refused.message.contains("sentry"), "{}", refused.message);
    assert_eq!(value(&store, "web.env.HOSTS"), json!("a"));
}
