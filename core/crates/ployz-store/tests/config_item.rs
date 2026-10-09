#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Configs, their files and their mounts through the Store's interface only, on
//! SQLite and on Postgres (see `backend`). A file's text is a `String`, so non-UTF-8
//! never reaches the Store; the CLI refuses it before it builds a command.

use ployz_core::config::FileMode;
use ployz_core::{ConfigFileName, ConfigName, RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AttachConfig, Batch, BatchCommand, Change, ConfigId, ConfigItemQuery, ConfigItemView,
    ConfigListing, ConfigMountAt, ConfigStaged, ConfigStore, ConfigsQuery, CreateConfig,
    CreateProject, CreateService, CreateVolume, DeleteConfig, DetachConfig, Edit, EnvironmentId,
    EnvironmentRef, Mount, OrganizationId, ProjectId, ProjectName, PutConfigFile, RemoveConfigFile,
    RemoveService, RenameConfig, RenameService, ServiceLineageId, ServiceQuery, SettingPath,
    VolumeId, VolumeName, Written,
};
use serde_json::{Value, json};

mod backend;

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn texts<T: ToString>(items: &[T]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

fn config(name: &str) -> ConfigName {
    ConfigName::parse(name).unwrap()
}

fn file(name: &str) -> ConfigFileName {
    ConfigFileName::parse(name).unwrap()
}

fn service(name: &str) -> ServiceName {
    ServiceName::parse(name).unwrap()
}

/// Project `shop`: Services `web` (`KEY=web-key`) and `worker` (`KEY=worker-key`),
/// and Volume `data` mounted into `web` at `/data`.
fn shop() -> (ConfigStore, Actor) {
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
    for (n, name) in [(3, "web"), (4, "worker")] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: service(name),
                    image: Some("nginx:1".into()),
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
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                mounts: vec![Mount {
                    service: service("web"),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
    edit(
        &store,
        &who,
        vec![
            set("web.env.KEY", json!("web-key")),
            set("worker.env.KEY", json!("worker-key")),
        ],
    )
    .unwrap();
    (store, who)
}

fn set(path: &str, value: Value) -> Change {
    Change::Set {
        path: SettingPath::parse(path).unwrap(),
        value,
    }
}

fn edit(store: &ConfigStore, who: &Actor, changes: Vec<Change>) -> Result<(), RpcError> {
    store
        .write(
            who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes,
            },
        )
        .map(|_| ())
}

fn create_command(n: u8, name: &str, mounts: &[(&str, &str)]) -> CreateConfig {
    CreateConfig {
        id: ConfigId::parse(uuid(n)).unwrap(),
        environment: EnvironmentRef::default(),
        name: config(name),
        mounts: mounts
            .iter()
            .map(|(name, dir)| ConfigMountAt {
                service: service(name),
                dir: (*dir).into(),
            })
            .collect(),
    }
}

fn create(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    name: &str,
    mounts: &[(&str, &str)],
) -> Result<ConfigStaged, RpcError> {
    store.write(who, &create_command(n, name, mounts))
}

fn put_command(name: &str, path: &str, content: &str) -> PutConfigFile {
    PutConfigFile {
        environment: EnvironmentRef::default(),
        config: config(name),
        file: file(path),
        content: content.into(),
        mode: None,
        uid: None,
        gid: None,
    }
}

fn put(
    store: &ConfigStore,
    who: &Actor,
    name: &str,
    path: &str,
    content: &str,
) -> Result<ConfigStaged, RpcError> {
    store.write(who, &put_command(name, path, content))
}

fn attach_command(name: &str, to: &str, dir: &str) -> AttachConfig {
    AttachConfig {
        environment: EnvironmentRef::default(),
        service: service(to),
        config: config(name),
        dir: dir.into(),
    }
}

fn attach(
    store: &ConfigStore,
    who: &Actor,
    name: &str,
    to: &str,
    dir: &str,
) -> Result<ConfigStaged, RpcError> {
    store.write(who, &attach_command(name, to, dir))
}

fn item(store: &ConfigStore, who: &Actor, name: &str) -> Result<ConfigItemView, RpcError> {
    store.read(
        who,
        &ConfigItemQuery {
            environment: EnvironmentRef::default(),
            config: config(name).into(),
        },
    )
}

fn listed(store: &ConfigStore, who: &Actor) -> Vec<ConfigListing> {
    store.read(who, &ConfigsQuery::default()).unwrap().configs
}

fn values(store: &ConfigStore, who: &Actor, name: &str) -> Value {
    Value::Object(
        store
            .read(
                who,
                &ServiceQuery {
                    environment: EnvironmentRef::default(),
                    service: service(name),
                },
            )
            .unwrap()
            .values,
    )
}

/// Each file of a Config as (name, mode, uid, gid).
fn ownership(staged: &ConfigStaged) -> Vec<(String, String, u32, u32)> {
    staged
        .config
        .files
        .iter()
        .map(|file| {
            (
                file.name.to_string(),
                file.mode.to_string(),
                file.uid,
                file.gid,
            )
        })
        .collect()
}

fn refused(result: Result<impl std::fmt::Debug, RpcError>) -> RpcError {
    result.unwrap_err()
}

#[test]
fn a_put_file_defaults_to_read_only_root_and_a_rewrite_keeps_what_it_does_not_name() {
    let (store, who) = shop();
    let created = create(&store, &who, 10, "sentry", &[]).unwrap();
    assert_eq!(texts(&created.staged), ["configs.sentry"]);
    assert_eq!(created.config.id.to_string(), uuid(10));
    assert!(created.config.files.is_empty());
    assert_eq!(
        refused(create(&store, &who, 11, "sentry", &[])).code,
        RpcErrorCode::Conflict
    );

    let put_new = put(&store, &who, "sentry", "config.yml", "a: 1\n").unwrap();
    assert_eq!(texts(&put_new.staged), ["configs.sentry"]);
    assert_eq!(
        ownership(&put_new),
        [("config.yml".into(), "0444".into(), 0, 0)]
    );

    let owned = store
        .write(
            &who,
            &PutConfigFile {
                mode: Some(FileMode::parse("0640").unwrap()),
                uid: Some(1000),
                gid: Some(1001),
                ..put_command("sentry", "config.yml", "a: 2\n")
            },
        )
        .unwrap();
    assert_eq!(
        ownership(&owned),
        [("config.yml".into(), "0640".into(), 1000, 1001)]
    );

    let rewritten = put(&store, &who, "sentry", "config.yml", "a: 3\n").unwrap();
    assert_eq!(
        ownership(&rewritten),
        [("config.yml".into(), "0640".into(), 1000, 1001)]
    );
    assert_eq!(
        item(&store, &who, "sentry").unwrap().contents[&file("config.yml")],
        "a: 3\n"
    );

    let revision = rewritten.environment.revision;
    let same = put(&store, &who, "sentry", "config.yml", "a: 3\n").unwrap();
    assert!(same.staged.is_empty());
    assert_eq!(same.environment.revision, revision);

    assert_eq!(
        refused(put(&store, &who, "nope", "config.yml", "")).code,
        RpcErrorCode::NotFound
    );
}

#[test]
fn removing_a_file_drops_it_and_a_missing_one_lists_the_files_there_are() {
    let (store, who) = shop();
    create(&store, &who, 10, "sentry", &[]).unwrap();
    put(&store, &who, "sentry", "a.yml", "a").unwrap();
    put(&store, &who, "sentry", "b.yml", "b").unwrap();
    let remove = |path: &str| {
        store.write(
            &who,
            &RemoveConfigFile {
                environment: EnvironmentRef::default(),
                config: config("sentry"),
                file: file(path),
            },
        )
    };
    let removed = remove("a.yml").unwrap();
    assert_eq!(texts(&removed.staged), ["configs.sentry"]);
    assert_eq!(
        texts(
            &removed
                .config
                .files
                .iter()
                .map(|f| &f.name)
                .collect::<Vec<_>>()
        ),
        ["b.yml"]
    );

    let missing = refused(remove("a.yml"));
    assert_eq!(missing.code, RpcErrorCode::NotFound);
    assert!(missing.message.contains("a.yml"), "{}", missing.message);
    assert_eq!(missing.details["valid_children"], json!(["b.yml"]));
    assert_eq!(missing.details["did_you_mean"], json!("b.yml"));
}

#[test]
fn a_renamed_config_keeps_its_files_and_mounts() {
    let (store, who) = shop();
    create(&store, &who, 10, "sentry", &[("web", "/etc/sentry")]).unwrap();
    create(&store, &who, 11, "taken", &[]).unwrap();
    put(&store, &who, "sentry", "config.yml", "a: 1\n").unwrap();
    let rename = |from: &str, to: &str| {
        store.write(
            &who,
            &RenameConfig {
                environment: EnvironmentRef::default(),
                config: config(from),
                name: config(to),
            },
        )
    };
    let renamed = rename("sentry", "relay").unwrap();
    assert_eq!(texts(&renamed.staged), ["configs.relay"]);
    assert_eq!(renamed.config.id.to_string(), uuid(10));
    assert_eq!(
        values(&store, &who, "web")["configs"],
        json!({ "relay": "/etc/sentry" })
    );
    let relay = item(&store, &who, "relay").unwrap();
    assert_eq!(relay.contents[&file("config.yml")], "a: 1\n");
    assert_eq!(relay.lineage.to_string(), uuid(10));
    assert_eq!(
        refused(item(&store, &who, "sentry")).code,
        RpcErrorCode::NotFound
    );

    assert_eq!(
        refused(rename("relay", "taken")).code,
        RpcErrorCode::Conflict
    );
    assert!(rename("relay", "relay").unwrap().staged.is_empty());
}

#[test]
fn deleting_a_config_unmounts_it_from_every_service_in_the_same_write() {
    let (store, who) = shop();
    let created = create(
        &store,
        &who,
        10,
        "sentry",
        &[("web", "/etc/sentry"), ("worker", "/etc/sentry")],
    )
    .unwrap();
    assert_eq!(
        texts(&created.staged),
        [
            "configs.sentry",
            "web.configs.sentry",
            "worker.configs.sentry"
        ]
    );
    create(&store, &who, 11, "other", &[("web", "/etc/other")]).unwrap();
    let before = listed(&store, &who);
    assert_eq!(before.len(), 2);

    let revision = item(&store, &who, "sentry").unwrap().environment.revision;
    let deleted = store
        .write(
            &who,
            &DeleteConfig {
                environment: EnvironmentRef::default(),
                config: config("sentry"),
            },
        )
        .unwrap();
    assert_eq!(
        texts(&deleted.staged),
        [
            "configs.sentry",
            "web.configs.sentry",
            "worker.configs.sentry"
        ]
    );
    assert_eq!(deleted.environment.revision.0, revision.0 + 1);
    assert_eq!(
        values(&store, &who, "web")["configs"],
        json!({ "other": "/etc/other" })
    );
    assert_eq!(values(&store, &who, "worker").get("configs"), None);
    let after = listed(&store, &who);
    assert_eq!(
        texts(&after.iter().map(|l| &l.config.name).collect::<Vec<_>>()),
        ["other"]
    );
    assert_eq!(
        refused(item(&store, &who, "sentry")).code,
        RpcErrorCode::NotFound
    );
}

#[test]
fn a_config_mounts_moves_and_unmounts_while_it_stays() {
    let (store, who) = shop();
    create(&store, &who, 10, "sentry", &[]).unwrap();
    let mounted = attach(&store, &who, "sentry", "web", "/etc/sentry").unwrap();
    assert_eq!(texts(&mounted.staged), ["web.configs.sentry"]);
    assert_eq!(
        values(&store, &who, "web")["configs"],
        json!({ "sentry": "/etc/sentry" })
    );
    assert!(
        attach(&store, &who, "sentry", "web", "/etc/sentry")
            .unwrap()
            .staged
            .is_empty()
    );
    attach(&store, &who, "sentry", "web", "/srv/sentry").unwrap();
    assert_eq!(
        values(&store, &who, "web")["configs"],
        json!({ "sentry": "/srv/sentry" })
    );

    for (to, dir, code) in [
        ("web", "relative", RpcErrorCode::InvalidArgument),
        ("nope", "/etc/sentry", RpcErrorCode::NotFound),
    ] {
        assert_eq!(refused(attach(&store, &who, "sentry", to, dir)).code, code);
    }
    assert_eq!(
        refused(attach(&store, &who, "nope", "web", "/etc/nope")).code,
        RpcErrorCode::NotFound
    );

    let detach = || {
        store.write(
            &who,
            &DetachConfig {
                environment: EnvironmentRef::default(),
                service: service("web"),
                config: config("sentry"),
            },
        )
    };
    assert_eq!(texts(&detach().unwrap().staged), ["web.configs.sentry"]);
    assert_eq!(values(&store, &who, "web").get("configs"), None);
    assert!(detach().unwrap().staged.is_empty());
    assert_eq!(listed(&store, &who).len(), 1);
}

#[test]
fn a_file_holds_at_most_256_kb() {
    let (store, who) = shop();
    create(&store, &who, 10, "sentry", &[]).unwrap();
    let cap = 256 * 1024;
    let full = put(&store, &who, "sentry", "full.txt", &"x".repeat(cap)).unwrap();
    assert_eq!(full.config.files[0].bytes, cap);

    let over = refused(put(
        &store,
        &who,
        "sentry",
        "over.txt",
        &"x".repeat(cap + 1),
    ));
    assert_eq!(over.code, RpcErrorCode::InvalidArgument);
    assert!(over.message.contains("256 KB"), "{}", over.message);
    assert_eq!(over.details["max_bytes"], json!(cap));
    assert_eq!(item(&store, &who, "sentry").unwrap().contents.len(), 1);
}

#[test]
fn a_file_name_is_a_relative_path_of_at_most_four_segments() {
    for bad in [
        "/etc/a",
        "..",
        "a/../b",
        "a//b",
        "a/",
        "",
        "a/b/c/d/e",
        "./a",
    ] {
        assert!(ConfigFileName::parse(bad).is_err(), "{bad}");
        let command = json!({
            "config": "sentry", "file": bad, "content": "x"
        });
        assert!(
            serde_json::from_value::<PutConfigFile>(command).is_err(),
            "{bad}"
        );
    }

    let (store, who) = shop();
    create(&store, &who, 10, "sentry", &[]).unwrap();
    for good in ["conf.d/site.conf", ".htpasswd", "a/b/c/d.txt"] {
        put(&store, &who, "sentry", good, "x").unwrap();
    }
    let contents = item(&store, &who, "sentry").unwrap().contents;
    assert_eq!(
        texts(&contents.keys().collect::<Vec<_>>()),
        [".htpasswd", "a/b/c/d.txt", "conf.d/site.conf"]
    );
}

#[test]
fn a_reference_names_a_service_that_exists() {
    let (store, who) = shop();
    create(&store, &who, 10, "sentry", &[]).unwrap();

    let unknown = refused(put(
        &store,
        &who,
        "sentry",
        "a.yml",
        "port: ${{ snuba.PORT }}",
    ));
    assert_eq!(unknown.code, RpcErrorCode::InvalidArgument);
    assert!(unknown.message.contains("snuba"), "{}", unknown.message);

    let bare = refused(put(&store, &who, "sentry", "a.yml", "key: ${{ KEY }}"));
    assert_eq!(bare.code, RpcErrorCode::InvalidArgument);
    assert!(
        bare.message.contains("Configs are shared"),
        "{}",
        bare.message
    );

    let malformed = refused(put(&store, &who, "sentry", "a.yml", "x: ${{ a-b }}"));
    assert_eq!(malformed.code, RpcErrorCode::InvalidArgument);

    let unfinished = refused(put(&store, &who, "sentry", "a.yml", "x: ${{ web.KEY"));
    assert_eq!(unfinished.code, RpcErrorCode::InvalidArgument);

    let no_variable = refused(put(&store, &who, "sentry", "a.yml", "x: ${{ web.NOPE }}"));
    assert_eq!(no_variable.code, RpcErrorCode::InvalidArgument);
    assert!(
        no_variable.message.contains("NOPE"),
        "{}",
        no_variable.message
    );

    assert!(item(&store, &who, "sentry").unwrap().contents.is_empty());
    put(
        &store,
        &who,
        "sentry",
        "a.yml",
        "x: ${{ web.KEY }} $${{ literal }}",
    )
    .unwrap();
}

#[test]
fn a_mount_directory_holds_one_volume_or_config() {
    let (store, who) = shop();
    create(&store, &who, 10, "sentry", &[("web", "/etc/sentry")]).unwrap();
    create(&store, &who, 11, "other", &[]).unwrap();

    let over_config = refused(attach(&store, &who, "other", "web", "/etc/sentry"));
    assert_eq!(over_config.code, RpcErrorCode::Conflict);
    assert!(
        over_config.message.contains("Config sentry"),
        "{}",
        over_config.message
    );

    let over_volume = refused(attach(&store, &who, "other", "web", "/data"));
    assert_eq!(over_volume.code, RpcErrorCode::Conflict);
    assert!(
        over_volume.message.contains("Volume data"),
        "{}",
        over_volume.message
    );

    let on_create = refused(create(&store, &who, 12, "third", &[("web", "/data")]));
    assert_eq!(on_create.code, RpcErrorCode::Conflict);
    assert_eq!(listed(&store, &who).len(), 2);

    attach(&store, &who, "other", "worker", "/etc/sentry").unwrap();

    for alias in [
        "/etc/sentry/",
        "/etc//sentry",
        "/etc/./sentry",
        "/etc/x/../sentry",
        "/",
    ] {
        let refused = refused(attach(&store, &who, "other", "web", alias));
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{alias}");
    }
}

#[test]
fn two_mounts_may_not_land_files_on_one_path() {
    let (store, who) = shop();
    create(&store, &who, 10, "outer", &[("web", "/etc")]).unwrap();
    create(&store, &who, 11, "inner", &[("web", "/etc/app")]).unwrap();
    put(&store, &who, "outer", "app/site.conf", "a").unwrap();

    let clash = refused(put(&store, &who, "inner", "site.conf", "b"));
    assert_eq!(clash.code, RpcErrorCode::Conflict);
    assert!(
        clash.message.contains("/etc/app/site.conf"),
        "{}",
        clash.message
    );
    assert!(item(&store, &who, "inner").unwrap().contents.is_empty());

    put(&store, &who, "inner", "other.conf", "b").unwrap();

    create(&store, &who, 12, "loose", &[]).unwrap();
    put(&store, &who, "loose", "conf", "a").unwrap();
    let folder = refused(put(&store, &who, "loose", "conf/site.yml", "b"));
    assert_eq!(folder.code, RpcErrorCode::InvalidArgument);
    assert!(
        folder.message.contains("conf/site.yml"),
        "{}",
        folder.message
    );
}

#[test]
fn a_service_or_variable_a_config_references_cannot_be_removed() {
    let (store, who) = shop();
    create(&store, &who, 10, "sentry", &[]).unwrap();
    put(
        &store,
        &who,
        "sentry",
        "a.yml",
        "key: ${{ worker.KEY }}\nhost: ${{ worker.PLOYZ_PRIVATE_DOMAIN }}",
    )
    .unwrap();

    let remove = refused(store.write(
        &who,
        &RemoveService {
            environment: EnvironmentRef::default(),
            service: service("worker"),
        },
    ));
    assert_eq!(remove.code, RpcErrorCode::Conflict);
    assert!(
        remove.message.contains("Config sentry"),
        "{}",
        remove.message
    );
    assert!(remove.message.contains("worker"), "{}", remove.message);

    let unset = refused(edit(
        &store,
        &who,
        vec![Change::Unset {
            path: SettingPath::parse("worker.env.KEY").unwrap(),
        }],
    ));
    assert_eq!(unset.code, RpcErrorCode::Conflict);
    assert!(unset.message.contains("worker.KEY"), "{}", unset.message);
    assert_eq!(
        values(&store, &who, "worker")["env"]["KEY"],
        json!("worker-key")
    );
}

#[test]
fn renaming_a_referenced_service_rewrites_the_file_under_its_new_name() {
    let (store, who) = shop();
    create(&store, &who, 10, "sentry", &[]).unwrap();
    put(&store, &who, "sentry", "a.yml", "key: ${{ worker.KEY }}").unwrap();
    store
        .write(
            &who,
            &RenameService {
                environment: EnvironmentRef::default(),
                service: service("worker"),
                name: service("jobs"),
            },
        )
        .unwrap();
    let sentry = item(&store, &who, "sentry").unwrap();
    assert_eq!(sentry.contents[&file("a.yml")], "key: ${{ jobs.KEY }}");
    let summary = &sentry.config.config.files[0];
    assert_eq!(summary.references, ["jobs"]);
    assert_eq!(summary.bytes, "key: ${{ jobs.KEY }}".len());
}

#[test]
fn configs_that_reference_each_others_mounting_services_are_accepted() {
    let (store, who) = shop();
    create(&store, &who, 10, "web-conf", &[("web", "/etc/web")]).unwrap();
    create(
        &store,
        &who,
        11,
        "worker-conf",
        &[("worker", "/etc/worker")],
    )
    .unwrap();
    put(&store, &who, "web-conf", "a.yml", "${{ worker.KEY }}").unwrap();
    put(&store, &who, "worker-conf", "a.yml", "${{ web.KEY }}").unwrap();
    let configs = listed(&store, &who);
    assert_eq!(configs[0].config.files[0].references, ["worker"]);
    assert_eq!(configs[1].config.files[0].references, ["web"]);
}

#[test]
fn every_config_command_runs_inside_a_batch() {
    let (store, who) = shop();
    let commands = vec![
        BatchCommand::CreateConfig(create_command(10, "sentry", &[])),
        BatchCommand::PutConfigFile(put_command("sentry", "a.yml", "a: ${{ web.KEY }}")),
        BatchCommand::PutConfigFile(put_command("sentry", "b.yml", "b")),
        BatchCommand::RemoveConfigFile(RemoveConfigFile {
            environment: EnvironmentRef::default(),
            config: config("sentry"),
            file: file("b.yml"),
        }),
        BatchCommand::RenameConfig(RenameConfig {
            environment: EnvironmentRef::default(),
            config: config("sentry"),
            name: config("relay"),
        }),
        BatchCommand::AttachConfig(attach_command("relay", "web", "/etc/relay")),
        BatchCommand::AttachConfig(attach_command("relay", "worker", "/etc/relay")),
        BatchCommand::DetachConfig(DetachConfig {
            environment: EnvironmentRef::default(),
            service: service("worker"),
            config: config("relay"),
        }),
    ];
    let batch = Batch {
        environment: EnvironmentRef::default(),
        commands,
        expect: None,
    };
    let results = store.write(&who, &batch).unwrap().results;
    assert!(matches!(
        results.as_slice(),
        [
            Written::Config(_),
            Written::Config(_),
            Written::Config(_),
            Written::Config(_),
            Written::ConfigRenamed(_),
            Written::Config(_),
            Written::Config(_),
            Written::Config(_),
        ]
    ));
    let relay = item(&store, &who, "relay").unwrap();
    assert_eq!(
        relay.config.mounts,
        [ConfigMountAt {
            service: service("web"),
            dir: "/etc/relay".into()
        }]
    );
    assert_eq!(texts(&relay.contents.keys().collect::<Vec<_>>()), ["a.yml"]);

    let deleted = store
        .write(
            &who,
            &Batch {
                environment: EnvironmentRef::default(),
                commands: vec![BatchCommand::DeleteConfig(DeleteConfig {
                    environment: EnvironmentRef::default(),
                    config: config("relay"),
                })],
                expect: None,
            },
        )
        .unwrap()
        .results;
    assert!(matches!(deleted.as_slice(), [Written::ConfigRemoved(_)]));
    assert!(listed(&store, &who).is_empty());

    let failing = Batch {
        environment: EnvironmentRef::default(),
        commands: vec![
            BatchCommand::CreateConfig(create_command(11, "sentry", &[])),
            BatchCommand::PutConfigFile(put_command("sentry", "a.yml", "${{ snuba.PORT }}")),
        ],
        expect: None,
    };
    assert_eq!(
        refused(store.write(&who, &failing)).code,
        RpcErrorCode::InvalidArgument
    );
    assert!(listed(&store, &who).is_empty());
}

#[test]
fn the_config_list_shows_files_sizes_and_mounts_but_never_resolved_values() {
    let (store, who) = shop();
    create(
        &store,
        &who,
        10,
        "sentry",
        &[("web", "/etc/sentry"), ("worker", "/srv/sentry")],
    )
    .unwrap();
    let text = "key: ${{ worker.KEY }}\n";
    put(&store, &who, "sentry", "conf.d/a.yml", text).unwrap();
    put(&store, &who, "sentry", ".htpasswd", "user:hash").unwrap();

    let view = store.read(&who, &ConfigsQuery::default()).unwrap();
    let [listing] = view.configs.as_slice() else {
        panic!("one Config: {:?}", view.configs)
    };
    assert_eq!(listing.config.name.as_str(), "sentry");
    assert_eq!(
        listing
            .config
            .files
            .iter()
            .map(|file| (file.name.to_string(), file.bytes))
            .collect::<Vec<_>>(),
        [(".htpasswd".into(), 9), ("conf.d/a.yml".into(), text.len())]
    );
    assert_eq!(
        listing.mounts,
        [
            ConfigMountAt {
                service: service("web"),
                dir: "/etc/sentry".into()
            },
            ConfigMountAt {
                service: service("worker"),
                dir: "/srv/sentry".into()
            },
        ]
    );
    assert!(!listing.deployed);

    let wire = serde_json::to_string(&view).unwrap();
    assert!(!wire.contains("worker-key"), "{wire}");
    let one = serde_json::to_string(&item(&store, &who, "sentry").unwrap()).unwrap();
    assert!(!one.contains("worker-key"), "{one}");
    assert!(one.contains("${{ worker.KEY }}"), "{one}");
}
