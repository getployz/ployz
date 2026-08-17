use std::{collections::BTreeMap, fs, process::Command};

use ployz::context::{Config, Connection, Context};

#[test]
fn context_commands_list_show_and_persist_an_explicit_selection() {
    let root = std::env::temp_dir().join(format!("ployz-context-cli-{}", std::process::id()));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([
            (
                "dev".into(),
                Context {
                    connections: vec![Connection::unix("/tmp/dev.sock").unwrap()],
                },
            ),
            (
                "prod".into(),
                Context {
                    connections: vec![Connection::unix("/tmp/prod.sock").unwrap()],
                },
            ),
        ]),
    )
    .save()
    .unwrap();

    let selected = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--ployz-config",
            path.to_str().unwrap(),
            "ctx",
            "use",
            "dev",
        ])
        .output()
        .unwrap();
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    assert_eq!(Config::load(&path).unwrap().current_context(), Some("dev"));

    let listed = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["ctx", "ls", "--ployz-config", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(listed.contains("dev"));
    assert!(listed.contains("prod"));

    let shown = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["ctx", "show", "--ployz-config", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert_eq!(String::from_utf8(shown.stdout).unwrap().trim(), "dev");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ctx_connection_shows_the_current_default_without_a_terminal() {
    let root =
        std::env::temp_dir().join(format!("ployz-ctx-connection-show-{}", std::process::id()));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    let before = Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([(
            "prod".into(),
            Context {
                connections: vec![
                    Connection::unix("/tmp/prod-a.sock").unwrap(),
                    Connection::unix("/tmp/prod-b.sock").unwrap(),
                ],
            },
        )]),
    );
    before.save().unwrap();

    let shown = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "ctx",
            "connection",
            "--ployz-config",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert_eq!(
        String::from_utf8(shown.stdout).unwrap().trim(),
        "unix:///tmp/prod-a.sock"
    );
    assert_eq!(Config::load(&path).unwrap(), before);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ctx_connection_selects_and_persists_across_invocations() {
    let root = std::env::temp_dir().join(format!(
        "ployz-ctx-connection-select-{}",
        std::process::id()
    ));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([(
            "prod".into(),
            Context {
                connections: vec![
                    Connection::unix("/tmp/prod-a.sock").unwrap(),
                    Connection::unix("/tmp/prod-b.sock").unwrap(),
                ],
            },
        )]),
    )
    .save()
    .unwrap();

    let selected = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "ctx",
            "connection",
            "unix:///tmp/prod-b.sock",
            "--ployz-config",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );

    let shown = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "ctx",
            "connection",
            "--ployz-config",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert_eq!(
        String::from_utf8(shown.stdout).unwrap().trim(),
        "unix:///tmp/prod-b.sock"
    );
    assert_eq!(
        Config::load(&path)
            .unwrap()
            .contexts
            .get("prod")
            .unwrap()
            .connections
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["unix:///tmp/prod-b.sock", "unix:///tmp/prod-a.sock"]
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ctx_connection_rejects_an_unknown_connection_without_mutating() {
    let root = std::env::temp_dir().join(format!(
        "ployz-ctx-connection-unknown-{}",
        std::process::id()
    ));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    let before = Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([(
            "prod".into(),
            Context {
                connections: vec![Connection::unix("/tmp/prod-a.sock").unwrap()],
            },
        )]),
    );
    before.save().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "ctx",
            "connection",
            "unix:///tmp/missing.sock",
            "--ployz-config",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        r#"connection "unix:///tmp/missing.sock" not found"#
    );
    assert_eq!(Config::load(&path).unwrap(), before);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ctx_connection_help_describes_show_and_select() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["ctx", "connection", "--help"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(
        help.contains("Show or select the default connection"),
        "{help}"
    );
    assert!(help.contains("[connection]"), "{help}");
}

#[test]
fn explicit_config_beats_environment_and_interactive_errors_do_not_mutate() {
    let root = std::env::temp_dir().join(format!("ployz-context-env-{}", std::process::id()));
    let flag_path = root.join("flag.yaml");
    let env_path = root.join("env.yaml");
    let _ = fs::remove_dir_all(&root);
    let contexts = BTreeMap::from([
        (
            "dev".into(),
            Context {
                connections: vec![Connection::unix("/tmp/dev.sock").unwrap()],
            },
        ),
        (
            "prod".into(),
            Context {
                connections: vec![Connection::unix("/tmp/prod.sock").unwrap()],
            },
        ),
    ]);
    Config::new(&flag_path, Some("prod".into()), contexts.clone())
        .save()
        .unwrap();
    Config::new(&env_path, Some("prod".into()), contexts)
        .save()
        .unwrap();

    let selected = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--ployz-config",
            flag_path.to_str().unwrap(),
            "ctx",
            "use",
            "dev",
        ])
        .env("PLOYZ_CONFIG", &env_path)
        .output()
        .unwrap();
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    assert_eq!(
        Config::load(&flag_path).unwrap().current_context(),
        Some("dev")
    );
    assert_eq!(
        Config::load(&env_path).unwrap().current_context(),
        Some("prod")
    );

    let before = Config::load(&flag_path).unwrap();
    for args in [vec!["ctx"], vec!["ctx", "use"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args(args)
            .args(["--ployz-config", flag_path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(Config::load(&flag_path).unwrap(), before);
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_filename_only_config_override_saves_in_the_current_directory() {
    let root = std::env::temp_dir().join(format!("ployz-relative-config-{}", std::process::id()));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([
            ("dev".into(), Context::default()),
            ("prod".into(), Context::default()),
        ]),
    )
    .save()
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .current_dir(&root)
        .args(["--ployz-config", "config.yaml", "ctx", "use", "dev"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(Config::load(&path).unwrap().current_context(), Some("dev"));
    fs::remove_dir_all(root).unwrap();
}
