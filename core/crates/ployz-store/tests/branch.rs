#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Branches through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): Own Copies and Live Nodes, Setup Commands, Update, Own Copy of a
//! Live Node, Keep and fixing a failed Deployment. None of it needs a Server until
//! something deploys.

use ployz_core::{DeployOutcome, DeployPreview, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AddDomain, Admit, Branched, Change, ConfigStore, CopyNode, CreateBranch, CreateProject,
    CreateService, CreateVolume, Deploy, DeploymentId, DiffQuery, DomainName, DomainsQuery, Edit,
    EnvironmentId, EnvironmentName, EnvironmentRef, KeepBranch, LiveNode, Mount, Move, MoveQuery,
    OrganizationId, PickChoice, ProjectId, ProjectName, Removal, RunEvidence, RunnerId, Save,
    ServiceId, ServiceQuery, SettingPath, SetupCommand, Trusted, Update, VolumeId, VolumeName,
};
use serde_json::{Value, json};

mod backend;

/// Items as their text, to compare with literals.
fn texts<T: ToString>(items: &[T]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

/// A node by its name: `SERVICE`, or `volumes.VOLUME`.
fn node(name: &str) -> ployz_store::NodeName {
    ployz_store::NodeName::parse(name).unwrap()
}

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

/// production: `web` (a secret, a reference to `db`, a private image with a
/// registry credential) uses `db`, which mounts Volume `data`. Nothing deployed.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .create_project(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    for (n, name, image) in [(3, "web", "web:1"), (4, "db", "postgres:17")] {
        store
            .create_service(
                &who,
                &CreateService {
                    id: ServiceId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some(image.into()),
                },
            )
            .unwrap();
    }
    store
        .create_volume(
            &who,
            &CreateVolume {
                id: VolumeId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("db").unwrap(),
                    path: "/var/lib/postgresql".into(),
                }],
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "production",
        &[
            ("web.env.TOKEN", json!({ "secret": "s3cret" })),
            ("web.env.DB_URL", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}")),
            (
                "web.registryCredential",
                json!({ "username": "bot", "secret": "pull-token" }),
            ),
        ],
    );
    (store, who)
}

fn set(store: &ConfigStore, who: &Actor, environment: &str, changes: &[(&str, Value)]) {
    store
        .edit(
            who,
            &Edit {
                environment: at(environment),
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
        .unwrap();
}

fn branch(name: &str, from: &str, copy: &[&str]) -> CreateBranch {
    CreateBranch {
        id: EnvironmentId::parse(
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).to_string(),
        )
        .unwrap(),
        from: at(from),
        name: EnvironmentName::parse(name).unwrap(),
        copy: copy.iter().map(|name| node(name)).collect(),
        live: Vec::new(),
        setup: Vec::new(),
        keep: false,
        fix: None,
    }
}

fn services(store: &ConfigStore, who: &Actor, environment: &str) -> Vec<(String, String)> {
    store
        .services(
            who,
            &ployz_store::ServicesQuery {
                environment: at(environment),
            },
        )
        .unwrap()
        .services
        .into_iter()
        .map(|listing| {
            (
                listing.service.name.to_string(),
                listing.service.id.to_string(),
            )
        })
        .collect()
}

fn values(store: &ConfigStore, who: &Actor, environment: &str, service: &str) -> Value {
    Value::Object(
        store
            .service(
                who,
                &ServiceQuery {
                    environment: at(environment),
                    service: ServiceName::parse(service).unwrap(),
                },
            )
            .unwrap()
            .values,
    )
}

/// Deploy `environment` in full and record `outcome` for it: every Service applied,
/// or nothing executed.
fn deploy(store: &ConfigStore, who: &Actor, environment: &str, n: u8, applied: bool) -> Value {
    let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap();
    store
        .admit(
            who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: at(environment),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
            }),
            &Trusted::default(),
        )
        .unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id, &runner).unwrap();
    if !applied {
        store
            .record(&id, &runner, RunEvidence::NotExecuted("no Servers".into()))
            .unwrap();
        return claimed.input;
    }
    let names: Vec<String> = claimed
        .intent
        .target
        .iter()
        .map(|service| service.name.to_string())
        .collect();
    let operation = |index: usize| {
        json!({"type": "remove_container", "machine_id": "a".repeat(32),
               "container_id": format!("{index:x}").repeat(64)})
    };
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace,
        "operations": names.iter().enumerate().map(|(index, name)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": name,
            "operation": operation(index), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(&id, &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: DeployOutcome<ployz_core::ExecutionError> = serde_json::from_value(json!({
        "type": "success",
        "completed": (0..names.len()).map(operation).collect::<Vec<_>>()
    }))
    .unwrap();
    store
        .record(
            &id,
            &runner,
            RunEvidence::Executed {
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
    claimed.input
}

/// A Service's resolved environment and Setup Commands in a claimed lowering input.
fn snapshot<'input>(input: &'input Value, service_id: &str) -> &'input Value {
    input["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|snapshot| snapshot["serviceId"] == service_id)
        .unwrap()
}

fn id_of(store: &ConfigStore, who: &Actor, environment: &str, service: &str) -> String {
    services(store, who, environment)
        .into_iter()
        .find(|(name, _)| name == service)
        .unwrap()
        .1
}

fn code<T: std::fmt::Debug>(result: Result<T, ployz_core::RpcError>) -> RpcErrorCode {
    result.unwrap_err().code
}

#[test]
fn a_branch_of_an_undeployed_parent_copies_what_it_uses_with_secrets_and_credentials() {
    let (store, who) = shop();
    let mut create = branch("fix-web", "production", &["web"]);
    create.keep = true;
    create.setup = vec![SetupCommand {
        service: ServiceName::parse("web").unwrap(),
        command: "  pnpm db:seed ".into(),
    }];
    let made = store.create_branch(&who, &create).unwrap();
    // A replay returns the same; the ID with another body is a conflict.
    assert_eq!(store.create_branch(&who, &create).unwrap(), made);
    let mut other = create.clone();
    other.keep = false;
    assert_eq!(
        code(store.create_branch(&who, &other)),
        RpcErrorCode::Conflict
    );

    // The Parent runs nothing, so web brings db and db its Volume, all under fresh ids.
    assert_eq!(texts(&made.staged), ["db", "web", "volumes.data"]);
    let copied = services(&store, &who, "fix-web");
    assert_eq!(
        copied
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["db", "web"]
    );
    assert!(
        copied
            .iter()
            .all(|(_, id)| *id != uuid(3) && *id != uuid(4))
    );
    let web = store
        .service(
            &who,
            &ServiceQuery {
                environment: at("fix-web"),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(web.lineage.to_string(), uuid(3));
    assert_eq!(
        web.values["env"],
        json!({ "DB_URL": "${{ db.PLOYZ_PRIVATE_DOMAIN }}", "TOKEN": { "secret": true } })
    );
    assert_eq!(web.values["registryCredential"], json!({ "secret": true }));
    assert_eq!(
        values(&store, &who, "fix-web", "db")["mounts"],
        json!({ "data": "/var/lib/postgresql" })
    );
    assert_eq!(
        made.branch.setup,
        [SetupCommand {
            service: ServiceName::parse("web").unwrap(),
            command: "pnpm db:seed".into()
        }]
    );
    assert!(made.branch.kept && made.branch.live.is_empty() && made.branch.update.is_empty());
    assert_eq!(made.branch.parent.as_str(), "production");
    // Each copy has its Node Introduction: all three are created by the next Deploy.
    let diff = store
        .diff(
            &who,
            &DiffQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap();
    assert_eq!(diff.changes.len(), 3);

    // Deploying it carries the sealed secret, the credential and web's Setup Command.
    let input = deploy(&store, &who, "fix-web", 1, false);
    let web_id = id_of(&store, &who, "fix-web", "web");
    let db_id = id_of(&store, &who, "fix-web", "db");
    assert_eq!(snapshot(&input, &web_id)["resolvedEnv"]["TOKEN"], "s3cret");
    assert_eq!(
        snapshot(&input, &web_id)["resolvedEnv"]["DB_URL"],
        "db.internal"
    );
    assert_eq!(
        snapshot(&input, &web_id)["setupCommands"],
        json!(["pnpm db:seed"])
    );
    assert_eq!(snapshot(&input, &db_id)["setupCommands"], json!([]));
    // Setup Commands run until the copy first deploys.
    let input = deploy(&store, &who, "fix-web", 2, true);
    assert_eq!(
        snapshot(&input, &web_id)["setupCommands"],
        json!(["pnpm db:seed"])
    );
    set(&store, &who, "fix-web", &[("web.replicas", json!(2))]);
    let input = deploy(&store, &who, "fix-web", 3, true);
    assert_eq!(snapshot(&input, &web_id)["setupCommands"], json!([]));
}

#[test]
fn a_branch_is_refused_before_anything_is_written() {
    let (store, who) = shop();
    store
        .create_branch(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    let mut taken = branch("fix-web", "production", &["web"]);
    taken.id = EnvironmentId::parse(uuid(90)).unwrap();
    assert_eq!(
        code(store.create_branch(&who, &taken)),
        RpcErrorCode::Conflict
    );
    assert_eq!(
        code(store.create_branch(&who, &branch("empty", "production", &[]))),
        RpcErrorCode::InvalidArgument
    );
    assert_eq!(
        code(store.create_branch(&who, &branch("stray", "production", &["nope"]))),
        RpcErrorCode::NotFound
    );
    let mut stray = branch("stray", "production", &["db"]);
    stray.setup = vec![SetupCommand {
        service: ServiceName::parse("web").unwrap(),
        command: "seed".into(),
    }];
    assert_eq!(
        code(store.create_branch(&who, &stray)),
        RpcErrorCode::InvalidArgument
    );
    stray.setup = vec![SetupCommand {
        service: ServiceName::parse("db").unwrap(),
        command: "x".repeat(2001),
    }];
    assert_eq!(
        code(store.create_branch(&who, &stray)),
        RpcErrorCode::InvalidArgument
    );
    // Only a Branch updates, copies or is kept.
    let keep = KeepBranch {
        environment: at("production"),
        kept: true,
    };
    assert_eq!(
        code(store.keep_branch(&who, &keep)),
        RpcErrorCode::InvalidArgument
    );
    assert!(
        store
            .branch(
                &who,
                &ployz_store::BranchQuery {
                    environment: at("stray")
                }
            )
            .is_err()
    );
}

#[test]
fn a_branch_uses_what_its_parent_runs_live_down_the_tree() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    let made = store.create_branch(&who, &create).unwrap();
    assert_eq!(texts(&made.staged), ["web"]);
    let production = EnvironmentName::parse("production").unwrap();
    assert_eq!(
        made.branch.live,
        // db mounts the data Volume: the Branch writes production's real data.
        [LiveNode {
            name: "db".into(),
            owner: Some(production.clone()),
            data: true,
        }]
    );
    // A live reference reads, and is written, by the name where it runs.
    assert_eq!(
        values(&store, &who, "fix-web", "web")["env"]["DB_URL"],
        "${{ db.PLOYZ_PRIVATE_DOMAIN }}"
    );
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.DB_HOST", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    let mut wrong = branch("other", "production", &["web"]);
    wrong.live = vec![node("volumes.data")];
    assert_eq!(
        code(store.create_branch(&who, &wrong)),
        RpcErrorCode::InvalidArgument
    );

    // Its deploy reads db at production's address; a Branch of the Branch too.
    let input = deploy(&store, &who, "fix-web", 2, true);
    let web = snapshot(&input, &id_of(&store, &who, "fix-web", "web"));
    assert_eq!(web["resolvedEnv"]["DB_URL"], "db.shop-production.internal");
    assert_eq!(web["resolvedEnv"]["DB_HOST"], "db.shop-production.internal");
    let child = store
        .create_branch(&who, &branch("child", "fix-web", &["web"]))
        .unwrap();
    assert_eq!(child.branch.live[0].owner, Some(production));
    let input = deploy(&store, &who, "child", 3, false);
    let web = snapshot(&input, &id_of(&store, &who, "child", "web"));
    assert_eq!(web["resolvedEnv"]["DB_URL"], "db.shop-production.internal");
}

#[test]
fn update_stages_the_parents_deployed_changes_once_the_branch_runs_its_working_state() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, true);
    store
        .create_branch(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    let update = |version: Option<String>| {
        Move::Update(Update {
            into: at("fix-web"),
            picks: None,
            version,
        })
    };
    // Never deployed: it doesn't run its Working State yet.
    assert_eq!(
        code(store.move_changes(&who, &update(None))),
        RpcErrorCode::Conflict
    );
    deploy(&store, &who, "fix-web", 2, true);
    assert_eq!(
        store.move_changes(&who, &update(None)).unwrap_err().message,
        "Nothing new in production"
    );

    set(
        &store,
        &who,
        "production",
        &[("web.image", json!("web:2")), ("web.env.NEW", json!("1"))],
    );
    // Staged in the Parent isn't deployed: nothing to take yet.
    let view = |store: &ConfigStore| {
        store
            .branch(
                &who,
                &ployz_store::BranchQuery {
                    environment: at("fix-web"),
                },
            )
            .unwrap()
    };
    assert!(view(&store).update.is_empty());
    deploy(&store, &who, "production", 3, true);
    let mut pending = view(&store).update;
    pending.sort();
    assert_eq!(pending, ["web.env.NEW", "web.image"]);

    let review = store
        .move_view(
            &who,
            &MoveQuery::Update {
                into: at("fix-web"),
            },
        )
        .unwrap();
    assert_eq!(review.from.name.as_str(), "production");
    let stale = update(Some("0:0".into()));
    assert_eq!(
        code(store.move_changes(&who, &stale)),
        RpcErrorCode::Conflict
    );
    let reviewed = update(Some(review.version));
    assert_eq!(
        store
            .move_changes(&who, &reviewed)
            .unwrap()
            .into
            .name
            .as_str(),
        "fix-web"
    );
    let web = values(&store, &who, "fix-web", "web");
    assert_eq!(
        (web["image"].clone(), web["env"]["NEW"].clone()),
        (json!("web:2"), json!("1"))
    );
    // The base advanced: nothing left, and the change waits for a Deploy.
    assert!(view(&store).update.is_empty());
    assert_eq!(
        code(store.move_changes(&who, &update(None))),
        RpcErrorCode::Conflict
    );
    deploy(&store, &who, "fix-web", 4, true);
    assert_eq!(
        store.move_changes(&who, &update(None)).unwrap_err().message,
        "Nothing new in production"
    );

    // Refused while a Deployment runs, and after one failed.
    set(&store, &who, "fix-web", &[("web.replicas", json!(3))]);
    deploy(&store, &who, "fix-web", 5, false);
    assert!(
        store
            .move_changes(&who, &update(None))
            .unwrap_err()
            .message
            .contains("aren't deployed")
    );
}

#[test]
fn an_own_copy_of_a_live_node_brings_an_empty_copy_of_its_volume() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, true);
    store
        .create_branch(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    deploy(&store, &who, "fix-web", 2, true);
    let copy = CopyNode {
        environment: at("fix-web"),
        node: ServiceName::parse("db").unwrap(),
        expect: None,
    };
    let copied: Branched = store.copy_node(&who, &copy).unwrap();
    assert_eq!(texts(&copied.staged), ["db", "volumes.data"]);
    assert!(copied.branch.live.is_empty());
    let db = id_of(&store, &who, "fix-web", "db");
    assert_ne!(db, uuid(4));
    assert_eq!(
        values(&store, &who, "fix-web", "db")["mounts"],
        json!({ "data": "/var/lib/postgresql" })
    );
    // The copy is new to the Branch: its next Deploy creates it and the Volume.
    let diff = store
        .diff(
            &who,
            &DiffQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap();
    assert_eq!(diff.changes.len(), 2);
    assert_eq!(code(store.copy_node(&who, &copy)), RpcErrorCode::Conflict);
    // Its references now resolve inside the Branch.
    let input = deploy(&store, &who, "fix-web", 3, true);
    let web = snapshot(&input, &id_of(&store, &who, "fix-web", "web"));
    assert_eq!(web["resolvedEnv"]["DB_URL"], "db.internal");
    assert!(
        store
            .branch(
                &who,
                &ployz_store::BranchQuery {
                    environment: at("fix-web")
                }
            )
            .unwrap()
            .update
            .is_empty()
    );
}

#[test]
fn a_failed_deployment_is_fixed_on_a_branch_and_the_parent_stays() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, true);
    set(&store, &who, "production", &[("web.image", json!("web:2"))]);
    deploy(&store, &who, "production", 2, false);
    set(&store, &who, "production", &[("web.image", json!("web:3"))]);
    let failed = DeploymentId::parse("00000000-0000-4000-8000-000000000102").unwrap();
    let applied = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();

    // Only a failed Deployment, and only when a Service it failed gets its own copy.
    let mut fix = branch("fix", "production", &[]);
    fix.fix = Some(applied);
    assert_eq!(
        code(store.create_branch(&who, &fix)),
        RpcErrorCode::InvalidArgument
    );
    fix.fix = Some(failed.clone());
    fix.copy = vec![node("volumes.data")];
    assert_eq!(
        code(store.create_branch(&who, &fix)),
        RpcErrorCode::InvalidArgument
    );

    // Named none: it copies the Services the Deployment didn't apply.
    fix.copy = Vec::new();
    let made = store.create_branch(&who, &fix).unwrap();
    assert_eq!(texts(&made.staged), ["web"]);
    assert_eq!(values(&store, &who, "fix", "web")["image"], "web:2");
    assert_eq!(values(&store, &who, "production", "web")["image"], "web:3");
}

#[test]
fn keep_applies_at_once_and_generated_domains_follow_the_branch_name() {
    let (store, who) = shop();
    store
        .add_domain(
            &who,
            &AddDomain {
                environment: at("production"),
                service: ServiceName::parse("web").unwrap(),
                hostname: None,
                port: None,
            },
            &Trusted::default(),
        )
        .unwrap();
    store
        .create_branch(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    let child = store
        .create_branch(&who, &branch("child", "fix-web", &["web"]))
        .unwrap();
    assert_eq!(child.branch.parent.as_str(), "fix-web");
    let prefix = |environment: &str| {
        let domains = store
            .domains(
                &who,
                &DomainsQuery {
                    environment: at(environment),
                    service: None,
                },
                &Trusted::default(),
            )
            .unwrap()
            .domains;
        match &domains[0].domain.name {
            DomainName::Generated { prefix, .. } => prefix.clone(),
            DomainName::Custom { hostname } => hostname.to_string(),
        }
    };
    assert_eq!(prefix("fix-web"), "web-fix-web");
    assert_eq!(prefix("child"), "web-child");

    let keep = |kept| KeepBranch {
        environment: at("child"),
        kept,
    };
    let kept = store.keep_branch(&who, &keep(true)).unwrap();
    assert!(kept.branch.kept);
    assert_eq!(
        (kept.immediate, kept.staged),
        (vec!["kept".to_owned()], Vec::new())
    );
    assert!(
        store
            .keep_branch(&who, &keep(true))
            .unwrap()
            .immediate
            .is_empty()
    );
    assert!(!store.keep_branch(&who, &keep(false)).unwrap().branch.kept);
}

fn save(picks: &[(&str, Option<&str>)], version: Option<&str>) -> Move {
    Move::Save(Save {
        from: at("fix-web"),
        picks: (!picks.is_empty()).then(|| {
            picks
                .iter()
                .map(|(row, choice)| ployz_store::MovePick {
                    row: (*row).to_owned(),
                    choice: choice.map(|choice| serde_json::from_value(json!(choice)).unwrap()),
                })
                .collect()
        }),
        version: version.map(str::to_owned),
        ..Save::default()
    })
}

#[test]
fn save_moves_the_picked_changes_into_the_parent_and_keeps_the_rest() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, true);
    store
        .create_branch(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    set(
        &store,
        &who,
        "fix-web",
        &[
            ("web.image", json!("web:2")),
            ("web.env.NEW", json!("1")),
            ("web.env.API_KEY", json!({ "secret": "branch-secret" })),
        ],
    );
    // The Parent changed the image too: saving it overwrites that.
    set(
        &store,
        &who,
        "production",
        &[("web.env.OWN", json!("mine"))],
    );
    set(
        &store,
        &who,
        "production",
        &[("web.image", json!("web:hot"))],
    );
    let query = MoveQuery::Save {
        from: at("fix-web"),
        into: None,
        when: None,
    };
    let review = store.move_view(&who, &query).unwrap();
    assert_eq!(review.into.name.as_str(), "production");
    let rows: Vec<(&str, bool)> = review
        .rows
        .iter()
        .map(|row| (row.row.as_str(), row.conflict))
        .collect();
    assert_eq!(
        rows,
        [
            ("web.env.API_KEY", false),
            ("web.env.NEW", false),
            ("web.image", true)
        ]
    );
    let image = &review.rows[2];
    assert_eq!(
        (&image.from, &image.into),
        (&json!("web:2"), &json!("web:hot"))
    );
    // A Branch's own new secret never moves by default: it wants a fresh value.
    let token = &review.rows[0];
    assert_eq!(token.from, json!({ "secret": true }));
    let choice = token.choice.as_ref().unwrap();
    assert!(choice.secret);
    assert_eq!(json!(choice.default), json!("new"));
    let refused = store.move_changes(&who, &save(&[], None)).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(refused.details["rows"], json!(["web.env.API_KEY"]));
    assert_eq!(
        code(store.move_changes(&who, &save(&[("nope", None)], None))),
        RpcErrorCode::NotFound
    );
    assert_eq!(
        code(store.move_changes(&who, &save(&[("web.image", Some("from"))], None))),
        RpcErrorCode::InvalidArgument
    );
    // Only a Branch saves, into its own Parent.
    let root = Move::Save(Save {
        from: at("production"),
        ..Save::default()
    });
    assert_eq!(
        code(store.move_changes(&who, &root)),
        RpcErrorCode::InvalidArgument
    );

    // A review goes stale when the Parent moves on; nothing lands.
    set(&store, &who, "production", &[("web.replicas", json!(2))]);
    let stale = save(&[("web.image", None)], Some(&review.version));
    assert_eq!(
        code(store.move_changes(&who, &stale)),
        RpcErrorCode::Conflict
    );
    assert_eq!(
        values(&store, &who, "production", "web")["image"],
        json!("web:hot")
    );

    let version = store.move_view(&who, &query).unwrap().version;
    let saved = store
        .move_changes(
            &who,
            &save(
                &[("web.image", None), ("web.env.NEW", None)],
                Some(&version),
            ),
        )
        .unwrap();
    assert_eq!(texts(&saved.staged), ["web"]);
    assert_eq!(saved.branch.unwrap().environment.name.as_str(), "fix-web");
    let web = values(&store, &who, "production", "web");
    assert_eq!(
        (
            &web["image"],
            &web["env"]["NEW"],
            &web["env"]["OWN"],
            web["replicas"].clone()
        ),
        (&json!("web:2"), &json!("1"), &json!("mine"), json!(2))
    );
    // The unpicked secret stays in the Branch and still moves later.
    let rows = store.move_view(&who, &query).unwrap().rows;
    assert_eq!(
        rows.iter().map(|row| row.row.as_str()).collect::<Vec<_>>(),
        ["web.env.API_KEY"]
    );
    let input = deploy(&store, &who, "production", 2, true);
    assert!(
        snapshot(&input, &uuid(3))["resolvedEnv"]
            .get("API_KEY")
            .is_none()
    );
    // Picked `from`, the Branch's sealed value lands as it is.
    store
        .move_changes(&who, &save(&[("web.env", Some("from"))], None))
        .unwrap();
    let input = deploy(&store, &who, "production", 3, true);
    assert_eq!(
        snapshot(&input, &uuid(3))["resolvedEnv"]["API_KEY"],
        "branch-secret"
    );
    assert_eq!(
        store
            .move_changes(&who, &save(&[], None))
            .unwrap_err()
            .message,
        "Nothing to save into production"
    );

    // Picked `new` with a value, each variable lands with the Parent's own: a secret sealed.
    set(
        &store,
        &who,
        "fix-web",
        &[
            ("web.env.SESSION_KEY", json!({ "secret": "branch-key" })),
            ("web.env.HOST", json!("branch-host")),
        ],
    );
    let fresh = |row: &str, value: &str| ployz_store::MovePick {
        row: row.to_owned(),
        choice: Some(PickChoice::New(value.to_owned())),
    };
    let picked = |picks: Vec<ployz_store::MovePick>| {
        Move::Save(Save {
            from: at("fix-web"),
            picks: Some(picks),
            ..Save::default()
        })
    };
    // A value is one variable's own.
    assert_eq!(
        code(store.move_changes(&who, &picked(vec![fresh("web.env", "x")]))),
        RpcErrorCode::InvalidArgument
    );
    store
        .move_changes(
            &who,
            &picked(vec![
                fresh("web.env.SESSION_KEY", "prod-key"),
                fresh("web.env.HOST", "${{ web.NEW }}-prod"),
            ]),
        )
        .unwrap();
    let input = deploy(&store, &who, "production", 5, true);
    let env = &snapshot(&input, &uuid(3))["resolvedEnv"];
    assert_eq!(
        (&env["SESSION_KEY"], &env["HOST"]),
        (&json!("prod-key"), &json!("1-prod"))
    );
    let stored = values(&store, &who, "production", "web");
    assert_eq!(stored["env"]["SESSION_KEY"], json!({ "secret": true }));

    // No Save from a Branch on its way off the Servers.
    set(&store, &who, "fix-web", &[("web.env.LATE", json!("1"))]);
    deploy(&store, &who, "fix-web", 4, true);
    store
        .admit(
            &who,
            &Admit::Remove(Removal {
                id: DeploymentId::parse(uuid(90)).unwrap(),
                environment: at("fix-web"),
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &Trusted::default(),
        )
        .unwrap();
    assert_eq!(
        code(store.move_changes(&who, &save(&[], None))),
        RpcErrorCode::Conflict
    );
}

#[test]
fn a_branch_is_planned_by_name_before_it_is_created() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, true);
    let plan = |copy: &[&str], preset: Option<&str>| {
        let query = ployz_store::Query::BranchPlan(ployz_store::BranchPlanQuery {
            from: at("production"),
            focus: vec![node("web")],
            copy: copy.iter().map(|name| node(name)).collect(),
            preset: preset.map(|preset| serde_json::from_value(json!(preset)).unwrap()),
        });
        let ployz_store::View::BranchPlan(plan) = store.read(&who, &query).unwrap() else {
            panic!("a branch plan");
        };
        json!(plan.nodes)
            .as_array()
            .unwrap()
            .iter()
            .map(|node| {
                (
                    node["name"].as_str().unwrap().to_owned(),
                    node["role"].as_str().unwrap().to_owned(),
                    node["toggled"].as_str().unwrap().to_owned(),
                    node["owner"].clone(),
                    node["data"].as_bool().unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let web = plan(&["web"], None);
    let db = web.iter().find(|node| node.0 == "db").unwrap();
    // web uses db live, from production, where it holds real data; toggled, the Branch gets its own.
    assert_eq!(
        (db.1.as_str(), db.2.as_str(), &db.3, db.4),
        ("live", "own", &json!("production"), true)
    );
    assert_eq!(web.iter().find(|node| node.0 == "web").unwrap().1, "own");
    // Everything copies every node.
    assert!(plan(&[], Some("all")).iter().all(|node| node.1 == "own"));
    // The Store creates the Branch the plan shows.
    let created = store
        .create_branch(&who, &branch("try-web", "production", &["web"]))
        .unwrap();
    assert_eq!(
        created
            .branch
            .live
            .iter()
            .map(|node| node.name.as_str())
            .collect::<Vec<_>>(),
        ["db"]
    );
    assert_eq!(
        code(store.read(
            &who,
            &ployz_store::Query::BranchPlan(ployz_store::BranchPlanQuery {
                from: at("production"),
                copy: vec![node("nope")],
                ..Default::default()
            })
        )),
        RpcErrorCode::NotFound
    );
}
