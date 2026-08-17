use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf, str::FromStr};

use ployz::{
    connect::resolve_connections,
    context::{
        Config, ConfigError, Connection, ConnectionSource, Context, ContextError, SshDestination,
        select_connections,
    },
};
use ployz_core::MachineId;

#[test]
fn config_round_trips_ordered_connections_with_private_permissions() {
    let root = std::env::temp_dir().join(format!("ployz-context-{}", std::process::id()));
    let path = root.join("nested/config.yaml");
    let _ = fs::remove_dir_all(&root);

    let connections = vec![
        Connection::ssh(SshDestination::parse("root@example.com:2222").unwrap())
            .with_ssh_key_file("~/.ssh/id_ed25519")
            .unwrap()
            .with_machine_id(MachineId::parse("0123456789abcdef0123456789abcdef").unwrap()),
        Connection::unix("/run/ployz/ployz.sock").unwrap(),
    ];
    let expected = Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([("prod".into(), Context { connections })]),
    );

    expected.save().unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    expected.save().unwrap();
    let actual = Config::load(&path).unwrap();

    assert_eq!(actual, expected);
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let yaml = fs::read_to_string(&path).unwrap();
    assert!(!yaml.contains("ssh_go"));
    assert!(!yaml.contains("ssh_cli"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn direct_connections_accept_only_the_reduced_transport_set() {
    for (input, canonical) in [
        ("root@example.com", "ssh://root@example.com"),
        ("ssh://root@example.com:2222", "ssh://root@example.com:2222"),
        ("ssh://root@[::1]:2222", "ssh://root@[::1]:2222"),
        ("tcp://127.0.0.1:51000", "tcp://127.0.0.1:51000"),
        (
            "unix:///run/ployz/ployz.sock",
            "unix:///run/ployz/ployz.sock",
        ),
    ] {
        assert_eq!(Connection::from_str(input).unwrap().to_string(), canonical);
    }

    for input in [
        "",
        "example.com",
        "-oProxyCommand=bad@example.com",
        "ssh://ssh://root@example.com",
        "root@example.com/path",
        "ssh://root@example.com:not-a-port",
        "ssh://root@example.com:22:33",
        "tcp://localhost",
        "unix://relative.sock",
        "ssh+go://root@example.com",
        "ssh+cli://root@example.com",
    ] {
        assert!(Connection::from_str(input).is_err(), "accepted {input:?}");
    }
}

#[test]
fn connection_sources_follow_direct_context_and_local_precedence() {
    let prod = Connection::tcp("127.0.0.1:51000".parse().unwrap());
    let dev = Connection::tcp("127.0.0.1:51001".parse().unwrap());
    let config = Config::new(
        "/tmp/config.yaml",
        Some("prod".into()),
        BTreeMap::from([
            (
                "dev".into(),
                Context {
                    connections: vec![dev.clone()],
                },
            ),
            (
                "prod".into(),
                Context {
                    connections: vec![prod.clone()],
                },
            ),
        ]),
    );
    let direct = Connection::unix("/tmp/direct.sock").unwrap();

    let selected = select_connections(
        Some(direct.clone()),
        Some(&config),
        Some("dev"),
        true,
        "/run/ployz/ployz.sock",
    )
    .unwrap();
    assert_eq!(selected.source, ConnectionSource::Direct);
    assert_eq!(selected.connections, vec![direct]);

    let selected = select_connections(
        None,
        Some(&config),
        Some("dev"),
        true,
        "/run/ployz/ployz.sock",
    )
    .unwrap();
    assert_eq!(selected.source, ConnectionSource::Context("dev".into()));
    assert_eq!(selected.connections, vec![dev]);

    let selected =
        select_connections(None, Some(&config), None, true, "/run/ployz/ployz.sock").unwrap();
    assert_eq!(selected.source, ConnectionSource::Context("prod".into()));
    assert_eq!(selected.connections, vec![prod]);

    let selected =
        select_connections(None, None, Some("ignored"), true, "/run/ployz/ployz.sock").unwrap();
    assert_eq!(selected.source, ConnectionSource::LocalSocket);
    assert_eq!(
        selected.connections,
        vec![Connection::unix("/run/ployz/ployz.sock").unwrap()]
    );

    assert!(
        select_connections(
            None,
            Some(&Config::new("/tmp/empty.yaml", None, BTreeMap::new())),
            None,
            true,
            "/run/ployz/ployz.sock",
        )
        .is_err()
    );
}

#[test]
fn stored_connections_reject_missing_multiple_malformed_and_removed_transports() {
    for yaml in [
        "{}",
        "ssh: root@example.com\ntcp: 127.0.0.1:51000",
        "tcp: localhost",
        "unix: relative.sock",
        "tcp: 127.0.0.1:51000\nssh_key_file: /tmp/key",
        "ssh_go: root@example.com",
        "ssh_cli: root@example.com",
    ] {
        assert!(
            serde_norway::from_str::<Connection>(yaml).is_err(),
            "accepted {yaml:?}"
        );
    }
    assert!(
        Connection::tcp("127.0.0.1:51000".parse().unwrap())
            .with_ssh_key_file("/tmp/key")
            .is_err()
    );
}

#[test]
fn selecting_a_connection_moves_only_that_entry_to_the_front() {
    let mut context = Context {
        connections: [51000, 51001, 51002, 51003]
            .map(|port| Connection::tcp(format!("127.0.0.1:{port}").parse().unwrap()))
            .into(),
    };
    let original = context.clone();

    assert!(context.select_connection(2));
    assert_eq!(
        context
            .connections
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        [
            "tcp://127.0.0.1:51002",
            "tcp://127.0.0.1:51000",
            "tcp://127.0.0.1:51001",
            "tcp://127.0.0.1:51003",
        ]
    );

    let mut invalid = original.clone();
    assert!(!invalid.select_connection(9));
    assert_eq!(invalid, original);
}

#[test]
fn dropping_a_machine_leaves_only_the_remaining_connections() {
    let mut context = Context {
        connections: vec![
            ssh_machine("ord1.example.com", '1'),
            ssh_machine("ord2.example.com", '2'),
            ssh_machine("ord3.example.com", '3'),
        ],
    };

    context.drop_machine(&machine_id('2'));

    assert_eq!(
        context
            .connections
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["ssh://root@ord1.example.com", "ssh://root@ord3.example.com",]
    );
}

#[test]
fn dropping_the_default_machine_leaves_the_next_connection_as_default() {
    let mut context = Context {
        connections: vec![
            ssh_machine("ord1.example.com", '1'),
            ssh_machine("ord2.example.com", '2'),
            ssh_machine("ord3.example.com", '3'),
        ],
    };

    context.drop_machine(&machine_id('1'));

    assert_eq!(
        context
            .connections
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["ssh://root@ord2.example.com", "ssh://root@ord3.example.com",]
    );
}

#[test]
fn dropping_a_machine_the_context_never_named_is_a_no_op() {
    let mut context = Context {
        connections: vec![
            ssh_machine("ord1.example.com", '1'),
            Connection::ssh(SshDestination::parse("root@manual.example.com").unwrap()),
        ],
    };
    let before = context.clone();

    context.drop_machine(&machine_id('9'));

    assert_eq!(context, before);
}

#[test]
fn filesystem_resolution_bypasses_config_and_limits_the_local_fallback() {
    let root = std::env::temp_dir().join(format!("ployz-resolution-{}", std::process::id()));
    let config = root.join("config.yaml");
    let socket = root.join("ployz.sock");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(&config, "not: [valid").unwrap();
    fs::write(&socket, "placeholder").unwrap();

    let direct = resolve_connections(
        &config,
        Some("tcp://127.0.0.1:51000"),
        Some("ignored"),
        &socket,
    )
    .unwrap();
    assert_eq!(direct.source, ConnectionSource::Direct);

    assert!(resolve_connections(&config, None, None, &socket).is_err());
    fs::remove_file(&config).unwrap();
    Config::new(
        &config,
        Some("prod".into()),
        BTreeMap::from([
            (
                "dev".into(),
                Context {
                    connections: vec![Connection::tcp("127.0.0.1:51001".parse().unwrap())],
                },
            ),
            (
                "prod".into(),
                Context {
                    connections: vec![Connection::tcp("127.0.0.1:51000".parse().unwrap())],
                },
            ),
        ]),
    )
    .save()
    .unwrap();
    let overridden = resolve_connections(&config, None, Some("dev"), &socket).unwrap();
    assert_eq!(overridden.source, ConnectionSource::Context("dev".into()));
    assert_eq!(
        Config::load(&config).unwrap().current_context(),
        Some("prod")
    );

    fs::remove_file(&config).unwrap();
    let fallback = resolve_connections(&config, None, None, &socket).unwrap();
    assert_eq!(fallback.source, ConnectionSource::LocalSocket);

    fs::remove_dir_all(root).unwrap();
}

fn ssh_machine(host: &str, seed: char) -> Connection {
    Connection::ssh(SshDestination::parse(format!("root@{host}")).unwrap())
        .with_machine_id(machine_id(seed))
}

fn machine_id(seed: char) -> MachineId {
    MachineId::parse(seed.to_string().repeat(32)).unwrap()
}

#[test]
fn config_cannot_store_current_context_as_empty_string() {
    let root = std::env::temp_dir().join(format!("ployz-blank-current-{}", std::process::id()));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);

    Config::new(&path, Some(String::new()), BTreeMap::new())
        .save()
        .unwrap();
    let yaml = fs::read_to_string(&path).unwrap();
    assert!(
        !yaml.contains("current_context:"),
        "empty current context was stored: {yaml}"
    );
    assert_eq!(Config::load(&path).unwrap().current_context(), None);

    let mut config = Config::load(&path).unwrap();
    config.set_current_context(Some(String::new()));
    config.save().unwrap();
    let yaml = fs::read_to_string(&path).unwrap();
    assert!(
        !yaml.contains("current_context:"),
        "empty current context was stored: {yaml}"
    );
    assert_eq!(config.current_context(), None);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_current_context_yaml_fails_to_load() {
    let root =
        std::env::temp_dir().join(format!("ployz-empty-current-yaml-{}", std::process::id()));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(&path, "current_context: \"\"\n").unwrap();

    let error = Config::load(&path).unwrap_err();
    assert!(
        matches!(error, ConfigError::EmptyCurrentContext(ref error_path) if error_path == &path),
        "{error}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn absent_or_null_current_context_yaml_loads_as_none() {
    let root = std::env::temp_dir().join(format!("ployz-null-current-yaml-{}", std::process::id()));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    fs::write(&path, "contexts: {}\n").unwrap();
    assert_eq!(Config::load(&path).unwrap().current_context(), None);

    fs::write(&path, "current_context: null\n").unwrap();
    assert_eq!(Config::load(&path).unwrap().current_context(), None);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn load_or_empty_with_no_file_has_no_current_context() {
    let path =
        std::env::temp_dir().join(format!("ployz-missing-config-{}.yaml", std::process::id()));
    let _ = fs::remove_file(&path);

    let config = Config::load_or_empty(&path).unwrap();
    assert_eq!(config.current_context(), None);
}

#[test]
fn selecting_connections_with_no_current_context_is_no_current_context() {
    let prod = Connection::tcp("127.0.0.1:51000".parse().unwrap());
    let path = PathBuf::from("/tmp/config.yaml");
    let config = Config::new(
        &path,
        None,
        BTreeMap::from([(
            "prod".into(),
            Context {
                connections: vec![prod],
            },
        )]),
    );

    assert_eq!(
        select_connections(None, Some(&config), None, true, "/run/ployz/ployz.sock"),
        Err(ContextError::NoCurrentContext(path))
    );
}

#[test]
fn selecting_connections_with_a_missing_name_is_context_not_found() {
    let prod = Connection::tcp("127.0.0.1:51000".parse().unwrap());
    let path = PathBuf::from("/tmp/config.yaml");
    let config = Config::new(
        &path,
        Some("gone".into()),
        BTreeMap::from([(
            "prod".into(),
            Context {
                connections: vec![prod],
            },
        )]),
    );

    assert_eq!(
        select_connections(None, Some(&config), None, true, "/run/ployz/ployz.sock"),
        Err(ContextError::ContextNotFound {
            name: "gone".into(),
            path,
        })
    );
}
