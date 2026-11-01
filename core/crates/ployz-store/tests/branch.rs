#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Branches through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): Own Copies and Live Nodes, Setup Commands, Own Copy of a Live Node,
//! Keep and fixing a failed Deployment. None of it needs a Server until
//! something deploys.

use ployz_core::{DeployOutcome, DeployPreview, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AddDomain, Admit, Branched, Change, ConfigStore, CopyNode, CreateBranch, CreateProject,
    CreateService, CreateVolume, Deploy, DeploymentId, DiffQuery, Discard, DomainName,
    DomainsQuery, Edit, EnvironmentId, EnvironmentName, EnvironmentRef, KeepBranch, LiveNode,
    Mount, OrganizationId, ProjectId, ProjectName, RunEvidence, RunnerId, SealedNamesQuery,
    ServiceLineageId, ServiceQuery, SettingPath, SetupCommand, Trusted, VolumeId, VolumeName,
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
    shop_on(backend::open())
}

/// [`shop`] on `store`.
fn shop_on(store: ConfigStore) -> (ConfigStore, Actor) {
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
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
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some(image.into()),
                    template: None,
                },
            )
            .unwrap();
    }
    store
        .write(
            &who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: ployz_core::config::VolumeKind::Docker {},
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
            (
                "web.env.TOKEN",
                backend::sealed_secret(&uuid(2), &uuid(3), "TOKEN", 1),
            ),
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
        .write(
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
        .read(
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
            .read(
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
        .write_trusted(
            who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: at(environment),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
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
                progress: Vec::new(),
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
    let made = store.write(&who, &create).unwrap();
    // A replay returns the same; the ID with another body is a conflict.
    assert_eq!(store.write(&who, &create).unwrap(), made);
    let mut other = create.clone();
    other.keep = false;
    assert_eq!(code(store.write(&who, &other)), RpcErrorCode::Conflict);

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
        .read(
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
        json!({ "DB_URL": "${{ db.PLOYZ_PRIVATE_DOMAIN }}", "TOKEN": { "secret": false } })
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
    assert!(made.branch.kept && made.branch.live.is_empty());
    assert_eq!(made.branch.parent.as_str(), "production");
    // Each copy has its Node Introduction: all three are created by the next Deploy.
    let diff = store
        .read(
            &who,
            &DiffQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap();
    assert_eq!(diff.changes.len(), 3);

    // Deploying it carries the credential and web's Setup Command. The secret was
    // sealed for production, so it arrives Not set here and never reaches the env.
    let input = deploy(&store, &who, "fix-web", 1, false);
    let web_id = id_of(&store, &who, "fix-web", "web");
    let db_id = id_of(&store, &who, "fix-web", "db");
    assert_eq!(snapshot(&input, &web_id)["resolvedEnv"].get("TOKEN"), None);
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
fn a_branch_names_the_secrets_it_copied_without_a_value() {
    let (store, who) = shop();
    let made = store
        .write(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    let not_set = |branched: &Branched| -> Vec<String> {
        branched
            .not_set
            .iter()
            .map(|variable| format!("{}.env.{}", variable.service, variable.key))
            .collect()
    };
    assert_eq!(not_set(&made), ["web.env.TOKEN"]);

    let only_db = store
        .write(&who, &branch("fix-db", "production", &["db"]))
        .unwrap();
    assert!(only_db.not_set.is_empty());

    set(
        &store,
        &who,
        "fix-web",
        &[(
            "web.env.TOKEN",
            backend::sealed_secret(
                &made.branch.environment.id.to_string(),
                &id_of(&store, &who, "fix-web", "web"),
                "TOKEN",
                2,
            ),
        )],
    );
    let kept = store
        .write(
            &who,
            &KeepBranch {
                environment: at("fix-web"),
                kept: true,
            },
        )
        .unwrap();
    assert!(kept.not_set.is_empty());
}

#[test]
fn a_branch_is_refused_before_anything_is_written() {
    let (store, who) = shop();
    store
        .write(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    let mut taken = branch("fix-web", "production", &["web"]);
    taken.id = EnvironmentId::parse(uuid(90)).unwrap();
    assert_eq!(code(store.write(&who, &taken)), RpcErrorCode::Conflict);
    assert_eq!(
        code(store.write(&who, &branch("empty", "production", &[]))),
        RpcErrorCode::InvalidArgument
    );
    assert_eq!(
        code(store.write(&who, &branch("stray", "production", &["nope"]))),
        RpcErrorCode::NotFound
    );
    let mut stray = branch("stray", "production", &["db"]);
    stray.setup = vec![SetupCommand {
        service: ServiceName::parse("web").unwrap(),
        command: "seed".into(),
    }];
    assert_eq!(
        code(store.write(&who, &stray)),
        RpcErrorCode::InvalidArgument
    );
    stray.setup = vec![SetupCommand {
        service: ServiceName::parse("db").unwrap(),
        command: "x".repeat(2001),
    }];
    assert_eq!(
        code(store.write(&who, &stray)),
        RpcErrorCode::InvalidArgument
    );
    // Only a Branch updates, copies or is kept.
    let keep = KeepBranch {
        environment: at("production"),
        kept: true,
    };
    assert_eq!(
        code(store.write(&who, &keep)),
        RpcErrorCode::InvalidArgument
    );
    assert!(
        store
            .read(
                &who,
                &ployz_store::BranchQuery {
                    environment: at("stray")
                }
            )
            .is_err()
    );
}

#[test]
fn an_empty_parent_says_there_is_nothing_to_branch() {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    for copy in [&[][..], &["web"][..]] {
        let refused = store
            .write(&who, &branch("fix", "production", copy))
            .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{refused:?}");
        assert_eq!(
            refused.message,
            "production is empty, so there is nothing to branch"
        );
        assert_eq!(refused.details["next"], "ployz env new fix --project shop");
    }
}

#[test]
fn a_branch_uses_what_its_parent_runs_live_down_the_tree() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    let made = store.write(&who, &create).unwrap();
    assert_eq!(texts(&made.staged), ["web"]);
    let production = EnvironmentName::parse("production").unwrap();
    assert_eq!(
        made.branch.live,
        // db mounts the data Volume: the Branch writes production's real data; web reads it.
        [LiveNode {
            name: "db".into(),
            owner: Some(production.clone()),
            data: true,
            used_by: vec![ServiceName::parse("web").unwrap()],
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
    // What a live node holds comes from the Parent, which may seal it, so a
    // template reading one counts as Sealed where production's own reading db doesn't.
    let sealed = |environment| {
        let query = SealedNamesQuery {
            environment: at(environment),
        };
        let names = store.read(&who, &query).unwrap().names;
        ["DB_HOST", "DB_URL"].map(|name| names.contains(name))
    };
    assert_eq!(sealed("fix-web"), [true, true]);
    assert_eq!(sealed("production"), [false, false]);
    let mut wrong = branch("other", "production", &["web"]);
    wrong.live = vec![node("volumes.data")];
    assert_eq!(
        code(store.write(&who, &wrong)),
        RpcErrorCode::InvalidArgument
    );

    // Its deploy reads db at production's address; a Branch of the Branch too.
    let input = deploy(&store, &who, "fix-web", 2, true);
    let web = snapshot(&input, &id_of(&store, &who, "fix-web", "web"));
    assert_eq!(web["resolvedEnv"]["DB_URL"], "db.shop-production.internal");
    assert_eq!(web["resolvedEnv"]["DB_HOST"], "db.shop-production.internal");
    let child = store
        .write(&who, &branch("child", "fix-web", &["web"]))
        .unwrap();
    assert_eq!(child.branch.live[0].owner, Some(production));
    let input = deploy(&store, &who, "child", 3, false);
    let web = snapshot(&input, &id_of(&store, &who, "child", "web"));
    assert_eq!(web["resolvedEnv"]["DB_URL"], "db.shop-production.internal");
}

#[test]
fn a_live_reference_breaks_once_its_parent_deploys_without_the_variable() {
    let (store, who) = shop();
    set(&store, &who, "production", &[("db.env.PASS", json!("pw"))]);
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    store.write(&who, &create).unwrap();
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.DB_PASS", json!("${{ db.PASS }}"))],
    );
    let broken = || {
        let diff = store
            .read(
                &who,
                &DiffQuery {
                    environment: at("fix-web"),
                },
            )
            .unwrap();
        texts(&diff.broken)
    };
    assert!(broken().is_empty());
    store
        .write(
            &who,
            &Edit {
                environment: at("production"),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("db.env.PASS").unwrap(),
                }],
            },
        )
        .unwrap();
    assert!(broken().is_empty());
    deploy(&store, &who, "production", 2, true);
    assert_eq!(
        broken(),
        ["web.env.DB_PASS references db.PASS, which doesn't exist"]
    );
}

#[test]
fn a_live_reference_holds_while_its_parent_only_stages_the_removal() {
    let (store, who) = shop();
    set(&store, &who, "production", &[("db.env.PASS", json!("pw"))]);
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    store.write(&who, &create).unwrap();
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.DB_PASS", json!("${{ db.PASS }}"))],
    );
    store
        .write(
            &who,
            &ployz_store::RemoveService {
                environment: at("production"),
                service: ServiceName::parse("db").unwrap(),
            },
        )
        .unwrap();
    let diff = store
        .read(
            &who,
            &DiffQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap();
    assert!(diff.broken.is_empty(), "{:?}", texts(&diff.broken));
    assert_eq!(
        values(&store, &who, "fix-web", "web")["env"]["DB_PASS"],
        json!("${{ db.PASS }}")
    );
}

#[test]
fn removing_an_own_copy_the_parent_still_runs_breaks_nothing() {
    let (store, who) = shop();
    set(&store, &who, "production", &[("db.env.PASS", json!("pw"))]);
    deploy(&store, &who, "production", 1, true);
    store
        .write(&who, &branch("fix-web", "production", &["web", "db"]))
        .unwrap();
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.DB_PASS", json!("${{ db.PASS }}"))],
    );
    let removed = store
        .write(
            &who,
            &ployz_store::RemoveService {
                environment: at("fix-web"),
                service: ServiceName::parse("db").unwrap(),
            },
        )
        .unwrap();
    assert!(removed.broken.is_empty(), "{:?}", texts(&removed.broken));
}

#[test]
fn an_own_copy_of_a_live_node_brings_an_empty_copy_of_its_volume() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, true);
    store
        .write(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    let copy = CopyNode {
        environment: at("fix-web"),
        node: ServiceName::parse("db").unwrap(),
        expect: None,
    };
    // Never deployed, it doesn't run its Working State yet.
    assert_eq!(code(store.write(&who, &copy)), RpcErrorCode::Conflict);
    deploy(&store, &who, "fix-web", 2, true);
    let copied: Branched = store.write(&who, &copy).unwrap();
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
        .read(
            &who,
            &DiffQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap();
    assert_eq!(diff.changes.len(), 2);
    assert_eq!(code(store.write(&who, &copy)), RpcErrorCode::Conflict);
    // Its references now resolve inside the Branch.
    let input = deploy(&store, &who, "fix-web", 3, true);
    let web = snapshot(&input, &id_of(&store, &who, "fix-web", "web"));
    assert_eq!(web["resolvedEnv"]["DB_URL"], "db.internal");
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
    assert_eq!(code(store.write(&who, &fix)), RpcErrorCode::InvalidArgument);
    fix.fix = Some(failed.clone());
    fix.copy = vec![node("volumes.data")];
    assert_eq!(code(store.write(&who, &fix)), RpcErrorCode::InvalidArgument);

    // Named none: it copies the Services the Deployment didn't apply.
    fix.copy = Vec::new();
    let made = store.write(&who, &fix).unwrap();
    assert_eq!(texts(&made.staged), ["web"]);
    assert_eq!(values(&store, &who, "fix", "web")["image"], "web:2");
    assert_eq!(values(&store, &who, "production", "web")["image"], "web:3");
}

#[test]
fn keep_applies_at_once_and_generated_domains_follow_the_branch_name() {
    let (store, who) = shop();
    store
        .write_trusted(
            &who,
            &AddDomain {
                environment: at("production"),
                service: ServiceName::parse("web").unwrap(),
                hostname: None,
                prefix: None,
                port: None,
            },
            &Trusted::default(),
        )
        .unwrap();
    store
        .write(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    let child = store
        .write(&who, &branch("child", "fix-web", &["web"]))
        .unwrap();
    assert_eq!(child.branch.parent.as_str(), "fix-web");
    let prefix = |environment: &str| {
        let domains = store
            .read_trusted(
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
    let kept = store.write(&who, &keep(true)).unwrap();
    assert!(kept.branch.kept);
    assert!(kept.staged.is_empty());
    assert!(!store.write(&who, &keep(false)).unwrap().branch.kept);
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
        .write(&who, &branch("try-web", "production", &["web"]))
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

#[test]
fn a_branch_of_nothing_offers_no_preset_that_copies_the_same() {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("empty").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    let query = ployz_store::Query::BranchPlan(ployz_store::BranchPlanQuery {
        from: at("production"),
        ..Default::default()
    });
    let ployz_store::View::BranchPlan(plan) = store.read(&who, &query).unwrap() else {
        panic!("a branch plan");
    };
    // Every preset copies nothing, so "Everything" would be a choice that changes nothing.
    assert_eq!(json!(plan.presets), json!(["only"]));
}

#[test]
fn a_branch_plan_presses_the_offered_preset_its_preset_copies_the_same_as() {
    // Nothing deployed: Only web already copies db, so Uses is no choice and All copies the same.
    let (store, who) = shop();
    let plan = |preset: &str| {
        let query = ployz_store::Query::BranchPlan(ployz_store::BranchPlanQuery {
            from: at("production"),
            focus: vec![node("web")],
            preset: Some(serde_json::from_value(json!(preset)).unwrap()),
            ..Default::default()
        });
        let ployz_store::View::BranchPlan(plan) = store.read(&who, &query).unwrap() else {
            panic!("a branch plan");
        };
        (json!(plan.preset), json!(plan.presets))
    };
    for preset in ["only", "uses", "all"] {
        let (pressed, offered) = plan(preset);
        assert_eq!(offered, json!(["only"]), "{preset}");
        assert_eq!(pressed, json!("only"), "{preset}");
    }
}

#[test]
fn a_branch_plan_presses_uses_for_all_when_everything_is_what_web_uses() {
    // Deployed: Only web leaves db live, and web uses everything else, so All copies the same as Uses.
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, true);
    let plan = |preset: Option<&str>, copy: &[&str]| {
        let query = ployz_store::Query::BranchPlan(ployz_store::BranchPlanQuery {
            from: at("production"),
            focus: vec![node("web")],
            copy: copy.iter().map(|name| node(name)).collect(),
            preset: preset.map(|preset| serde_json::from_value(json!(preset)).unwrap()),
        });
        let ployz_store::View::BranchPlan(plan) = store.read(&who, &query).unwrap() else {
            panic!("a branch plan");
        };
        (json!(plan.preset), json!(plan.presets))
    };
    for (preset, pressed) in [("only", "only"), ("uses", "uses"), ("all", "uses")] {
        assert_eq!(
            plan(Some(preset), &[]),
            (json!(pressed), json!(["only", "uses"])),
            "{preset}"
        );
    }
    // Picks that match no preset press none.
    assert_eq!(plan(None, &["db"]), (Value::Null, json!(["only", "uses"])));
}

#[test]
fn a_branch_naming_no_setup_runs_its_parents_defaults_for_what_it_copies() {
    let (store, who) = shop();
    let setup = |service: &str, command: &str| SetupCommand {
        service: ServiceName::parse(service).unwrap(),
        command: command.into(),
    };
    // A Setup Command runs in a Service the Environment has.
    let refused = store.write(
        &who,
        &ployz_store::SetBranchSetup {
            environment: at("production"),
            setup: vec![setup("web", "true"), setup("gone", "true")],
        },
    );
    assert_eq!(code(refused), RpcErrorCode::NotFound);
    let listed = store
        .write(
            &who,
            &ployz_store::SetBranchSetup {
                environment: at("production"),
                setup: vec![setup("web", " pnpm db:seed ")],
            },
        )
        .unwrap();
    let production = listed
        .environments
        .iter()
        .find(|listing| listing.name.as_str() == "production")
        .unwrap();
    assert_eq!(production.branch_setup.len(), 1);
    for invalid in ["  ", "private\0value"] {
        let refused = store
            .write(
                &who,
                &ployz_store::SetBranchSetup {
                    environment: at("production"),
                    setup: vec![setup("web", invalid)],
                },
            )
            .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
        assert!(!refused.message.contains("private"));
    }

    // Only what the Branch copies runs, and a Branch naming its own replaces them.
    let made = store
        .write(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    assert_eq!(made.branch.setup, [setup("web", "pnpm db:seed")]);
    let mut own = branch("fix-db", "production", &["db"]);
    own.setup = vec![setup("db", "seed")];
    let made = store.write(&who, &own).unwrap();
    assert_eq!(made.branch.setup, [setup("db", "seed")]);
}

#[test]
fn setup_is_a_reserved_service_name_and_an_older_service_named_setup_must_be_renamed() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_on(ConfigStore::open(&url, backend::key()).unwrap());
    let reserved = "setup is reserved: paths name a Branch's Setup Commands as setup.SERVICE";

    let created = store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(9)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("setup").unwrap(),
                image: Some("busybox".into()),
                template: None,
            },
        )
        .unwrap_err();
    assert_eq!(created.code, RpcErrorCode::InvalidArgument);
    assert_eq!(created.message, reserved);

    let renamed = store
        .write(
            &who,
            &ployz_store::RenameService {
                environment: at("production"),
                service: ServiceName::parse("db").unwrap(),
                name: ServiceName::parse("setup").unwrap(),
            },
        )
        .unwrap_err();
    assert_eq!(renamed.message, reserved);

    // A Service named setup from before the name was reserved.
    backend::execute(
        &url,
        r#"UPDATE config_environment SET working = replace(replace(working, '"slug":"db"', '"slug":"setup"'), '"privateDns":"db"', '"privateDns":"setup"')"#,
    );
    let saved = store
        .write_trusted(
            &who,
            &ployz_store::Publish {
                id: None,
                environment: at("production"),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &Trusted {
                approval: ployz_store::Approval::NotRequired,
                ..Trusted::default()
            },
        )
        .unwrap()
        .saved
        .unwrap();

    // A Branch or a Sync never brings the name into another Environment.
    let copied = store
        .write(&who, &branch("fix-setup", "production", &["setup"]))
        .unwrap_err();
    assert_eq!(copied.message, reserved);
    store
        .write(
            &who,
            &ployz_store::CreateEnvironment {
                id: EnvironmentId::parse(uuid(20)).unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    let offered = store
        .read(
            &who,
            &ployz_store::SyncQuery {
                from: at("production"),
                into: Some(at("staging")),
            },
        )
        .unwrap();
    let synced = store
        .write(
            &who,
            &ployz_store::SyncChanges {
                from: at("production"),
                into: Some(at("staging")),
                close_after: false,
                version: offered.version,
                picks: None,
                skip: Vec::new(),
                values: std::collections::BTreeMap::new(),
                id: None,
            },
        )
        .unwrap_err();
    assert_eq!(synced.message, reserved);

    let rename_first = "A Service in production is named setup, and setup.SERVICE now means \
        a Branch's Setup Command; rename it first: \
        ployz service rename setup NAME --env production --project shop";
    let read = store
        .read(
            &who,
            &ployz_store::EnvironmentQuery {
                environment: at("production"),
                path: Some(SettingPath::parse("setup.image").unwrap()),
                all: false,
            },
        )
        .unwrap_err();
    assert_eq!(read.message, rename_first);
    let set = store
        .write(
            &who,
            &Edit {
                environment: at("production"),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("setup.web").unwrap(),
                    value: json!("pnpm db:seed"),
                }],
            },
        )
        .unwrap_err();
    assert_eq!(set.message, rename_first);

    // Renaming it away is allowed, and setup.SERVICE paths work again.
    store
        .write(
            &who,
            &ployz_store::RenameService {
                environment: at("production"),
                service: ServiceName::parse("setup").unwrap(),
                name: ServiceName::parse("db").unwrap(),
            },
        )
        .unwrap();
    let read = store
        .read(
            &who,
            &ployz_store::EnvironmentQuery {
                environment: at("production"),
                path: Some(SettingPath::parse("setup.web").unwrap()),
                all: false,
            },
        )
        .unwrap_err();
    assert_eq!(read.message, "production is not a Branch");

    // Restoring the saved Service name setup is refused, though db still holds
    // setup as its Private DNS name.
    let restore = |accept_overwrite, version| ployz_store::StageHistory {
        environment: at("production"),
        revision: saved,
        action: ployz_store::HistoryAction::Restore,
        change: None,
        version,
        accept_overwrite,
    };
    let preview = store.read(
        &who,
        &ployz_store::HistoryPreviewQuery {
            environment: at("production"),
            revision: saved,
            action: ployz_store::HistoryAction::Restore,
            change: None,
        },
    );
    let restored = match preview {
        Ok(preview) => store
            .write(&who, &restore(true, preview.version))
            .unwrap_err(),
        Err(refused) => refused,
    };
    assert_eq!(restored.message, reserved);

    // Nor is discarding back to it.
    let discarded = store
        .write(
            &who,
            &ployz_store::Discard {
                environment: at("production"),
                target: Some(ployz_store::DiscardTarget::Saved),
                path: None,
                version: None,
            },
        )
        .unwrap_err();
    assert!(
        discarded.message.contains(reserved),
        "{}",
        discarded.message
    );
    let names = store
        .read(
            &who,
            &ployz_store::ServicesQuery {
                environment: at("production"),
            },
        )
        .unwrap()
        .services
        .into_iter()
        .map(|listing| listing.service.name.to_string())
        .collect::<Vec<_>>();
    assert!(!names.contains(&"setup".to_owned()), "{names:?}");
}

#[test]
fn a_branchs_own_setup_command_is_read_set_and_unset_at_setup_service() {
    let (store, who) = shop();
    let path = |path: &str| SettingPath::parse(path).unwrap();
    let edit = |environment: &str, change: Change| {
        store.write(
            &who,
            &Edit {
                environment: at(environment),
                expect: None,
                changes: vec![change],
            },
        )
    };
    let get = |environment: &str, at_path: Option<&str>| {
        store.read(
            &who,
            &ployz_store::EnvironmentQuery {
                environment: at(environment),
                path: at_path.map(path),
                all: false,
            },
        )
    };
    let pending = || {
        store
            .read(
                &who,
                &DiffQuery {
                    environment: at("fix-web"),
                },
            )
            .unwrap()
            .setup
    };
    let setup = |command: &str| SetupCommand {
        service: ServiceName::parse("web").unwrap(),
        command: command.into(),
    };
    assert_eq!(path("setup.WEB").to_string(), "setup.web");

    let mut create = branch("fix-web", "production", &["web"]);
    create.setup = vec![setup("exit 7")];
    store.write(&who, &create).unwrap();
    let web_id = id_of(&store, &who, "fix-web", "web");

    // get and diff show it before it runs.
    let read = get("fix-web", Some("setup.web")).unwrap();
    assert_eq!(
        texts(
            &read
                .settings
                .iter()
                .map(|row| &row.path)
                .collect::<Vec<_>>()
        ),
        ["setup.web"]
    );
    assert_eq!(read.settings[0].value, json!("exit 7"));
    assert_eq!(read.settings[0].apply, ployz_store::Apply::Immediate);
    let whole = get("fix-web", None).unwrap();
    assert!(
        whole
            .settings
            .iter()
            .any(|row| row.path == path("setup.web"))
    );
    assert!(
        !whole
            .settings
            .iter()
            .any(|row| row.path == path("setup.db"))
    );
    assert_eq!(pending(), [setup("exit 7")]);

    // A failed Deploy runs it again, so fixing it takes effect at once, unstaged.
    let input = deploy(&store, &who, "fix-web", 1, false);
    assert_eq!(
        snapshot(&input, &web_id)["setupCommands"],
        json!(["exit 7"])
    );
    let edited = edit(
        "fix-web",
        Change::Set {
            path: path("setup.web"),
            value: json!(" pnpm db:seed "),
        },
    )
    .unwrap();
    assert_eq!(texts(&edited.immediate), ["setup.web"]);
    assert!(edited.staged.is_empty());
    assert_eq!(pending(), [setup("pnpm db:seed")]);
    let input = deploy(&store, &who, "fix-web", 2, false);
    assert_eq!(
        snapshot(&input, &web_id)["setupCommands"],
        json!(["pnpm db:seed"])
    );

    // Unset drops it; a second unset changes nothing.
    let unset = || {
        edit(
            "fix-web",
            Change::Unset {
                path: path("setup.web"),
            },
        )
        .unwrap()
    };
    assert_eq!(texts(&unset().immediate), ["setup.web"]);
    assert!(unset().immediate.is_empty());
    assert!(pending().is_empty());
    assert_eq!(
        get("fix-web", Some("setup.web")).unwrap().settings[0].value,
        Value::Null
    );
    let input = deploy(&store, &who, "fix-web", 3, false);
    assert_eq!(snapshot(&input, &web_id)["setupCommands"], json!([]));

    // Once web deploys, a new one won't run, and get says so.
    edit(
        "fix-web",
        Change::Set {
            path: path("setup.web"),
            value: json!("true"),
        },
    )
    .unwrap();
    deploy(&store, &who, "fix-web", 4, true);
    assert!(pending().is_empty());
    let note = get("fix-web", Some("setup.web")).unwrap().settings[0]
        .note
        .clone();
    assert_eq!(
        note.as_deref(),
        Some("web has deployed in fix-web, so setup.web won't run")
    );

    // Only a Branch has one, of a Service it has; it isn't staged, so there is no discard.
    let refused = |result: Result<_, ployz_core::RpcError>| {
        let error = result.map(|_: ployz_store::Edited| ()).unwrap_err();
        (error.code, error.message)
    };
    assert_eq!(
        refused(edit(
            "production",
            Change::Set {
                path: path("setup.web"),
                value: json!("true"),
            },
        )),
        (
            RpcErrorCode::InvalidArgument,
            "production is not a Branch".into()
        )
    );
    assert_eq!(
        code(get("production", Some("setup.web"))),
        RpcErrorCode::InvalidArgument
    );
    assert!(
        !get("production", None)
            .unwrap()
            .settings
            .iter()
            .any(|row| row.path.to_string().starts_with("setup."))
    );
    assert_eq!(
        code(edit(
            "fix-web",
            Change::Set {
                path: path("setup.nosuch"),
                value: json!("true"),
            },
        )),
        RpcErrorCode::NotFound
    );
    assert_eq!(
        code(edit(
            "fix-web",
            Change::Set {
                path: path("setup.web"),
                value: json!(3),
            },
        )),
        RpcErrorCode::InvalidArgument
    );
    assert_eq!(
        code(store.write(
            &who,
            &ployz_store::Discard {
                environment: at("fix-web"),
                target: None,
                path: Some(path("setup.web")),
                version: None,
            },
        )),
        RpcErrorCode::InvalidArgument
    );
}

#[test]
fn a_branch_owns_a_copy_of_the_config_a_copied_service_mounts() {
    let (store, who) = shop();
    let config = |name: &str| ployz_core::ConfigName::parse(name).unwrap();
    let file = |name: &str| ployz_core::ConfigFileName::parse(name).unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(10)).unwrap(),
                environment: at("production"),
                name: config("sentry"),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::PutConfigFile {
                environment: at("production"),
                config: config("sentry"),
                file: file("conf.d/db.yml"),
                content: "host: ${{ db.PLOYZ_PRIVATE_DOMAIN }}".into(),
                mode: Some(ployz_core::config::FileMode::parse("0640").unwrap()),
                uid: Some(1000),
                gid: None,
            },
        )
        .unwrap();

    let made = store
        .write(&who, &branch("fix-web", "production", &["web"]))
        .unwrap();
    assert!(
        texts(&made.staged).contains(&"configs.sentry".to_owned()),
        "{:?}",
        made.staged
    );
    let item = |environment: &str| {
        store
            .read(
                &who,
                &ployz_store::ConfigItemQuery {
                    environment: at(environment),
                    config: config("sentry").into(),
                },
            )
            .unwrap()
    };
    let (parent, copy) = (item("production"), item("fix-web"));
    assert_eq!(copy.lineage.to_string(), uuid(10));
    assert_ne!(copy.config.config.id, parent.config.config.id);
    assert_eq!(copy.contents, parent.contents);
    assert_eq!(copy.config.config.files, parent.config.config.files);
    assert_eq!(
        values(&store, &who, "fix-web", "web")["configs"],
        json!({ "sentry": "/etc/sentry" })
    );

    store
        .write(
            &who,
            &ployz_store::RenameService {
                environment: at("fix-web"),
                service: ServiceName::parse("db").unwrap(),
                name: ServiceName::parse("pg").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        item("fix-web").contents[&file("conf.d/db.yml")],
        "host: ${{ pg.PLOYZ_PRIVATE_DOMAIN }}"
    );
    assert_eq!(
        item("production").contents[&file("conf.d/db.yml")],
        "host: ${{ db.PLOYZ_PRIVATE_DOMAIN }}"
    );

    store
        .write(
            &who,
            &ployz_store::RenameConfig {
                environment: at("fix-web"),
                config: config("sentry"),
                name: config("sentinel"),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::AttachConfig {
                environment: at("fix-web"),
                service: ServiceName::parse("web").unwrap(),
                config: config("sentinel"),
                dir: "/etc/moved".into(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::Discard {
                environment: at("fix-web"),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(ployz_store::SettingPath::parse("web.configs.sentinel").unwrap()),
                version: None,
            },
        )
        .unwrap();
    assert_eq!(
        values(&store, &who, "fix-web", "web")["configs"],
        json!({ "sentinel": "/etc/sentry" })
    );
}

#[test]
fn a_branch_shows_a_service_its_parent_removed_by_the_parents_name_for_it() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(6)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("jobs").unwrap(),
                image: Some("jobs:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "production",
        &[(
            "web.env.HOSTS",
            json!("${{ db.PLOYZ_PRIVATE_DOMAIN }},${{ jobs.PLOYZ_PRIVATE_DOMAIN }}"),
        )],
    );
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db"), node("jobs")];
    store.write(&who, &create).unwrap();
    let remove = |service: &str, n: u8| {
        store
            .write(
                &who,
                &ployz_store::RemoveService {
                    environment: at("production"),
                    service: ServiceName::parse(service).unwrap(),
                },
            )
            .unwrap();
        deploy(&store, &who, "production", n, true);
    };
    let hosts = || values(&store, &who, "fix-web", "web")["env"]["HOSTS"].clone();
    remove("jobs", 2);
    let shown = hosts();
    // db sorts before jobs, so a label numbered as reads meet removals would move.
    remove("db", 3);

    let kept = shown
        .as_str()
        .unwrap()
        .replace("${{ db.PLOYZ_PRIVATE_DOMAIN }},", "");
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.HOSTS", json!(format!("{kept}/v2")))],
    );
    assert_eq!(
        hosts(),
        json!("${{ <deleted jobs>.PLOYZ_PRIVATE_DOMAIN }}/v2")
    );
}

#[test]
fn an_edit_reads_the_removed_service_it_showed_after_a_second_removal_shares_its_name() {
    let (store, who) = shop();
    let jobs = |environment: &str, n: u8| {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: at(environment),
                    name: ServiceName::parse("jobs").unwrap(),
                    image: Some("jobs:1".into()),
                    template: None,
                },
            )
            .unwrap();
    };
    let remove = |environment: &str, n: u8| {
        store
            .write(
                &who,
                &ployz_store::RemoveService {
                    environment: at(environment),
                    service: ServiceName::parse("jobs").unwrap(),
                },
            )
            .unwrap();
        deploy(&store, &who, environment, n, true);
    };
    let reference = json!("${{ jobs.PLOYZ_PRIVATE_DOMAIN }}");
    jobs("production", 6);
    set(
        &store,
        &who,
        "production",
        &[("web.env.FIRST", reference.clone())],
    );
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("b", "production", &["web"]);
    create.live = vec![node("db"), node("jobs")];
    store.write(&who, &create).unwrap();
    remove("production", 2);
    jobs("b", 7);
    set(&store, &who, "b", &[("web.env.SECOND", reference)]);
    deploy(&store, &who, "b", 3, true);
    let mut create = branch("c", "b", &["web"]);
    create.live = vec![node("jobs")];
    store.write(&who, &create).unwrap();
    let first = || values(&store, &who, "c", "web")["env"]["FIRST"].clone();
    // c's own value, so b's edits to its copy stay b's.
    let shown = json!("${{ <deleted jobs>.PLOYZ_PRIVATE_DOMAIN }}/c");
    set(&store, &who, "c", &[("web.env.FIRST", shown.clone())]);
    assert_eq!(first(), shown);

    // b's own jobs leaves as `<deleted jobs>` too, so c shows both by lineage.
    set(&store, &who, "b", &[("web.env.FIRST", json!("plain"))]);
    remove("b", 4);
    set(
        &store,
        &who,
        "c",
        &[(
            "web.env.FIRST",
            json!(format!("{}/v2", shown.as_str().unwrap())),
        )],
    );
    assert_eq!(
        first(),
        json!("${{ <deleted #00000006>.PLOYZ_PRIVATE_DOMAIN }}/c/v2")
    );
}

/// The `before` and `after` of `path`'s row in `environment`'s review.
fn review_row(store: &ConfigStore, who: &Actor, environment: &str, path: &str) -> (Value, Value) {
    let review = store
        .read(
            who,
            &DiffQuery {
                environment: at(environment),
            },
        )
        .unwrap();
    let shown = serde_json::to_string(&review).unwrap();
    let row = review
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|setting| setting.path == path)
        .unwrap_or_else(|| panic!("no {path} row in {shown}"));
    (row.before.clone(), row.after.clone())
}

/// fix-web copies `web` and uses `db` live, deploys, then makes its Own Copy of
/// `db` and deletes `db.env.TOKEN` from it.
fn copy_db_and_delete_token(store: &ConfigStore, who: &Actor) {
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    store.write(who, &create).unwrap();
    deploy(store, who, "fix-web", 2, true);
    copy_db(store, who);
    unset(store, who, "fix-web", "db.env.TOKEN");
}

fn copy_db(store: &ConfigStore, who: &Actor) {
    let copy = CopyNode {
        environment: at("fix-web"),
        node: ServiceName::parse("db").unwrap(),
        expect: None,
    };
    store.write(who, &copy).unwrap();
}

fn unset(store: &ConfigStore, who: &Actor, environment: &str, path: &str) {
    store
        .write(
            who,
            &Edit {
                environment: at(environment),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse(path).unwrap(),
                }],
            },
        )
        .unwrap();
}

/// A Branch that deleted a variable from its Own Copy of a node before the node's
/// owner sealed it never shows the earlier plain value.
#[test]
fn an_own_copy_that_deleted_a_value_before_its_parent_sealed_it_never_shows_the_plain_value() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("db.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 1, true);
    copy_db_and_delete_token(&store, &who);
    set(
        &store,
        &who,
        "production",
        &[(
            "db.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(4), "TOKEN", 2),
        )],
    );
    deploy(&store, &who, "production", 3, true);

    assert_eq!(
        review_row(&store, &who, "fix-web", "db.env.TOKEN"),
        (json!({ "secret": true }), Value::Null)
    );
}

/// Discarding the delete a Branch's later Own Copy made of a variable its owner
/// sealed since brings it back Not set, never as the earlier plain value.
#[test]
fn discarding_an_own_copys_delete_its_owner_sealed_since_brings_it_back_not_set() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("db.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 1, true);
    copy_db_and_delete_token(&store, &who);
    set(
        &store,
        &who,
        "production",
        &[(
            "db.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(4), "TOKEN", 2),
        )],
    );
    deploy(&store, &who, "production", 3, true);

    discard_token(&store, &who);

    let values = values(&store, &who, "fix-web", "db");
    assert_eq!(values["env"]["TOKEN"], json!({ "secret": false }));
    assert!(!values.to_string().contains("plain-private"), "{values}");
}

/// Discarding the delete a Branch's later Own Copy made of a variable its owner
/// replaced with a plain one after a seal brings back that plain value.
#[test]
fn discarding_an_own_copys_delete_its_owner_added_plain_after_a_seal_brings_the_value_back() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[(
            "db.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(4), "TOKEN", 2),
        )],
    );
    deploy(&store, &who, "production", 1, true);
    unset(&store, &who, "production", "db.env.TOKEN");
    set(
        &store,
        &who,
        "production",
        &[("db.env.TOKEN", json!("public-new"))],
    );
    deploy(&store, &who, "production", 3, true);
    copy_db_and_delete_token(&store, &who);

    discard_token(&store, &who);

    assert_eq!(
        values(&store, &who, "fix-web", "db")["env"]["TOKEN"],
        json!("public-new")
    );
}

fn discard_token(store: &ConfigStore, who: &Actor) {
    discard_db_token(store, who, "fix-web");
}

/// The owner's seal of a variable it has since replaced with a plain one doesn't
/// hide the plain one a Branch's later Own Copy deletes.
#[test]
fn an_own_copy_deleting_a_value_its_parent_added_plain_after_a_seal_still_shows_it() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[(
            "db.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(4), "TOKEN", 2),
        )],
    );
    deploy(&store, &who, "production", 1, true);
    unset(&store, &who, "production", "db.env.TOKEN");
    set(
        &store,
        &who,
        "production",
        &[("db.env.TOKEN", json!("public-new"))],
    );
    deploy(&store, &who, "production", 3, true);
    copy_db_and_delete_token(&store, &who);

    assert_eq!(
        review_row(&store, &who, "fix-web", "db.env.TOKEN"),
        (json!("public-new"), Value::Null)
    );
}

/// production seals and deploys `db.env.TOKEN`, which held "plain-private".
fn seal_db_token(store: &ConfigStore, who: &Actor, n: u8) {
    set(
        store,
        who,
        "production",
        &[(
            "db.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(4), "TOKEN", 2),
        )],
    );
    deploy(store, who, "production", n, true);
}

/// fix-web copies `web` and uses `db` live from production, which holds
/// `db.env.TOKEN` plain, deploys, then makes its Own Copy of `db`.
fn copy_db_with_plain_token(store: &ConfigStore, who: &Actor) {
    set(
        store,
        who,
        "production",
        &[("db.env.TOKEN", json!("plain-private"))],
    );
    deploy(store, who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    store.write(who, &create).unwrap();
    deploy(store, who, "fix-web", 2, true);
    copy_db(store, who);
}

/// An in-place plain edit of an Own Copy's variable the owner sealed since reviews
/// from sealed.
#[test]
fn an_own_copys_in_place_edit_made_before_its_owner_sealed_never_shows_the_plain_value() {
    let (store, who) = shop();
    copy_db_with_plain_token(&store, &who);
    set(
        &store,
        &who,
        "fix-web",
        &[("db.env.TOKEN", json!("plain-edit"))],
    );
    seal_db_token(&store, &who, 3);

    assert_eq!(
        review_row(&store, &who, "fix-web", "db.env.TOKEN"),
        (json!({ "secret": true }), json!("plain-edit"))
    );
}

/// A Branch made from a Branch's Own Copy follows it to the owner's seal of a
/// variable both deleted before it.
#[test]
fn a_branch_of_an_own_copy_that_deleted_a_value_before_its_owner_sealed_it_never_shows_it() {
    let (store, who) = shop();
    copy_db_with_plain_token(&store, &who);
    store
        .write(&who, &branch("fix-db", "fix-web", &["web", "db"]))
        .unwrap();
    unset(&store, &who, "fix-web", "db.env.TOKEN");
    unset(&store, &who, "fix-db", "db.env.TOKEN");
    seal_db_token(&store, &who, 3);

    assert_eq!(
        review_row(&store, &who, "fix-db", "db.env.TOKEN"),
        (json!({ "secret": true }), Value::Null)
    );
}

/// Discarding that delete brings the variable back Not set.
#[test]
fn discarding_a_delete_in_a_branch_of_an_own_copy_its_owner_sealed_since_brings_it_back_not_set() {
    let (store, who) = shop();
    copy_db_with_plain_token(&store, &who);
    store
        .write(&who, &branch("fix-db", "fix-web", &["web", "db"]))
        .unwrap();
    unset(&store, &who, "fix-web", "db.env.TOKEN");
    unset(&store, &who, "fix-db", "db.env.TOKEN");
    seal_db_token(&store, &who, 3);

    discard_db_token(&store, &who, "fix-db");

    let values = values(&store, &who, "fix-db", "db");
    assert_eq!(values["env"]["TOKEN"], json!({ "secret": false }));
    assert!(!values.to_string().contains("plain-private"), "{values}");
}

/// An Own Copy of a node its owner itself copied follows that copy to the seal,
/// though the owner deleted the variable since and so took no seal.
#[test]
fn an_own_copy_of_an_own_copy_that_deleted_a_value_before_the_root_sealed_it_never_shows_it() {
    let (store, who) = shop();
    copy_db_with_plain_token(&store, &who);
    deploy(&store, &who, "fix-web", 3, true);
    let mut create = branch("fix-db", "fix-web", &["web"]);
    create.live = vec![node("db")];
    store.write(&who, &create).unwrap();
    deploy(&store, &who, "fix-db", 4, true);
    store
        .write(
            &who,
            &CopyNode {
                environment: at("fix-db"),
                node: ServiceName::parse("db").unwrap(),
                expect: None,
            },
        )
        .unwrap();
    unset(&store, &who, "fix-web", "db.env.TOKEN");
    unset(&store, &who, "fix-db", "db.env.TOKEN");
    seal_db_token(&store, &who, 5);

    assert_eq!(
        review_row(&store, &who, "fix-db", "db.env.TOKEN"),
        (json!({ "secret": true }), Value::Null)
    );
}

/// An owner's own in-place change isn't what the root sealed: an Own Copy of it
/// that deletes it still shows it.
#[test]
fn an_own_copy_deleting_a_value_its_owner_changed_before_a_seal_still_shows_it() {
    let (store, who) = shop();
    copy_db_with_plain_token(&store, &who);
    set(
        &store,
        &who,
        "fix-web",
        &[("db.env.TOKEN", json!("plain-edit"))],
    );
    deploy(&store, &who, "fix-web", 3, true);
    let mut create = branch("fix-db", "fix-web", &["web"]);
    create.live = vec![node("db")];
    store.write(&who, &create).unwrap();
    deploy(&store, &who, "fix-db", 4, true);
    store
        .write(
            &who,
            &CopyNode {
                environment: at("fix-db"),
                node: ServiceName::parse("db").unwrap(),
                expect: None,
            },
        )
        .unwrap();
    unset(&store, &who, "fix-db", "db.env.TOKEN");
    seal_db_token(&store, &who, 5);

    assert_eq!(
        review_row(&store, &who, "fix-db", "db.env.TOKEN"),
        (json!("plain-edit"), Value::Null)
    );
}

fn discard_db_token(store: &ConfigStore, who: &Actor, environment: &str) {
    store
        .write(
            who,
            &Discard {
                environment: at(environment),
                target: None,
                path: Some(SettingPath::parse("db.env.TOKEN").unwrap()),
                version: None,
            },
        )
        .unwrap();
}

/// A copy removed and made again matches the owner's seals by its own source: the
/// first copy's sealed variable doesn't hide the plain one the second came from.
#[test]
fn an_own_copy_made_again_matches_only_its_own_source() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("db.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    store.write(&who, &create).unwrap();
    deploy(&store, &who, "fix-web", 2, true);
    copy_db(&store, &who);
    store
        .write(
            &who,
            &ployz_store::RemoveService {
                environment: at("fix-web"),
                service: ServiceName::parse("db").unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::RemoveVolume {
                environment: at("fix-web"),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "production",
        &[(
            "db.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(4), "TOKEN", 2),
        )],
    );
    deploy(&store, &who, "production", 3, true);
    unset(&store, &who, "production", "db.env.TOKEN");
    set(
        &store,
        &who,
        "production",
        &[("db.env.TOKEN", json!("public-new"))],
    );
    deploy(&store, &who, "production", 4, true);
    copy_db(&store, &who);
    unset(&store, &who, "fix-web", "db.env.TOKEN");

    assert_eq!(
        review_row(&store, &who, "fix-web", "db.env.TOKEN"),
        (json!("public-new"), Value::Null)
    );
}

/// A Branch of a Branch copies a node its Parent uses live from production: the
/// owner's seal, not the Parent's, is the one its Own Copy follows.
#[test]
fn an_own_copy_from_a_grandparent_never_shows_a_value_it_sealed_since() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("db.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    store.write(&who, &create).unwrap();
    deploy(&store, &who, "fix-web", 2, true);
    store
        .write(&who, &branch("child", "fix-web", &["web"]))
        .unwrap();
    deploy(&store, &who, "child", 3, true);
    let copy = CopyNode {
        environment: at("child"),
        node: ServiceName::parse("db").unwrap(),
        expect: None,
    };
    store.write(&who, &copy).unwrap();
    unset(&store, &who, "child", "db.env.TOKEN");
    set(
        &store,
        &who,
        "production",
        &[(
            "db.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(4), "TOKEN", 2),
        )],
    );
    deploy(&store, &who, "production", 4, true);

    assert_eq!(
        review_row(&store, &who, "child", "db.env.TOKEN"),
        (json!({ "secret": true }), Value::Null)
    );
}

/// The warnings of a Deploy of `environment`, claimed and left not executed.
fn warnings(store: &ConfigStore, who: &Actor, environment: &str, n: u8) -> Value {
    let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap();
    store
        .write_trusted(
            who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: at(environment),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id, &runner).unwrap();
    store
        .record(&id, &runner, RunEvidence::NotExecuted("no Servers".into()))
        .unwrap();
    json!(claimed.deployment.warnings)
}

/// A Branch's variable that reads a value its parent sealed in a node they share
/// is left out, and the warning names what it reads and that it's sealed there.
#[test]
fn a_branch_reference_to_a_value_its_parent_sealed_names_it_as_sealed_in_the_parent() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[
            ("db.env.TOK3", json!("fake-plain-3677")),
            ("web.env.DBURL", json!("${{ db.TOK3 }}")),
        ],
    );
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    store.write(&who, &create).unwrap();
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.VIA", json!("${{ TOKEN }}"))],
    );
    set(
        &store,
        &who,
        "production",
        &[(
            "db.env.TOK3",
            backend::sealed_secret(&uuid(2), &uuid(4), "TOK3", 2),
        )],
    );
    deploy(&store, &who, "production", 2, true);

    let warnings = warnings(&store, &who, "fix-web", 3);
    let mut warnings: Vec<Value> = warnings.as_array().unwrap().clone();
    warnings.sort_by_key(|warning| warning["variable"].as_str().map(str::to_owned));
    assert_eq!(
        warnings,
        [
            json!({"service": "web", "variable": "DBURL", "reads": "db.TOK3", "sealed_elsewhere": true}),
            json!({"service": "web", "variable": "TOKEN"}),
            json!({"service": "web", "variable": "VIA", "reads": "web.TOKEN"}),
        ]
    );
    assert!(!json!(warnings).to_string().contains("fake-plain-3677"));
}

/// A sealed value a Branch reads through a node it shares, from a node it
/// doesn't name, is named by the variable of the shared node it reads.
#[test]
fn a_branch_reference_through_a_shared_node_names_the_shared_variable_it_reads() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(6)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("vault").unwrap(),
                image: Some("vault:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "production",
        &[
            ("vault.env.PASS", json!("fake-plain-3677")),
            ("db.env.URL", json!("${{ vault.PASS }}")),
            ("web.env.DBURL", json!("${{ db.URL }}")),
        ],
    );
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    store.write(&who, &create).unwrap();
    set(
        &store,
        &who,
        "production",
        &[(
            "vault.env.PASS",
            backend::sealed_secret(&uuid(2), &uuid(6), "PASS", 2),
        )],
    );
    deploy(&store, &who, "production", 2, true);

    let warnings = warnings(&store, &who, "fix-web", 3);
    let dburl: Vec<&Value> = warnings
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning["variable"] == "DBURL")
        .collect();
    assert_eq!(
        dburl,
        [
            &json!({"service": "web", "variable": "DBURL", "reads": "db.URL", "sealed_elsewhere": true})
        ]
    );
    assert!(!warnings.to_string().contains("fake-plain-3677"));
}

/// A fake plain value no Branch may keep once production seals it.
const FAKE: &str = "fake-plain-3666";

/// production runs `db.env.TOKEN` plain [`FAKE`] beside a plain and a generated
/// variable, and fix-web copies `web` and uses `db` live. On `url`, a fresh database.
fn running_plain_token(url: &str) -> (ConfigStore, Actor) {
    let (store, who) = shop_on(ConfigStore::open(url, backend::key()).unwrap());
    set(
        &store,
        &who,
        "production",
        &[
            ("db.env.TOKEN", json!(FAKE)),
            ("db.env.REGION", json!("fake-region")),
            ("db.env.SALT", json!("${{ secret(32, \"a-z\") }}")),
        ],
    );
    deploy(&store, &who, "production", 1, true);
    let mut create = branch("fix-web", "production", &["web"]);
    create.live = vec![node("db")];
    store.write(&who, &create).unwrap();
    deploy(&store, &who, "fix-web", 2, true);
    (store, who)
}

fn seal_token(store: &ConfigStore, who: &Actor) {
    set(
        store,
        who,
        "production",
        &[(
            "db.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(4), "TOKEN", 2),
        )],
    );
}

fn copy_db_into(store: &ConfigStore, who: &Actor, environment: &str) -> Branched {
    store
        .write(
            who,
            &CopyNode {
                environment: at(environment),
                node: ServiceName::parse("db").unwrap(),
                expect: None,
            },
        )
        .unwrap()
}

/// The copy's `TOKEN` is Not set and named so, and its other variables are as the
/// owner runs them.
fn assert_token_not_set(store: &ConfigStore, who: &Actor, environment: &str, copied: &Branched) {
    let values = values(store, who, environment, "db");
    assert_eq!(values["env"]["TOKEN"], json!({ "secret": false }));
    assert_eq!(values["env"]["REGION"], json!("fake-region"));
    assert!(values["env"]["SALT"].is_string(), "{values}");
    assert!(!values.to_string().contains(FAKE), "{values}");
    assert!(
        copied
            .not_set
            .iter()
            .any(|unset| unset.service.as_str() == "db" && unset.key == "TOKEN"),
        "{:?}",
        copied.not_set
    );
}

/// Every table production still legitimately holds [`FAKE`] in: what it ran.
fn only_what_production_ran(url: &str) {
    let held = backend::tables_holding(url, FAKE);
    for branch_held in [
        "config_environment",
        "config_environment_branch",
        "config_node_introduction",
        "config_sync_arrival",
        "config_sync_base",
    ] {
        assert!(!held.iter().any(|t| t == branch_held), "{held:?}");
    }
}

/// An Own Copy made after its owner saved a seal whose Deploy didn't run copies the
/// value Not set, never the plain value the owner still runs (#3666).
#[test]
fn an_own_copy_made_while_its_owners_seal_is_not_yet_running_copies_it_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = running_plain_token(&url);
    seal_token(&store, &who);
    deploy(&store, &who, "production", 3, false);

    let copied = copy_db_into(&store, &who, "fix-web");

    assert_token_not_set(&store, &who, "fix-web", &copied);
    only_what_production_ran(&url);
    let input = deploy(&store, &who, "fix-web", 4, true);
    assert!(!input.to_string().contains(FAKE));
}

/// Saved without a Deploy, the seal still keeps the plain value out of a copy.
#[test]
fn an_own_copy_made_after_its_owner_only_saved_a_seal_copies_it_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = running_plain_token(&url);
    seal_token(&store, &who);
    store
        .write_trusted(
            &who,
            &ployz_store::Publish {
                id: None,
                environment: at("production"),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &Trusted::default(),
        )
        .unwrap();

    let copied = copy_db_into(&store, &who, "fix-web");

    assert_token_not_set(&store, &who, "fix-web", &copied);
    only_what_production_ran(&url);
}

/// A seal only in the owner's Working State counts, as it does for a new Branch.
#[test]
fn an_own_copy_made_after_its_owner_sealed_without_saving_copies_it_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = running_plain_token(&url);
    seal_token(&store, &who);

    let copied = copy_db_into(&store, &who, "fix-web");

    assert_token_not_set(&store, &who, "fix-web", &copied);
    only_what_production_ran(&url);
}

/// A value the owner sealed and then deleted, still running plain, copies Not set.
#[test]
fn an_own_copy_of_a_value_its_owner_sealed_then_deleted_copies_it_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = running_plain_token(&url);
    seal_token(&store, &who);
    unset(&store, &who, "production", "db.env.TOKEN");

    let copied = copy_db_into(&store, &who, "fix-web");

    assert_token_not_set(&store, &who, "fix-web", &copied);
    only_what_production_ran(&url);
}

/// A plain variable the owner deleted and added back sealed under its key, not yet
/// saved, copies Not set: the seal is a new variable, matched by key.
#[test]
fn an_own_copy_of_a_value_its_owner_replaced_with_a_sealed_one_copies_it_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = running_plain_token(&url);
    unset(&store, &who, "production", "db.env.TOKEN");
    seal_token(&store, &who);

    let copied = copy_db_into(&store, &who, "fix-web");

    assert_token_not_set(&store, &who, "fix-web", &copied);
    only_what_production_ran(&url);
}

/// A variable the owner sealed, deleted and added again plain, then deployed, is a
/// new plain variable: the copy takes its value.
#[test]
fn an_own_copy_of_a_value_its_owner_added_plain_again_after_a_seal_copies_the_new_value() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = running_plain_token(&url);
    seal_token(&store, &who);
    deploy(&store, &who, "production", 3, true);
    unset(&store, &who, "production", "db.env.TOKEN");
    set(
        &store,
        &who,
        "production",
        &[("db.env.TOKEN", json!("fake-public-new"))],
    );
    deploy(&store, &who, "production", 4, true);

    copy_db_into(&store, &who, "fix-web");

    assert_eq!(
        values(&store, &who, "fix-web", "db")["env"]["TOKEN"],
        json!("fake-public-new")
    );
}

/// A Branch of a Branch copies from production, the node's owner, and takes its seal.
#[test]
fn an_own_copy_from_a_grandparent_whose_seal_is_not_yet_running_copies_it_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = running_plain_token(&url);
    store
        .write(&who, &branch("child", "fix-web", &["web"]))
        .unwrap();
    deploy(&store, &who, "child", 3, true);
    seal_token(&store, &who);
    deploy(&store, &who, "production", 4, false);

    let copied = copy_db_into(&store, &who, "child");

    assert_token_not_set(&store, &who, "child", &copied);
    only_what_production_ran(&url);
}

/// Once the seal runs, the copy is Not set as it always was.
#[test]
fn an_own_copy_made_after_its_owners_seal_deployed_copies_it_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = running_plain_token(&url);
    seal_token(&store, &who);
    deploy(&store, &who, "production", 3, true);

    let copied = copy_db_into(&store, &who, "fix-web");

    assert_token_not_set(&store, &who, "fix-web", &copied);
}

/// Deleting the copy's Not set value and discarding that brings it back Not set.
#[test]
fn discarding_a_delete_of_a_copy_made_before_its_owners_seal_ran_brings_it_back_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = running_plain_token(&url);
    seal_token(&store, &who);
    deploy(&store, &who, "production", 3, false);
    copy_db_into(&store, &who, "fix-web");
    deploy(&store, &who, "fix-web", 4, true);
    unset(&store, &who, "fix-web", "db.env.TOKEN");

    discard_token(&store, &who);

    let values = values(&store, &who, "fix-web", "db");
    assert_eq!(values["env"]["TOKEN"], json!({ "secret": false }));
    assert!(!values.to_string().contains(FAKE), "{values}");
    only_what_production_ran(&url);
}

/// A fix of a failed Deploy that sealed a value in a Service the fix Branch neither
/// copies nor uses keeps the plain value production still runs out of what the
/// Branch shares with it.
#[test]
fn a_fix_of_a_failed_seal_keeps_no_plain_value_in_what_it_shares_with_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_on(ConfigStore::open(&url, backend::key()).unwrap());
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(7)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("jobs").unwrap(),
                image: Some("jobs:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "production",
        &[("jobs.env.API_KEY", json!(FAKE))],
    );
    deploy(&store, &who, "production", 1, true);
    let jobs = id_of(&store, &who, "production", "jobs");
    set(
        &store,
        &who,
        "production",
        &[
            ("web.image", json!("web:2")),
            (
                "jobs.env.API_KEY",
                backend::sealed_secret(&uuid(2), &jobs, "API_KEY", 3),
            ),
        ],
    );
    deploy(&store, &who, "production", 2, false);
    let mut fix = branch("fix", "production", &["web"]);
    fix.fix = Some(DeploymentId::parse("00000000-0000-4000-8000-000000000102").unwrap());

    let made = store.write(&who, &fix).unwrap();

    assert_eq!(texts(&made.staged), ["web"]);
    only_what_production_ran(&url);
}

/// A fix that copies the Service whose Deploy failed to seal a value takes it Not set
/// and keeps no plain value.
#[test]
fn a_fix_copying_the_service_whose_seal_failed_takes_it_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_on(ConfigStore::open(&url, backend::key()).unwrap());
    set(
        &store,
        &who,
        "production",
        &[("web.env.API_KEY", json!(FAKE))],
    );
    deploy(&store, &who, "production", 1, true);
    set(
        &store,
        &who,
        "production",
        &[
            ("web.image", json!("web:2")),
            (
                "web.env.API_KEY",
                backend::sealed_secret(&uuid(2), &uuid(3), "API_KEY", 3),
            ),
        ],
    );
    deploy(&store, &who, "production", 2, false);
    let mut fix = branch("fix", "production", &[]);
    fix.fix = Some(DeploymentId::parse("00000000-0000-4000-8000-000000000102").unwrap());

    let made = store.write(&who, &fix).unwrap();

    assert_eq!(texts(&made.staged), ["web"]);
    assert_eq!(
        values(&store, &who, "fix", "web")["env"]["API_KEY"],
        json!({ "secret": false })
    );
    only_what_production_ran(&url);
}

/// A fix of a failed Deploy, made after the Parent sealed a value only in its
/// Working State, takes it Not set, never as the failed revision saved it.
#[test]
fn a_fix_made_after_its_parent_sealed_a_value_the_failed_revision_held_plain_takes_it_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_on(ConfigStore::open(&url, backend::key()).unwrap());
    set(
        &store,
        &who,
        "production",
        &[("web.env.API_KEY", json!(FAKE))],
    );
    deploy(&store, &who, "production", 1, true);
    set(&store, &who, "production", &[("web.image", json!("web:2"))]);
    deploy(&store, &who, "production", 2, false);
    set(
        &store,
        &who,
        "production",
        &[(
            "web.env.API_KEY",
            backend::sealed_secret(&uuid(2), &uuid(3), "API_KEY", 3),
        )],
    );
    let mut fix = branch("fix", "production", &[]);
    fix.fix = Some(DeploymentId::parse("00000000-0000-4000-8000-000000000102").unwrap());

    let made = store.write(&who, &fix).unwrap();

    assert_eq!(texts(&made.staged), ["web"]);
    let web = values(&store, &who, "fix", "web");
    assert_eq!(web["image"], json!("web:2"));
    assert_eq!(web["env"]["API_KEY"], json!({ "secret": false }));
    only_what_production_ran(&url);
}
