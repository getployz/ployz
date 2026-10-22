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

    assert_eq!(current_context(&path), Some("dev".into()));

    fs::remove_dir_all(root).unwrap();
}

/// The current context as `ctx ls --json` reports it.
fn current_context(path: &std::path::Path) -> Option<String> {
    let listed = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "ctx",
            "ls",
            "--json",
            "--ployz-config",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let contexts: Vec<ListedContext> =
        serde_json::from_value(listed.get("contexts").unwrap().clone()).unwrap();
    contexts
        .into_iter()
        .find(|context| context.current)
        .map(|context| context.name)
}

#[derive(serde::Deserialize)]
struct ListedContext {
    name: String,
    current: bool,
}

#[test]
fn ctx_ls_json_lists_connections_with_the_default_first_without_mutating() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
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

    let listed = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "ctx",
            "ls",
            "--json",
            "--ployz-config",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(
        listed,
        serde_json::json!({ "contexts": [{
            "name": "prod",
            "current": true,
            "connections": ["unix:///tmp/prod-a.sock", "unix:///tmp/prod-b.sock"],
        }] })
    );
    assert_eq!(Config::load(&path).unwrap(), before);
}

#[test]
fn ctx_use_connection_selects_and_persists_across_invocations() {
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
            "use",
            "--connection",
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
fn ctx_use_rejects_an_unknown_connection_without_mutating() {
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

    for (requested, expected) in [
        (
            "unix:///tmp/missing.sock",
            r#"connection "unix:///tmp/missing.sock" not found"#,
        ),
        (
            "unix:///tmp/missing socket\n\u{1b}[2J",
            r#"connection "unix:///tmp/missing socket\n\u{1b}[2J" not found"#,
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args([
                "ctx",
                "use",
                "--connection",
                requested,
                "--ployz-config",
                path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            format!("error: {expected}")
        );
        assert_eq!(Config::load(&path).unwrap(), before);
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ctx_use_help_describes_the_connection_choice() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["ctx", "use", "--help"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--connection"), "{help}");
    assert!(help.contains("[context-name]"), "{help}");
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
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["ctx", "use"])
        .args(["--ployz-config", flag_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("needs a terminal"), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "retry: ployz ctx use --ployz-config {} CONTEXT",
            shell_words::quote(flag_path.to_str().unwrap())
        )),
        "{stderr}"
    );
    assert_eq!(Config::load(&flag_path).unwrap(), before);

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

#[test]
fn ctx_rm_of_a_non_current_context_persists() {
    let root =
        std::env::temp_dir().join(format!("ployz-ctx-rm-non-current-{}", std::process::id()));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([
            (
                "default".into(),
                Context {
                    connections: vec![Connection::unix("/tmp/default.sock").unwrap()],
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

    let removed = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "ctx",
            "rm",
            "default",
            "--ployz-config",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert_eq!(
        String::from_utf8(removed.stdout).unwrap().trim(),
        "Removed context default."
    );

    let config = Config::load(&path).unwrap();
    assert_eq!(config.current_context(), Some("prod"));
    assert!(!config.contexts.contains_key("default"));
    assert!(config.contexts.contains_key("prod"));

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
    assert!(!listed.contains("default"), "{listed}");
    assert!(listed.contains("prod"), "{listed}");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ctx_rm_of_the_current_context_unsets_current() {
    let root = std::env::temp_dir().join(format!("ployz-ctx-rm-current-{}", std::process::id()));
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

    let removed = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "ctx",
            "rm",
            "prod",
            "--ployz-config",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let stdout = String::from_utf8(removed.stdout).unwrap();
    assert!(stdout.contains("Removed context prod."), "{stdout}");
    let stderr = String::from_utf8(removed.stderr).unwrap();
    assert!(stderr.contains("Current context is now unset."), "{stderr}");

    assert_eq!(current_context(&path), None);

    let config = Config::load(&path).unwrap();
    assert_eq!(config.current_context(), None);
    assert!(!config.contexts.contains_key("prod"));
    let yaml = fs::read_to_string(&path).unwrap();
    assert!(
        !yaml.contains("current_context:"),
        "dangling current context was stored: {yaml}"
    );

    let selected = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "ctx",
            "use",
            "dev",
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
    assert_eq!(Config::load(&path).unwrap().current_context(), Some("dev"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ctx_rm_of_the_last_context_leaves_an_empty_file() {
    let root = std::env::temp_dir().join(format!("ployz-ctx-rm-last-{}", std::process::id()));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    Config::new(
        &path,
        Some("default".into()),
        BTreeMap::from([(
            "default".into(),
            Context {
                connections: vec![Connection::unix("/tmp/default.sock").unwrap()],
            },
        )]),
    )
    .save()
    .unwrap();

    let removed = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "ctx",
            "rm",
            "default",
            "--ployz-config",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );

    assert!(path.exists());
    let config = Config::load(&path).unwrap();
    assert!(config.contexts.is_empty());
    assert_eq!(config.current_context(), None);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ctx_rm_of_an_unknown_name_fails_without_mutating() {
    let root = std::env::temp_dir().join(format!("ployz-ctx-rm-unknown-{}", std::process::id()));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    let before = Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([(
            "prod".into(),
            Context {
                connections: vec![Connection::unix("/tmp/prod.sock").unwrap()],
            },
        )]),
    );
    before.save().unwrap();

    for action in ["use", "rm"] {
        for name in ["gone", "gone\n\u{1b}[2J"] {
            let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
                .args([
                    "ctx",
                    action,
                    name,
                    "--ployz-config",
                    path.to_str().unwrap(),
                ])
                .output()
                .unwrap();
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(
                error.contains(&format!("context {} not found", name.escape_debug())),
                "{error}"
            );
            assert!(!error.trim().chars().any(char::is_control), "{error}");
            assert_eq!(Config::load(&path).unwrap(), before);
        }
    }

    fs::remove_dir_all(root).unwrap();
}

/// `ployz --ployz-config PATH`, in an empty home, with `env` the only Ployz environment:
/// a token in it signs in, against a Cloud that refuses every connection.
fn ployz_with(
    path: &std::path::Path,
    env: &[(&str, &str)],
) -> impl Fn(&[&str]) -> std::process::Output {
    let path = path.to_owned();
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    move |args| {
        let home = path.parent().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .args(["--ployz-config", path.to_str().unwrap()])
            .args(args)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home)
            .env_remove("PLOYZ_TOKEN")
            .env_remove("PLOYZ_CLOUD_URL")
            .env_remove("PLOYZ_CONTEXT")
            .env_remove("PLOYZ_CONNECT")
            .envs(env.iter().map(|(key, value)| (key, value)));
        command.output().unwrap()
    }
}

const SIGNED_IN: [(&str, &str); 2] = [
    ("PLOYZ_TOKEN", "token"),
    ("PLOYZ_CLOUD_URL", "http://127.0.0.1:9"),
];

#[test]
fn with_no_contexts_ls_and_use_point_at_login_or_cloud_servers() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
    // A config named on the command line carries into the next command.
    let config = format!("ployz --ployz-config {}", path.display());
    let with_context = [SIGNED_IN[0], SIGNED_IN[1], ("PLOYZ_CONTEXT", "dev")];
    for (env, next) in [
        (&[][..], Some(format!("{config} login"))),
        (&SIGNED_IN[..], Some(format!("{config} server ls"))),
        // `server ls` would find no contexts too, so nothing is offered.
        (&with_context[..], None),
        // Signing in keeps PLOYZ_CONTEXT, so `login` would lead back here too.
        (&[("PLOYZ_CONTEXT", "dev")][..], None),
        (&[("PLOYZ_CONTEXT", "")][..], None),
    ] {
        let ployz = ployz_with(&path, env);

        let listed = ployz(&["ctx", "ls"]);
        assert!(listed.status.success(), "{env:?}: {listed:?}");
        let stderr = String::from_utf8_lossy(&listed.stderr);
        assert!(stderr.contains("No contexts yet."), "{env:?}: {stderr}");
        let printed = stderr.lines().find_map(|line| line.strip_prefix("next: "));
        assert_eq!(printed, next.as_deref(), "{env:?}: {stderr}");

        let listed = ployz(&["ctx", "ls", "--json"]);
        assert!(listed.status.success(), "{env:?}: {listed:?}");
        let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        let expected = match &next {
            Some(next) => serde_json::json!({ "contexts": [], "next": next }),
            None => serde_json::json!({ "contexts": [] }),
        };
        assert_eq!(listed, expected, "{env:?}");

        let selected = ployz(&["ctx", "use", "dev"]);
        assert_eq!(selected.status.code(), Some(1), "{env:?}: {selected:?}");
        let stderr = String::from_utf8_lossy(&selected.stderr);
        assert!(stderr.contains("no contexts found"), "{env:?}: {stderr}");
        let printed = stderr.lines().find_map(|line| line.strip_prefix("next: "));
        assert_eq!(printed, next.as_deref(), "{env:?}: {stderr}");
        assert!(!path.exists(), "a failed ctx use writes no config");
    }
}

/// Every command that finds no contexts prints the line `ctx ls` prints, and that line never
/// leads back here: run in the same state, it gets past the empty config, or it is `ployz login`.
#[test]
fn every_command_that_finds_no_contexts_prints_a_next_line_that_gets_past_them() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
    Config::new(&path, None, BTreeMap::new()).save().unwrap();
    let login = format!("ployz --ployz-config {} login", path.display());
    let with_context = [SIGNED_IN[0], SIGNED_IN[1], ("PLOYZ_CONTEXT", "dev")];
    // An empty PLOYZ_CONTEXT still names a context.
    let with_empty_context = [SIGNED_IN[0], SIGNED_IN[1], ("PLOYZ_CONTEXT", "")];

    // Signed in, Cloud answers unless a context is named, so these name one.
    for (env, context) in [
        (&[][..], &[][..]),
        (&[("PLOYZ_CONTEXT", "dev")][..], &[][..]),
        (&[("PLOYZ_CONTEXT", "")][..], &[][..]),
        (&SIGNED_IN[..], &["--context", "dev"][..]),
        (&with_context[..], &[][..]),
        (&with_empty_context[..], &[][..]),
    ] {
        let ployz = ployz_with(&path, env);
        let listed = ployz(&["ctx", "ls"]);
        let listed = String::from_utf8_lossy(&listed.stderr);
        let next = listed
            .lines()
            .find(|line| line.starts_with("next: "))
            .map(str::to_owned);

        for (command, args) in [
            (&["ps"][..], &[][..]),
            (&["server", "ls"], &[]),
            (&["logs"], &["web"]),
            (&["service", "restart"], &["web"]),
            (&["exec"], &["web", "--", "true"]),
            (&["ctx", "use"], &["dev"]),
        ] {
            let context = if command == ["ctx", "use"] {
                &[]
            } else {
                context
            };
            let full = [command, context, args].concat();
            let failed = ployz(&full);
            assert_eq!(
                failed.status.code(),
                Some(1),
                "{env:?} {full:?}: {failed:?}"
            );
            let stderr = String::from_utf8_lossy(&failed.stderr);
            assert!(
                stderr.contains("no contexts found"),
                "{env:?} {full:?}: {stderr}"
            );
            let printed = stderr
                .lines()
                .find(|line| line.starts_with("next: "))
                .map(str::to_owned);
            assert_eq!(printed, next, "{env:?} {full:?}: {stderr}");
        }

        let failed = ployz(&[&["ps"][..], context, &["--json"]].concat());
        let error: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
        assert_eq!(
            error
                .pointer("/error/details/next")
                .and_then(serde_json::Value::as_str),
            next.as_deref().and_then(|next| next.strip_prefix("next: ")),
            "{env:?}: {error}"
        );

        let Some(next) = next else { continue };
        let next = next.strip_prefix("next: ").unwrap();
        if next == login {
            assert!(
                env.is_empty(),
                "only a signed-out user is sent to login: {env:?}"
            );
            continue;
        }
        let words = shell_words::split(next).unwrap();
        let Some(("ployz", args)) = words
            .split_first()
            .map(|(ployz, args)| (ployz.as_str(), args))
        else {
            panic!("{next}");
        };
        let ran = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args(args)
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env_remove("PLOYZ_CONTEXT")
            .env_remove("PLOYZ_CONNECT")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&ran.stderr);
        assert!(
            !stderr.contains("no contexts found"),
            "{env:?}: `{next}` leads back to the same error: {stderr}"
        );
    }
}

/// A Cloud that counts the requests it hears and answers each that approval is pending.
fn counting_cloud() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let heard = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = heard.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = stream.read(&mut [0; 4096]);
            let body = r#"{"error":"authorization_pending"}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, heard)
}

/// A device sign-in waiting for approval is not signed in, and choosing the next line for an
/// empty config reads it without asking Cloud or rewriting it.
#[test]
fn with_no_contexts_a_pending_sign_in_points_at_login_without_asking_cloud() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
    Config::new(&path, None, BTreeMap::new()).save().unwrap();
    let (cloud, heard) = counting_cloud();
    let stored = root.path().join("cloud.json");
    let pending = serde_json::json!({
        "state": "pending",
        "cloud": cloud,
        "device_code": "device-secret",
        "code": "ABCD2345",
        "url": format!("{cloud}/device?user_code=ABCD2345"),
        "expires_at": 4_102_444_800_u64,
        "interval_secs": 1,
    });
    fs::write(&stored, serde_json::to_vec_pretty(&pending).unwrap()).unwrap();
    let before = fs::read(&stored).unwrap();
    let login = format!("ployz --ployz-config {} login", path.display());
    let ployz = ployz_with(&path, &[]);

    let selected = ployz(&["ctx", "use", "dev"]);
    assert_eq!(selected.status.code(), Some(1), "{selected:?}");
    let stderr = String::from_utf8_lossy(&selected.stderr);
    assert!(stderr.contains("no contexts found"), "{stderr}");
    assert!(stderr.contains(&format!("next: {login}")), "{stderr}");

    let listed = ployz(&["ctx", "ls"]);
    assert!(listed.status.success(), "{listed:?}");
    let stderr = String::from_utf8_lossy(&listed.stderr);
    assert!(stderr.contains(&format!("next: {login}")), "{stderr}");

    assert_eq!(heard.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        fs::read(&stored).unwrap(),
        before,
        "cloud.json is unchanged"
    );
}

#[test]
fn ctx_rm_rejects_a_direct_connection() {
    let root = std::env::temp_dir().join(format!("ployz-ctx-rm-connect-{}", std::process::id()));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    let before = Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([(
            "prod".into(),
            Context {
                connections: vec![Connection::unix("/tmp/prod.sock").unwrap()],
            },
        )]),
    );
    before.save().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            "tcp://127.0.0.1:1",
            "ctx",
            "rm",
            "prod",
            "--ployz-config",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "error: context management is unavailable with a direct connection"
    );
    assert_eq!(Config::load(&path).unwrap(), before);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ctx_rm_help_describes_local_removal() {
    let rm = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["ctx", "rm", "--help"])
        .output()
        .unwrap();
    assert!(
        rm.status.success(),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
    let help = String::from_utf8(rm.stdout).unwrap();
    assert!(help.contains("local"), "{help}");
    assert!(help.contains("<context-name>"), "{help}");
}

#[test]
fn management_context_selection_and_listing_never_print_capabilities() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("config.yaml");
    let secret = ployz_core::ManagementCapability::new(
        ployz_core::ManagementIdentity::from_bytes([1; 32]),
        [2; 32],
    )
    .to_secret_string();
    Config::new(
        &path,
        Some("private".into()),
        BTreeMap::from([(
            "private".into(),
            Context {
                connections: vec![Connection::management(&secret).unwrap()],
            },
        )]),
    )
    .save()
    .unwrap();
    for args in [
        vec!["ctx", "ls"],
        vec!["ctx", "ls", "--json"],
        vec!["ctx", "use", "--connection", "management:[redacted]"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--ployz-config")
            .arg(&path)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&secret));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&secret));
        if args.len() > 2 {
            assert!(String::from_utf8_lossy(&output.stdout).contains("management:[redacted]"));
        }
    }
}

#[test]
fn management_selection_uses_machine_labels_or_ordered_indices_in_a_mixed_context() {
    use ployz::context::SshDestination;
    use ployz_core::MachineId;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("config.yaml");
    let ssh = Connection::ssh(SshDestination::parse("root@example.com").unwrap());
    let first_secret = ployz_core::ManagementCapability::new(
        ployz_core::ManagementIdentity::from_bytes([3; 32]),
        [4; 32],
    )
    .to_secret_string();
    let second_secret = ployz_core::ManagementCapability::new(
        ployz_core::ManagementIdentity::from_bytes([5; 32]),
        [6; 32],
    )
    .to_secret_string();
    let first = Connection::management(&first_secret)
        .unwrap()
        .with_machine_id(MachineId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap());
    let second = Connection::management(&second_secret)
        .unwrap()
        .with_machine_id(MachineId::parse("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap());
    Config::new(
        &path,
        Some("mixed".into()),
        BTreeMap::from([(
            "mixed".into(),
            Context {
                connections: vec![ssh.clone(), first.clone(), second.clone()],
            },
        )]),
    )
    .save()
    .unwrap();
    for (selector, expected) in [
        (
            "3".to_owned(),
            vec![second.clone(), ssh.clone(), first.clone()],
        ),
        (
            first.to_string(),
            vec![first.clone(), second.clone(), ssh.clone()],
        ),
        (
            ssh.to_string(),
            vec![ssh.clone(), first.clone(), second.clone()],
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--ployz-config")
            .arg(&path)
            .args(["ctx", "use", "--connection", &selector])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rendered = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!rendered.contains(&first_secret));
        assert!(!rendered.contains(&second_secret));
        assert_eq!(
            Config::load(&path)
                .unwrap()
                .contexts
                .get("mixed")
                .unwrap()
                .connections,
            expected
        );
    }
}

#[test]
fn ambiguous_management_labels_fail_without_mutation_and_index_selects_the_second() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("config.yaml");
    let first = Connection::management(
        ployz_core::ManagementCapability::new(
            ployz_core::ManagementIdentity::from_bytes([1; 32]),
            [2; 32],
        )
        .to_secret_string(),
    )
    .unwrap();
    let second = Connection::management(
        ployz_core::ManagementCapability::new(
            ployz_core::ManagementIdentity::from_bytes([3; 32]),
            [4; 32],
        )
        .to_secret_string(),
    )
    .unwrap();
    let config = Config::new(
        &path,
        Some("private".into()),
        BTreeMap::from([(
            "private".into(),
            Context {
                connections: vec![first.clone(), second.clone()],
            },
        )]),
    );
    config.save().unwrap();
    for (selector, message) in [
        ("management:[redacted]", "ambiguous"),
        ("0", "out of range"),
        ("3", "out of range"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--ployz-config")
            .arg(&path)
            .args(["ctx", "use", "--connection", selector])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        assert_eq!(Config::load(&path).unwrap(), config);
    }
    let selected = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--ployz-config")
        .arg(&path)
        .args(["ctx", "use", "--connection", "2"])
        .output()
        .unwrap();
    assert!(selected.status.success());
    assert_eq!(
        Config::load(&path)
            .unwrap()
            .contexts
            .get("private")
            .unwrap()
            .connections,
        vec![second, first]
    );
}

#[test]
fn context_config_selection_works_before_between_and_after_commands() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.yaml");
    Config::new(
        &path,
        Some("default".into()),
        BTreeMap::from([
            (
                "default".into(),
                Context {
                    connections: Vec::new(),
                },
            ),
            (
                "verify".into(),
                Context {
                    connections: Vec::new(),
                },
            ),
        ]),
    )
    .save()
    .unwrap();
    let run = ployz_with(&path, &[("PLOYZ_CONTEXT", "default")]);
    for args in [
        &["-c", "verify", "server", "ls"][..],
        &["--context=verify", "server", "ls"],
        &["server", "--context", "verify", "ls"],
        &["server", "ls", "--context", "verify"],
    ] {
        let output = run(&[args, &["--json"]].concat());
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}: {json}");
        assert_eq!(
            json.pointer("/error/message").unwrap().as_str().unwrap(),
            "Context verify has no Servers.",
            "{args:?}: {json}"
        );
        assert_eq!(
            json.pointer("/error/details/next")
                .unwrap()
                .as_str()
                .unwrap(),
            format!(
                "ployz --ployz-config {} server add --context verify",
                path.display()
            ),
            "{json}"
        );
    }
}

#[test]
fn context_config_corrupt_yaml_keeps_its_line_and_parser_cause() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.yaml");
    fs::write(&path, "current_context: [[[\nbad line\n").unwrap();
    let run = ployz_with(&path, &[]);
    for args in [&["ctx", "ls"][..], &["status"]] {
        let output = run(&[args, &["--json"]].concat());
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}: {json}");
        assert_eq!(
            json.pointer("/error/message").unwrap().as_str().unwrap(),
            format!("Could not parse Ployz config {} (line 1).", path.display()),
            "{json}"
        );
        let causes = json.pointer("/error/cause").unwrap().as_array().unwrap();
        assert!(!causes.is_empty(), "{json}");
        assert!(
            causes
                .last()
                .unwrap()
                .as_str()
                .unwrap()
                .contains("current_context"),
            "{json}"
        );
        let human = run(args);
        let stderr = String::from_utf8(human.stderr).unwrap();
        assert!(stderr.contains("(line 1)."), "{stderr}");
        assert!(stderr.contains("cause:"), "{stderr}");
        assert!(!stderr.contains("ployz login"), "{stderr}");
    }
}

#[test]
fn context_config_status_reports_read_errors_but_allows_a_missing_config() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.yaml");
    let run = ployz_with(&path, &[]);
    assert!(run(&["status", "--json"]).status.success());
    fs::create_dir(&path).unwrap();
    let output = run(&["status", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{json}");
    assert_eq!(
        json.pointer("/error/message").unwrap().as_str().unwrap(),
        format!("Could not read Ployz config {}.", path.display()),
        "{json}"
    );
}

#[test]
fn context_config_empty_context_names_the_next_step_for_runtime_commands() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.yaml");
    Config::new(
        &path,
        Some("verify".into()),
        BTreeMap::from([(
            "verify".into(),
            Context {
                connections: Vec::new(),
            },
        )]),
    )
    .save()
    .unwrap();
    let run = ployz_with(&path, &[]);
    for args in [&["server", "ls"][..], &["ps"]] {
        let output = run(args);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(
            stderr.contains("error: Context verify has no Servers."),
            "{stderr}"
        );
        assert!(
            stderr.contains("next: ployz") && stderr.contains("server add --context verify"),
            "{stderr}"
        );
        assert!(!stderr.contains("no connections found"), "{stderr}");
    }
}
