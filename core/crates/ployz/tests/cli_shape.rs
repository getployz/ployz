#[cfg(unix)]
use std::process::{Command as ProcessCommand, Stdio};

#[test]
fn no_command_declares_an_alias() {
    fn walk(command: &clap::Command, parent: &str) {
        for child in command.get_subcommands() {
            let path = format!("{parent}{}", child.get_name());
            assert_eq!(
                child.get_all_aliases().count(),
                0,
                "{path} declares an alias"
            );
            walk(child, &format!("{path} "));
        }
    }
    walk(&ployz::cli::command(), "");
}

#[test]
fn json_is_one_global_switch_and_no_command_keeps_an_output_format() {
    fn assert_no_output(command: &clap::Command, path: &str) {
        assert!(
            command.get_arguments().all(|arg| arg.get_id() != "output"),
            "{path} keeps an output format"
        );
        for child in command.get_subcommands() {
            assert_no_output(child, &format!("{path} {}", child.get_name()));
        }
    }
    let command = ployz::cli::command();
    let json = command
        .get_arguments()
        .find(|arg| arg.get_id() == "json")
        .expect("root --json");
    assert!(json.is_global_set());
    assert_eq!(json.get_short(), None);
    assert_no_output(&command, "ployz");

    let matches = command
        .try_get_matches_from(["ployz", "--json", "server", "ls"])
        .unwrap();
    assert!(matches.get_flag("json"));
}

#[test]
fn sessions_shell_code_and_build_refuse_json() {
    for args in [
        &["exec", "--json", "api"][..],
        &["completion", "--json", "bash"],
        &[
            "build",
            "--json",
            "--grant",
            "grant",
            "--deployment",
            "deployment.json",
            "--commit",
            "abc",
            "--fingerprint",
            "fp",
        ],
    ] {
        let (code, json, stderr) = run_json(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert_eq!(
            json.pointer("/error/code").unwrap(),
            "invalid_argument",
            "{json}"
        );
        assert!(
            message(&json).ends_with("does not support --json"),
            "{json}"
        );
    }
}

#[test]
fn each_short_flag_has_one_meaning_across_the_tree() {
    fn collect(
        command: &clap::Command,
        path: &str,
        seen: &mut std::collections::BTreeMap<char, (String, String)>,
    ) {
        for arg in command.get_arguments() {
            let Some(short) = arg.get_short() else {
                continue;
            };
            let id = arg.get_id().to_string();
            let previous = seen
                .entry(short)
                .or_insert_with(|| (id.clone(), path.to_owned()));
            assert_eq!(
                previous.0, id,
                "-{short} means --{} in `{}` but --{id} in `{path}`",
                previous.0, previous.1
            );
        }
        for child in command.get_subcommands() {
            collect(child, &format!("{path} {}", child.get_name()), seen);
        }
    }
    collect(&ployz::cli::command(), "ployz", &mut Default::default());
}

#[test]
fn completion_hooks_the_binary_for_every_supported_shell() {
    for shell in ["bash", "elvish", "fish", "powershell", "zsh"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--why=test")
            .args(["completion", shell])
            .output()
            .unwrap();
        assert!(output.status.success(), "{shell}");
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(output.contains("PLOYZ_COMPLETE"), "{shell} hook: {output}");
    }
}

#[test]
fn server_upgrade_requires_explicit_targets_in_order() {
    let command = ployz::cli::command();
    let request = command
        .clone()
        .try_get_matches_from([
            "ployz",
            "server",
            "upgrade",
            "1.2.3-beta.4",
            "edge-a",
            "0123456789abcdef0123456789abcdef",
        ])
        .unwrap();
    let upgrade = request
        .subcommand_matches("server")
        .unwrap()
        .subcommand_matches("upgrade")
        .unwrap();
    assert_eq!(
        upgrade
            .get_one::<ployz_core::MachineRelease>("version")
            .map(ToString::to_string)
            .as_deref(),
        Some("1.2.3-beta.4")
    );
    assert_eq!(
        upgrade
            .get_many::<String>("server")
            .unwrap()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["edge-a", "0123456789abcdef0123456789abcdef"]
    );
    assert!(
        command
            .try_get_matches_from(["ployz", "server", "upgrade", "stable"])
            .is_err()
    );
}

/// Run the binary against an empty config home; returns (exit code, stdout JSON, stderr).
fn run_json(args: &[&str]) -> (Option<i32>, serde_json::Value, String) {
    run_json_with::<&str>(args, &[])
}

/// [`run_json`] with extra environment variables.
fn run_json_with<V: AsRef<std::ffi::OsStr>>(
    args: &[&str],
    envs: &[(&str, V)],
) -> (Option<i32>, serde_json::Value, String) {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.yaml");
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .args(args)
        .args(["--ployz-config", config.to_str().unwrap()])
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("PLOYZ_CONTEXT")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_STORE")
        .envs(envs.iter().map(|(name, value)| (name, value)))
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON object ({error}): {stdout:?}"));
    (
        output.status.code(),
        json,
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn json_results_and_errors_are_one_stdout_object_with_distinct_exit_codes() {
    let (code, json, _) = run_json(&["ctx", "ls", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(json.get("contexts"), Some(&serde_json::json!([])), "{json}");
    assert!(
        json.get("next")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|next| next.ends_with(" login")),
        "{json}"
    );

    let (code, json, _) = run_json(&["ctx", "use", "missing", "--json"]);
    assert_eq!(code, Some(1));
    assert_eq!(json.pointer("/error/code").unwrap(), "not_found", "{json}");
    assert!(message(&json).contains("no contexts"), "{json}");
    assert_eq!(
        json.pointer("/error/cause").unwrap(),
        &serde_json::json!([]),
        "{json}"
    );

    let (code, json, stderr) = run_json(&["volume", "ls", "--json", "--no-such-flag"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "invalid_argument",
        "{json}"
    );
    assert!(message(&json).contains("--no-such-flag"), "{json}");
}

#[test]
fn docs_answers_offline_from_the_binary() {
    let (code, json, stderr) = run_json(&["docs", "add", "a", "custom", "domain", "--json"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(json.pointer("/query").unwrap(), "add a custom domain");
    assert_eq!(
        json.pointer("/results/0/path").unwrap(),
        "services/domains.md"
    );
    assert_eq!(
        json.pointer("/results/0/anchor").unwrap(),
        "add-a-custom-domain"
    );

    let (code, json, stderr) = run_json(&["docs", "domains.md", "--json"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(json.pointer("/path").unwrap(), "services/domains.md");
    assert!(
        json.pointer("/content")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("## Add a custom domain")
    );

    let (code, json, _) = run_json(&["docs", "services/domain.md", "--json"]);
    assert_eq!(code, Some(1));
    assert_eq!(json.pointer("/error/code").unwrap(), "not_found", "{json}");
    assert_eq!(
        json.pointer("/error/details/did_you_mean").unwrap(),
        "services/domains.md",
        "{json}"
    );
}

#[test]
fn an_unreadable_config_fails_the_same_way_for_every_command() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.yaml");
    std::fs::write(&config, "contexts: {}\n").unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        std::fs::read(&config).is_err(),
        "this user reads a mode 000 file; run the suite as a non-root user"
    );
    let run = |args: &[&str]| {
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--why=test")
            .args(args)
            .args(["--json", "--ployz-config", config.to_str().unwrap()])
            .env("HOME", home.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("PLOYZ_CONTEXT")
            .env_remove("PLOYZ_CONNECT")
            .env_remove("PLOYZ_TOKEN")
            .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
            .env_remove("PLOYZ_STORE")
            .output()
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        (output.status.code(), json.pointer("/error/code").cloned())
    };
    let ps = run(&["ps"]);
    assert_eq!(ps, (Some(1), Some(serde_json::json!("internal"))));
    assert_eq!(run(&["ctx", "use", "prod"]), ps);
    assert_eq!(run(&["server", "ls"]), ps);
}

#[test]
fn no_failure_message_flattens_a_cause() {
    const CONSTRUCTORS: [&str; 10] = [
        "::usage(",
        "::not_found(",
        "::ambiguous(",
        "::conflict(",
        "::unavailable(",
        "::coded(",
        "::detailed(",
        "::caused(",
        ".context(",
        "#[error(",
    ];
    const FLATTENERS: [&str; 12] = [
        "inline(",
        "error_chain::row(",
        "error.to_string()",
        "{error}",
        "{error:",
        "{err}",
        "{err:",
        "{e}",
        "{e:",
        "{source}",
        "{source:",
        "{cause}",
    ];
    let src = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut found = Vec::new();
    let mut dirs = vec![src.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for constructor in CONSTRUCTORS {
                for (start, _) in source.match_indices(constructor) {
                    if source[..start].ends_with("RpcError") {
                        continue;
                    }
                    let rest = &source[start + constructor.len()..];
                    let mut depth = 1;
                    let end = rest
                        .char_indices()
                        .find_map(|(at, c)| {
                            depth += match c {
                                '(' => 1,
                                ')' => -1,
                                _ => 0,
                            };
                            (depth == 0).then_some(at)
                        })
                        .unwrap_or(rest.len());
                    let argument = &rest[..end];
                    if FLATTENERS
                        .iter()
                        .any(|flattener| argument.contains(flattener))
                    {
                        found.push(format!("{}: {constructor}{argument}", path.display()));
                    }
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "use Failure::caused or .context:\n{}",
        found.join("\n")
    );
}

/// Run the binary for human output against an empty config home; returns (exit code, stderr).
fn run_human(args: &[&str]) -> (Option<i32>, String) {
    run_human_with(args, &[])
}

fn run_human_with(args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String) {
    let output = human_output(args, env);
    (
        output.status.code(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn human_output(args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.yaml");
    ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .args(args)
        .args(["--ployz-config", config.to_str().unwrap()])
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("PLOYZ_CONTEXT")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_STORE")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        &["explain", "web.restart_policy"][..],
        &["cloud", "reset"],
        &[
            "build",
            "--grant",
            "x",
            "--deployment",
            "1",
            "--commit",
            "abc",
            "--fingerprint",
            "f",
        ],
        &["completion", "--json", "bash"],
        &["exec", "--json", "api"],
        &["login", "--cloud-url", "ftp://x"],
        &["server", "add", "root@"],
    ] {
        let (code, stderr) = run_human(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        if !args.contains(&"--json") {
            assert!(stderr.starts_with("error: "), "{args:?}: {stderr}");
        }
    }
}

#[test]
fn a_ci_or_agent_key_without_an_environment_is_refused_before_anything_is_read() {
    for args in [
        &["key", "add", "--ci", "--name", "github-actions", "--json"][..],
        &["key", "add", "--agent", "--json"],
        &["key", "scope", "mmg42klgyq", "--json"],
    ] {
        let (code, json, stderr) = run_json(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert_eq!(
            json.pointer("/error/code").unwrap(),
            "invalid_argument",
            "{json}"
        );
        assert!(message(&json).contains("--env"), "{json}");
    }
}

#[test]
fn a_cluster_name_past_63_characters_or_not_one_label_is_refused_before_any_server_is_reached() {
    let long = "a".repeat(64);
    for args in [
        &["cluster", "name", long.as_str(), "--json"][..],
        &["cluster", "name", "Acme", "--json"],
        &[
            "server",
            "add",
            "--standalone",
            "--no-install",
            "--storage",
            "none",
            "root@192.0.2.1",
            "--cluster-name",
            "acme.prod",
            "--json",
        ],
    ] {
        let (code, json, stderr) = run_json(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert_eq!(
            json.pointer("/error/code").unwrap(),
            "invalid_argument",
            "{json}"
        );
        assert!(message(&json).contains("up to 63"), "{json}");
    }
}

#[test]
fn naming_the_cluster_signed_in_asks_cloud_and_with_a_context_skips_it() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cloud = fake_cloud(routes.clone(), FakeCloud::default());
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];

    run_json_with(&["cluster", "name", "beta", "--json"], &token);
    assert_eq!(
        routes.lock().unwrap().drain(..).collect::<Vec<_>>(),
        [r#"POST /api/cli/run/cluster_rename {"input":{"name":"beta"}}"#]
    );

    let (code, json, _) = run_json_with(
        &["cluster", "name", "beta", "--context", "staging", "--json"],
        &token,
    );
    assert_ne!(code, Some(0), "{json}");
    assert!(routes.lock().unwrap().is_empty(), "{json}");
}

#[test]
fn founding_a_standalone_cluster_with_json_needs_its_name() {
    let (code, json, stderr) = run_json(&[
        "server",
        "add",
        "--standalone",
        "--no-install",
        "--storage",
        "none",
        "root@192.0.2.1",
        "--json",
    ]);
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "invalid_argument",
        "{json}"
    );
    assert!(message(&json).contains("--cluster-name"), "{json}");
}

#[test]
fn a_cut_valid_list_still_names_the_closest_setting() {
    let (code, stderr) = run_human(&["explain", "web.restart_policy"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("\ndid you mean restartPolicy?\nvalid: "),
        "{stderr}"
    );
    assert_eq!(stderr.matches("valid:").count(), 1, "{stderr}");
}

#[test]
fn color_never_emits_no_escapes() {
    let (code, stderr) = run_human_with(
        &["ctx", "use", "missing", "--color", "never"],
        &[("CLICOLOR_FORCE", "1")],
    );
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(!stderr.contains('\x1b'), "{stderr:?}");
    let (_, forced) = run_human_with(&["ctx", "use", "missing"], &[("CLICOLOR_FORCE", "1")]);
    assert!(forced.contains('\x1b'), "{forced:?}");

    let (code, stderr) = run_human(&["ctx", "use", "missing", "--color", "always"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains('\x1b'), "{stderr:?}");

    let (_, forced) = run_human_with(&["ps", "--bogus"], &[("CLICOLOR_FORCE", "1")]);
    assert!(forced.contains('\x1b'), "{forced:?}");
    for args in [
        &["-c", "prod", "--color", "never", "deploy", "--bogus"][..],
        &["ps", "--color", "never", "--bogus"],
    ] {
        let (code, stderr) = run_human_with(args, &[("CLICOLOR_FORCE", "1")]);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("--bogus"), "{args:?}: {stderr}");
        assert!(!stderr.contains('\x1b'), "{args:?}: {stderr:?}");
    }
}

#[test]
fn asking_ps_for_a_container_column_says_where_the_ids_are() {
    for args in [
        &["ps", "--container"][..],
        &["ps", "--wide"],
        &["ps", "--sort", "service", "--wide"],
        &["ps", "-o", "wide"],
        &["ps", "--format={{.ID}}"],
        &["ps", "-q"],
    ] {
        let (code, stderr) = run_human(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert!(
            stderr.contains(
                "with server access, 'ployz ps --json' lists each container's container_id"
            ),
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("--connect"), "{args:?}: {stderr}");
    }

    let (code, json, stderr) = run_json(&["ps", "--json", "--container"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(json.pointer("/error/code").unwrap(), "invalid_argument");
    assert!(message(&json).contains("ployz ps --json"), "{json}");

    let (_, stderr) = run_human(&["ps", "--bogus"]);
    assert!(!stderr.contains("container_id"), "{stderr}");
}

#[test]
fn a_column_flag_given_as_the_sort_value_keeps_claps_error() {
    for (args, says) in [
        (
            &["ps", "--sort", "--wide"][..],
            "unexpected argument '--wide'",
        ),
        (&["ps", "--sort", "-w"], "unexpected argument '-w'"),
        (&["ps", "--sort=--wide"], "invalid value '<VALUE>'"),
    ] {
        let (code, stderr) = run_human(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(says), "{args:?}: {stderr}");
        assert!(!stderr.contains("container_id"), "{args:?}: {stderr}");
    }
}

#[test]
fn a_column_flag_after_the_option_terminator_keeps_claps_error() {
    for args in [
        &["ps", "--", "--wide"][..],
        &["ps", "--sort", "service", "--", "--wide"],
    ] {
        let (code, stderr) = run_human(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert!(
            stderr.contains("unexpected argument '--wide'"),
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("container_id"), "{args:?}: {stderr}");
    }

    let (code, stderr) = run_human(&["ps", "--wide", "--", "--wide"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("container_id"), "{stderr}");
}

#[test]
fn a_column_flag_on_another_command_keeps_its_error_when_a_global_took_ps_as_its_value() {
    let (code, stderr) = run_human(&["--connect", "ps", "server", "ls", "--wide"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("Usage: ployz server ls"), "{stderr}");
    assert!(!stderr.contains("container_id"), "{stderr}");
}

#[test]
fn color_always_keeps_a_piped_record_colored_and_auto_leaves_it_plain() {
    let stdout = |args: &[&str]| String::from_utf8(human_output(args, &[]).stdout).unwrap();
    assert_eq!(
        stdout(&["status", "--color", "always"]),
        "\x1b[2msigned in as\x1b[0m = no one\n"
    );
    assert_eq!(stdout(&["status"]), "signed in as = no one\n");
}

#[test]
fn json_without_a_command_is_the_version_or_an_error() {
    for flag in ["--version", "-V"] {
        let (code, json, _) = run_json(&["--json", flag]);
        assert_eq!(code, Some(0), "{flag}");
        assert_eq!(
            json,
            serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }),
            "{flag}"
        );
    }

    let (code, json, _) = run_json(&["--json"]);
    assert_eq!(code, Some(2));
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "invalid_argument",
        "{json}"
    );
    assert_eq!(message(&json), "a command is required");
}

#[test]
fn json_with_a_missing_subcommand_is_a_usage_error_not_help() {
    let (code, json, _) = run_json(&["server", "--json"]);
    assert_eq!(code, Some(2));
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "invalid_argument",
        "{json}"
    );
    assert_eq!(message(&json), "ployz server requires a subcommand");
}

#[test]
fn a_group_without_its_subcommand_prints_its_help_like_bare_ployz() {
    for group in ["org", "server", "cloud", "deployment"] {
        let home = tempfile::tempdir().unwrap();
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--why=test")
            .arg(group)
            .env("HOME", home.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("PLOYZ_CONTEXT")
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(0), "{group}");
        assert!(output.stderr.is_empty(), "{group}");
        assert!(
            stdout.contains(&format!("Usage: ployz {group} ")),
            "{group}: {stdout}"
        );
    }
}

#[test]
fn cloud_commands_act_with_ployz_token_or_the_signed_in_device() {
    for args in [
        &["token", "ls", "--json"][..],
        &["org", "ls", "--json"],
        &["github", "ls", "--json"],
        // Store and live commands fail the same way instead of naming a config file.
        &["get", "--json"],
        &["deploy", "--json"],
        &["server", "ls", "--json"],
        &["ps", "--json"],
        &["logs", "--json"],
        &["service", "restart", "web", "--json"],
    ] {
        let (code, json, _) = run_json(args);
        assert_eq!(code, Some(1), "{args:?}");
        assert_eq!(
            json.pointer("/error/code").unwrap(),
            "unauthenticated",
            "{json}"
        );
        assert_eq!(
            json.pointer("/error/details/next").unwrap(),
            "ployz login",
            "{json}"
        );
    }

    // A token needs no sign-in: it goes straight to its Cloud (here, nothing listens).
    let token = [
        ("PLOYZ_TOKEN", "ployz_secret"),
        ("PLOYZ_CLOUD_URL", "http://127.0.0.1:1"),
    ];
    let (code, json, _) = run_json_with(&["token", "ls", "--json"], &token);
    assert_eq!(code, Some(1));
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "unavailable",
        "{json}"
    );
    assert!(!json.to_string().contains("ployz_secret"), "{json}");

    let (code, json, _) = run_json_with(&["org", "use", "acme", "--json"], &token);
    assert_eq!(code, Some(2));
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "invalid_argument",
        "{json}"
    );

    let (code, json, _) = run_json(&["token", "new", "ci", "--expires-in", "0", "--json"]);
    assert_eq!(code, Some(2), "{json}");
}

#[test]
fn token_new_takes_the_three_presets_and_env_only_with_deploy() {
    for (args, wants) in [
        (
            &["--preset", "admin"][..],
            "invalid value '<VALUE>' for '--preset <preset>'",
        ),
        (&["--preset", "deploy"], "--env"),
        (
            &["--preset", "read", "--env", "staging"],
            "only --preset deploy takes --env",
        ),
    ] {
        let mut full = vec!["token", "new", "ci"];
        full.extend_from_slice(args);
        full.push("--json");
        let (code, json, _) = run_json(&full);
        assert_eq!(code, Some(2), "{args:?}: {json}");
        assert!(message(&json).contains(wants), "{args:?}: {json}");
    }
}

#[test]
fn server_clean_signed_in_refuses_another_clusters_servers() {
    let token = [
        ("PLOYZ_TOKEN", "ployz_secret"),
        ("PLOYZ_CLOUD_URL", "http://127.0.0.1:1"),
    ];
    for args in [
        &["server", "clean", "--context", "staging", "--json"][..],
        &[
            "server",
            "clean",
            "--namespace",
            "stray",
            "--context",
            "staging",
            "--json",
        ],
        &["server", "clean", "--connect", "10.0.0.1:4317", "--json"],
    ] {
        let (code, json, _) = run_json_with(args, &token);
        assert_eq!(code, Some(2), "{args:?}: {json}");
        assert_eq!(
            message(&json),
            "server clean reads your Organization's Servers; drop --context and --connect",
            "{args:?}"
        );
    }
}

/// What the fake Cloud answers: the Servers it can't reach, those that refused it, and the
/// connections it hands back.
#[derive(Default)]
struct FakeCloud {
    unreachable: Vec<String>,
    /// The Servers Server Access says refused Cloud, with what each said.
    refused: Vec<serde_json::Value>,
    /// The names Server Access gives the Servers, by id.
    names: serde_json::Value,
    /// The connections Server Access hands back, held before any Server was probed.
    connections: Vec<serde_json::Value>,
}

/// Serve `cloud` on a local port, recording each route with the body of a POST, and
/// answering anything else with 404.
fn fake_cloud(routes: std::sync::Arc<std::sync::Mutex<Vec<String>>>, cloud: FakeCloud) -> String {
    use std::io::{BufRead, Read, Write};
    let FakeCloud {
        unreachable,
        refused,
        names,
        connections,
    } = cloud;
    let names = if names.is_null() {
        serde_json::json!({})
    } else {
        names
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut sent = vec![0; length];
            reader.read_exact(&mut sent).unwrap();
            let route = request_line.rsplit_once(' ').unwrap().0.to_owned();
            let (status, body) = match route.as_str() {
                "POST /api/cli/server-access" => (
                    "200 OK",
                    serde_json::json!({
                        "connections": connections,
                        "unreachable": unreachable,
                        "refused": refused,
                        "names": names,
                    }),
                ),
                "POST /api/config/read" => (
                    "404 Not Found",
                    serde_json::json!({ "error": { "code": "not_found", "message": "This Organization has no Project yet" } }),
                ),
                _ => ("404 Not Found", serde_json::json!({ "error": "not found" })),
            };
            let recorded = match route.starts_with("POST") {
                true => format!("{route} {}", String::from_utf8_lossy(&sent)),
                false => route,
            };
            // Every run reports itself; the routes under test are the command's own.
            if !recorded.starts_with("POST /api/cli/agent/calls ") {
                routes.lock().unwrap().push(recorded);
            }
            let body = body.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    url
}

#[test]
fn server_ls_lists_an_organization_without_servers_as_empty() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cloud = fake_cloud(routes, FakeCloud::default());
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];

    let (code, json, stderr) = run_json_with(&["server", "ls", "--json"], &token);
    assert_eq!(code, Some(0), "{json} {stderr}");
    assert_eq!(
        json,
        serde_json::json!({ "servers": [], "failures": [], "omitted": [] })
    );

    let (code, stdout, stderr) = run_output(&["server", "ls"], &token);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        stdout.unwrap(),
        "SERVER\tREGION\tMEMBERSHIP\tSTORAGE\tPUBLIC IP\tSUBNET\tDAEMON\tOS\n"
    );
    assert!(stderr.contains("No Servers yet."), "{stderr}");
}

#[test]
fn ps_lists_an_organization_without_servers_as_empty() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cloud = fake_cloud(routes, FakeCloud::default());
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];

    let (code, json, stderr) = run_json_with(&["ps", "--json"], &token);
    assert_eq!(code, Some(0), "{json} {stderr}");
    assert_eq!(
        json,
        serde_json::json!({ "containers": [], "failures": [], "omitted": [] })
    );

    let (code, stdout, stderr) = run_output(&["ps"], &token);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stdout.unwrap(), "SERVICE\tKIND\tSERVER\tSTATE\n");
    assert!(stderr.contains("No Containers running yet."), "{stderr}");

    let (code, json, _) = run_json_with(&["metrics", "--server", "web-1", "--json"], &token);
    assert_eq!(code, Some(1), "{json}");
    assert_eq!(json.pointer("/error/code").unwrap(), "conflict", "{json}");
    assert_eq!(message(&json), "this Organization has no Servers");
}

/// Run the binary for human output with `env`; returns (exit code, stdout, stderr).
fn run_output(args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, Option<String>, String) {
    let home = tempfile::tempdir().unwrap();
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .args(args)
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("PLOYZ_CONTEXT")
        .env_remove("PLOYZ_CONNECT")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).ok(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn a_server_that_is_not_answering_yet_is_named_with_when_to_try_again() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let joining = "b".repeat(32);
    let cloud = fake_cloud(
        routes,
        FakeCloud {
            unreachable: vec![joining.clone()],
            names: serde_json::json!({ joining.clone(): "box-node-7" }),
            ..FakeCloud::default()
        },
    );
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];

    let (code, json, _) = run_json_with(&["server", "ls", "--json"], &token);
    assert_eq!(code, Some(1), "{json}");
    assert_eq!(
        json.pointer("/error/message").unwrap(),
        "Server box-node-7 isn't answering. If it just joined or restarted, try again in a moment.",
        "{json}"
    );
    assert_eq!(
        json.pointer("/error/details/unreachable").unwrap(),
        &serde_json::json!([joining]),
        "{json}"
    );
}

#[test]
fn a_server_cloud_handed_a_connection_for_that_does_not_answer_is_named() {
    let held = "0123456789abcdef0123456789abcdef";
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let human = |names: serde_json::Value| {
        let cloud = fake_cloud(
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            FakeCloud {
                connections: vec![serde_json::json!({ "tcp": closed, "machine_id": held })],
                names,
                ..FakeCloud::default()
            },
        );
        let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];
        let (code, _, stderr) = run_output(&["ps"], &token);
        assert_eq!(code, Some(1), "{stderr}");
        let (_, json, _) = run_json_with(&["ps", "--json"], &token);
        assert_eq!(
            json.pointer("/error/details/unreachable").unwrap(),
            &serde_json::json!([held]),
            "{json}"
        );
        stderr
    };

    let named = human(serde_json::json!({ held: "box-node-09c8" }));
    assert!(
        named.starts_with(
            "error: Server box-node-09c8 isn't answering. Check that its Ployz daemon is running, then retry.\n  cause: "
        ),
        "{named}"
    );
    // Cloud never learned its name: the Machine ID says which.
    let unnamed = human(serde_json::json!({}));
    assert!(
        unnamed.starts_with(&format!(
            "error: Server {held} isn't answering. Check that its Ployz daemon is running, then retry.\n  cause: "
        )),
        "{unnamed}"
    );
}

#[test]
fn removing_a_server_that_refused_cloud_says_why_and_not_to_forget_it() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let refusing = "b".repeat(32);
    let cloud = fake_cloud(
        routes,
        FakeCloud {
            refused: vec![serde_json::json!({
                "machine_id": refusing,
                "message": "Machine is not participating",
            })],
            names: serde_json::json!({ refusing.clone(): "web-1" }),
            ..FakeCloud::default()
        },
    );
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];

    let (code, json, _) = run_json_with(&["server", "rm", "web-1", "--json"], &token);
    assert_eq!(code, Some(1), "{json}");
    assert_eq!(
        json.pointer("/error/message").unwrap(),
        "Server web-1 couldn't give this command access: Machine is not participating",
        "{json}"
    );
    assert_eq!(
        json.pointer("/error/details/refused/0/machine_id").unwrap(),
        &serde_json::json!(refusing),
        "{json}"
    );
    assert_eq!(json.pointer("/error/details/next"), None, "{json}");
}

#[test]
fn removing_a_server_when_cloud_does_not_answer_says_to_retry_not_forget() {
    let token = [
        ("PLOYZ_TOKEN", "ployz_secret"),
        ("PLOYZ_CLOUD_URL", "http://127.0.0.1:9"),
    ];

    let (code, json, _) = run_json_with(&["server", "rm", "web-1", "--json"], &token);
    assert_eq!(code, Some(1), "{json}");
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "unavailable",
        "{json}"
    );
    assert_eq!(json.pointer("/error/details/next"), None, "{json}");
    let retry = json
        .pointer("/error/details/retry")
        .unwrap_or_else(|| panic!("{json}"))
        .as_str()
        .unwrap();
    // The retry keeps the reason this run gave, so an agent can run it as is.
    assert!(retry.starts_with("ployz --ployz-config "), "{json}");
    assert!(
        retry.contains(" --json --why '<REASON>' server rm "),
        "{json}"
    );
    assert!(retry.ends_with(" server rm web-1"), "{json}");
}

#[test]
fn server_inspect_is_gone_and_show_takes_its_place() {
    let (code, json, _) = run_json(&["server", "inspect", "web-1", "--json"]);
    assert_eq!(code, Some(2), "{json}");
    let command = ployz::cli::command();
    assert!(
        command
            .try_get_matches_from(["ployz", "server", "show", "web-1"])
            .is_ok()
    );
}

#[test]
fn root_help_ends_with_the_catalog_pointer() {
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(
        help.trim_end()
            .ends_with("`ployz explain SERVICE.SETTING` describes one Setting."),
        "{help}"
    );
}

#[test]
fn a_non_http_cloud_url_names_where_it_came_from() {
    let output = human_output(&["login"], &[("PLOYZ_CLOUD_URL", "ftp://x")]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("PLOYZ_CLOUD_URL takes a host"), "{stderr}");
    let output = human_output(&["login", "--cloud-url", "ftp://x"], &[]);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--cloud-url takes a host"), "{stderr}");
}

#[test]
fn explaining_a_setting_name_alone_points_at_the_setting() {
    let output = human_output(&["explain", "mounts"], &[]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("mounts.replicas"), "{stdout}");
    let output = human_output(&["explain", "web"], &[]);
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
    for setting in ["mounts", "env", "image", "domains"] {
        let output = human_output(&["explain", setting], &[]);
        assert_eq!(output.status.code(), Some(0), "{setting}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        let suggested = stderr
            .strip_prefix(&format!("{setting} is also a Setting; `"))
            .and_then(|rest| rest.strip_suffix("` describes it.\n"))
            .unwrap_or_else(|| panic!("{stderr}"));
        let words = shell_words::split(suggested).unwrap();
        let (ployz, args) = words.split_first().unwrap();
        assert_eq!(ployz, "ployz", "{suggested}");
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = human_output(&args, &[]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{suggested}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn a_store_url_that_is_not_unicode_is_a_usage_error() {
    use std::os::unix::ffi::OsStrExt;

    let store = std::ffi::OsStr::from_bytes(b"sqlite:/tmp/\xff.db");
    let (code, json, stderr) =
        run_json_with(&["project", "ls", "--json"], &[("PLOYZ_STORE", store)]);
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(
        json.pointer("/error/code"),
        Some(&serde_json::json!("invalid_argument")),
        "{json}"
    );
    assert_eq!(
        json.pointer("/error/message"),
        Some(&serde_json::json!("PLOYZ_STORE must be valid Unicode")),
        "{json}"
    );
}

#[test]
fn every_error_field_the_agent_skill_names_appears_in_real_output() {
    let skill = include_str!("../../../site/skill.md");
    let line = skill
        .lines()
        .find(|line| line.contains(r#"{"error": {"#))
        .unwrap();
    let shape = &line[line.find(r#"{"error": {"#).unwrap() + 11..];
    let mut named: Vec<String> = shape[..shape.find('}').unwrap()]
        .split(", ")
        .map(|field| format!("/error/{field}"))
        .collect();
    named.extend(skill.split('`').filter_map(|code| {
        code.strip_prefix("details.")
            .map(|key| format!("/error/details/{key}"))
    }));
    assert!(named.len() > 4, "{named:?}");

    let store = tempfile::tempdir().unwrap();
    let store = format!("sqlite:{}", store.path().join("store.db").display());
    let with_store = [("PLOYZ_STORE", store.as_str())];
    let (code, _, stderr) = run_json_with(&["project", "new", "blog", "--json"], &with_store);
    assert_eq!(code, Some(0), "{stderr}");
    let outputs = [
        run_json(&["token", "ls", "--json"]).1,
        run_json(&["explain", "web.restart_policy", "--json"]).1,
        run_json_with(&["project", "rm", "blog", "--json"], &with_store).1,
    ];
    for field in named {
        assert!(
            outputs.iter().any(|json| json.pointer(&field).is_some()),
            "{field} named by the skill appears in no output: {outputs:?}"
        );
    }
}

fn message(json: &serde_json::Value) -> &str {
    json.pointer("/error/message")
        .and_then(serde_json::Value::as_str)
        .unwrap()
}

#[cfg(unix)]
#[test]
fn a_daemon_that_hangs_up_is_one_json_error_on_piped_stdout() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            drop(connection);
        }
    });
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.yaml");
    let connect = format!("tcp://127.0.0.1:{port}");
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .args(["--json", "server", "ls", "--connect", &connect])
        .args(["--ployz-config", config.to_str().unwrap()])
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("PLOYZ_CONTEXT")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_STORE")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let values = serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<serde_json::Value>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let [value] = values.as_slice() else {
        panic!("stdout is not one JSON value: {values:?}");
    };
    assert_eq!(
        value.pointer("/error/code"),
        Some(&serde_json::json!("internal")),
        "{value}"
    );
}

#[cfg(unix)]
#[test]
fn piped_output_exits_on_sigpipe_when_the_reader_is_gone() {
    use std::os::unix::process::ExitStatusExt;

    for args in [&["schema"][..], &["--version"], &["--help"]] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--why=test")
            .args(args)
            .stdout(Stdio::from(writer))
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(output.status.signal(), Some(13), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?} {output:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_full_stdout_fails_the_command_in_both_modes() {
    // A result, clap's help and version, and the help a bare command or group prints.
    for args in [
        &["--version"][..],
        &["--json", "--version"],
        &["-V"],
        &["--help"],
        &["--json", "--help"],
        &["-h"],
        &["help", "server"],
        &["server", "ls", "--help"],
        &["--json", "server", "ls", "--help"],
        &[],
        &["server"],
    ] {
        let full = std::fs::File::options()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--why=test")
            .args(args)
            .stdout(Stdio::from(full))
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?} {output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(
            stderr,
            "error: Could not write the result to stdout.\n  cause: No space left on device (os error 28)\n",
            "{args:?}"
        );
    }
}

#[test]
fn compose_workflows_and_inputs_are_not_accepted() {
    for args in [
        &["ployz", "deploy", "--file", "compose.yaml"][..],
        &["ployz", "changes"],
        &["ployz", "build"],
        &["ployz", "run", "nginx"],
        &["ployz", "service", "run", "nginx"],
        &["ployz", "logs", "--file", "compose.yaml", "api"],
        &["ployz", "service", "scale", "-p", "shop", "api", "2"],
    ] {
        assert!(
            ployz::cli::command().try_get_matches_from(args).is_err(),
            "{args:?}"
        );
    }
}

#[test]
fn machine_policy_flags_are_independent_boolean_values_and_legacy_ingress_is_rejected() {
    for path in [
        vec!["server", "set", "node"],
        vec!["server", "add", "--standalone"],
        vec!["server", "add", "root@node"],
        vec!["server", "add", "--token-stdin"],
    ] {
        let mut args = vec!["ployz"];
        args.extend(path);
        let mut valid = args.clone();
        valid.extend([
            "--accepts-builds=true",
            "--accepts-services=false",
            "--accepts-ingress=true",
            "--label-add",
            "region=west",
            "--label-add",
            "disk=ssd",
        ]);
        let matches = ployz::cli::command().try_get_matches_from(valid).unwrap();
        let mut leaf = &matches;
        while let Some((_, child)) = leaf.subcommand() {
            leaf = child;
        }
        assert_eq!(leaf.get_one::<bool>("accepts-builds"), Some(&true));
        assert_eq!(leaf.get_one::<bool>("accepts-services"), Some(&false));
        assert_eq!(leaf.get_one::<bool>("accepts-ingress"), Some(&true));
        assert_eq!(leaf.get_many::<String>("label-add").unwrap().count(), 2);
        let mut removal = args.clone();
        removal.extend(["--label-rm", "retired"]);
        assert_eq!(
            ployz::cli::command().try_get_matches_from(removal).is_ok(),
            args.get(2) == Some(&"set")
        );
        for invalid in [
            "--no-ingress",
            "--accepts-services",
            "--accepts-builds=maybe",
        ] {
            let mut invalid_args = args.clone();
            invalid_args.push(invalid);
            assert!(
                ployz::cli::command()
                    .try_get_matches_from(invalid_args)
                    .is_err()
            );
        }
    }
}

#[test]
fn a_patch_excludes_a_secret_or_an_env_file() {
    let parse = |args: &[&str]| {
        ployz::cli::command()
            .try_get_matches_from([&["ployz", "set", "web"][..], args].concat())
            .is_ok()
    };
    assert!(!parse(&["--patch", "{}", "--from-env-file", ".env"]));
    assert!(!parse(&["--patch", "{}", "--secret"]));
    assert!(parse(&["--from-env-file", ".env", "--secret"]));
}

#[test]
fn up_resets_only_a_server_it_adds() {
    let parse = |args: &[&str]| {
        ployz::cli::command()
            .try_get_matches_from([&["ployz", "up"][..], args].concat())
            .is_ok()
    };
    assert!(parse(&["--server", "root@203.0.113.1", "--reset"]));
    assert!(!parse(&["--reset"]));
}

#[test]
fn up_keeps_other_services_with_no_prune() {
    let matches = ployz::cli::command()
        .try_get_matches_from(["ployz", "up", "--no-prune"])
        .unwrap();
    let (_, up) = matches.subcommand().unwrap();
    assert!(up.get_flag("no-prune"));
}

/// A thiserror message that interpolates its own source prints that cause
/// twice: once in the message, once as the next link in the chain.
#[test]
fn no_error_derive_interpolates_its_source() {
    use syn::visit::Visit;

    struct Derives<'a> {
        file: &'a std::path::Path,
        found: Vec<String>,
    }

    impl Derives<'_> {
        fn check(&mut self, item: &str, attrs: &[syn::Attribute], fields: &syn::Fields) {
            let marked = |field: &syn::Field| {
                field
                    .attrs
                    .iter()
                    .any(|attr| attr.path().is_ident("source") || attr.path().is_ident("from"))
            };
            let explicit = fields.iter().any(marked);
            let sources: Vec<String> = fields
                .iter()
                .enumerate()
                .filter(|(_, field)| {
                    marked(field)
                        || (!explicit && field.ident.as_ref().is_some_and(|name| name == "source"))
                })
                .map(|(at, field)| {
                    field
                        .ident
                        .as_ref()
                        .map_or_else(|| at.to_string(), ToString::to_string)
                })
                .collect();
            if sources.is_empty() {
                return;
            }
            for attr in attrs.iter().filter(|attr| attr.path().is_ident("error")) {
                let Ok(list) = attr.meta.require_list() else {
                    continue;
                };
                let Some(Ok(message)) = list
                    .tokens
                    .clone()
                    .into_iter()
                    .next()
                    .map(|first| syn::parse2::<syn::LitStr>(first.into()))
                else {
                    continue;
                };
                let text = list.tokens.to_string();
                let arguments = &text[message.token().to_string().len().min(text.len())..];
                let mut used = interpolated(&message.value());
                used.extend(arguments.split('.').skip(1).map(|after| {
                    after
                        .trim_start()
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect::<String>()
                }));
                // A message that is only its source repeats it exactly, and the chain drops
                // a line equal to the one above it.
                if arguments.trim().is_empty()
                    && sources
                        .iter()
                        .any(|source| message.value() == format!("{{{source}}}"))
                {
                    continue;
                }
                if let Some(source) = sources.iter().find(|source| used.contains(source)) {
                    self.found.push(format!(
                        "{}: {item} shows its source `{source}` in {:?}",
                        self.file.display(),
                        message.value()
                    ));
                }
            }
        }
    }

    impl<'ast> Visit<'ast> for Derives<'_> {
        fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
            self.check(&item.ident.to_string(), &item.attrs, &item.fields);
        }

        fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
            for variant in &item.variants {
                let name = format!("{}::{}", item.ident, variant.ident);
                self.check(&name, &variant.attrs, &variant.fields);
            }
        }
    }

    fn interpolated(message: &str) -> Vec<String> {
        let mut names = Vec::new();
        let mut rest = message.replace("{{", "");
        while let Some(open) = rest.find('{') {
            let tail = &rest[open + 1..];
            let close = tail.find('}').unwrap_or(tail.len());
            let name = tail[..close].split(':').next().unwrap_or("").trim();
            names.push(name.to_owned());
            rest = tail[close..].to_owned();
        }
        names
    }

    let crates = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
    let mut dirs: Vec<_> = std::fs::read_dir(crates)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .map(|krate| krate.join("src"))
        .filter(|src| src.is_dir())
        .collect();
    let mut found = Vec::new();
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let file = syn::parse_file(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let mut derives = Derives {
                file: &path,
                found: Vec::new(),
            };
            derives.visit_file(&file);
            found.append(&mut derives.found);
        }
    }
    found.sort();
    assert!(
        found.is_empty(),
        "the chain already prints the source; drop it from the message:\n{}",
        found.join("\n")
    );
}

/// Every raw-text field a CLI error message repeats, as `Type::Variant.field`, with where
/// its value comes from. None comes from a command line or a variable: a value typed there
/// reaches a message only once it parsed into its type, or as a `Shown`, since a value
/// refused as one kind may be a credential typed in the wrong place.
const RAW_MESSAGE_FIELDS: &[(&str, &str)] = &[
    (
        "BuildCommandError::Docker.command",
        "text the build runner composes from the Build Grant Cloud signed",
    ),
    (
        "BuildCommandError::Docker.stderr",
        "output of docker or git, run here on what the command builds",
    ),
    (
        "BuildCommandError::Git.command",
        "text the build runner composes from the Build Grant Cloud signed",
    ),
    (
        "BuildCommandError::Git.stderr",
        "output of docker or git, run here on what the command builds",
    ),
    (
        "BuildCommandError::NotAtCommit.0",
        "text the build runner composes from the Build Grant Cloud signed",
    ),
    (
        "BuildCommandError::Sealed.0",
        "text the build runner composes from the Build Grant Cloud signed",
    ),
    (
        "CapabilityCheckError::RefusedByEnforcement.0",
        "text a Server's daemon sent",
    ),
    (
        "ConfigError::CreateDirectory.path",
        "the Ployz config path: PLOYZ_CONFIG refuses a URL",
    ),
    (
        "ConfigError::EmptyCurrentContext.0",
        "the Ployz config path: PLOYZ_CONFIG refuses a URL",
    ),
    ("ConfigError::Parse.line", "text Ployz composes"),
    (
        "ConfigError::Parse.path",
        "the Ployz config path: PLOYZ_CONFIG refuses a URL",
    ),
    (
        "ConfigError::PrivatePermissions.0",
        "the Ployz config path: PLOYZ_CONFIG refuses a URL",
    ),
    (
        "ConfigError::Read.path",
        "the Ployz config path: PLOYZ_CONFIG refuses a URL",
    ),
    (
        "ConfigError::Write.path",
        "the Ployz config path: PLOYZ_CONFIG refuses a URL",
    ),
    (
        "ConnectError::AllFailed.server",
        "a Server's name from Cloud, a context or the Server itself",
    ),
    ("ConnectError::Attempt.0", "text Ployz composes"),
    (
        "ConnectError::Path.path",
        "the local daemon socket's path, compiled in",
    ),
    (
        "ConnectError::UnsupportedNetwork.0",
        "a network an SDK program asked to dial, not a command line",
    ),
    (
        "ContextError::ContextNotFound.path",
        "the Ployz config path: PLOYZ_CONFIG refuses a URL",
    ),
    (
        "ContextError::NoContexts.0",
        "the Ployz config path: PLOYZ_CONFIG refuses a URL",
    ),
    (
        "ContextError::NoCurrentContext.0",
        "the Ployz config path: PLOYZ_CONFIG refuses a URL",
    ),
    ("DeliveryError.0", "text Ployz composes"),
    ("Stop::Unobservable.detail", "text a Server's daemon sent"),
    ("Error::CodedConflict.body", "text Cloud sent"),
    ("Error::Exhausted.operation", "text Ployz composes"),
    (
        "Error::Invalid.0",
        "text the build runner composes from the Build Grant Cloud signed",
    ),
    (
        "Error::Io.0",
        "text the build runner composes from the Build Grant Cloud signed",
    ),
    (
        "Error::Sealed.0",
        "text the build runner composes from the Build Grant Cloud signed",
    ),
    ("Error::Status.body", "text Cloud sent"),
    (
        "KeyError::Exposed.path",
        "a file Ployz keeps beside its config",
    ),
    (
        "KeyError::Malformed.path",
        "a file Ployz keeps beside its config",
    ),
    (
        "KeyError::Read.path",
        "a file Ployz keeps beside its config",
    ),
    (
        "KeyError::Write.path",
        "a file Ployz keeps beside its config",
    ),
    ("LogError::Message.0", "text a Server's daemon sent"),
    ("LoginError::AmbiguousCredential.ids", "text Cloud sent"),
    (
        "LoginError::AmbiguousCredential.name",
        "a token name typed that matched more than one Cloud lists",
    ),
    ("LoginError::AwaitingApproval.url", "text Cloud sent"),
    (
        "LoginError::Corrupt.path",
        "a file Ployz keeps beside its config",
    ),
    (
        "LoginError::NotMember.0",
        "the stored sign-in's Organization, from Cloud",
    ),
    ("LoginError::Reply.0", "text Cloud sent"),
    ("LoginError::Status.body", "text Cloud sent"),
    (
        "LoginError::Store.path",
        "a file Ployz keeps beside its config",
    ),
    (
        "LoginError::TokenCloudUnreadable.0",
        "a file Ployz keeps beside its config",
    ),
    (
        "OperatorError::ExecConnectionLost.server",
        "a Service, Container or Server name typed, parsed as its selector, or one a Server reported",
    ),
    (
        "OperatorError::ExecContainerStopped.service",
        "a Service, Container or Server name typed, parsed as its selector, or one a Server reported",
    ),
    (
        "OperatorError::NotRunning.name",
        "a Service, Container or Server name typed, parsed as its selector, or one a Server reported",
    ),
    (
        "OperatorError::NotRunningOnAnswered.service",
        "a Service, Container or Server name typed, parsed as its selector, or one a Server reported",
    ),
    (
        "OperatorError::OpenContainerLogs.container",
        "a Service, Container or Server name typed, parsed as its selector, or one a Server reported",
    ),
    (
        "OperatorError::OpenContainerLogs.server",
        "a Service, Container or Server name typed, parsed as its selector, or one a Server reported",
    ),
    (
        "OperatorError::StartExec.container",
        "a Service, Container or Server name typed, parsed as its selector, or one a Server reported",
    ),
    (
        "OperatorError::UnsupportedLogService.service",
        "a Service, Container or Server name typed, parsed as its selector, or one a Server reported",
    ),
    (
        "PinError::Malformed.path",
        "a file Ployz keeps beside its config",
    ),
    (
        "PinError::NeedsCredentials.reference",
        "an image reference from the authored config or a build, parsed as one",
    ),
    (
        "PinError::NeedsCredentials.registry",
        "an image reference from the authored config or a build, parsed as one",
    ),
    (
        "PinError::NotATag.reference",
        "an image reference from the authored config or a build, parsed as one",
    ),
    (
        "PinError::Read.path",
        "a file Ployz keeps beside its config",
    ),
    (
        "PinError::TimedOut.registry",
        "an image reference from the authored config or a build, parsed as one",
    ),
    (
        "PinError::Unread.reference",
        "an image reference from the authored config or a build, parsed as one",
    ),
    (
        "PinError::Unread.registry",
        "an image reference from the authored config or a build, parsed as one",
    ),
    (
        "PinError::Write.path",
        "a file Ployz keeps beside its config",
    ),
    (
        "PlanError::CopiesUnknown.name",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::DependencyCycle.service",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::FirstCopyUnfit.machine",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::FirstCopyUnfit.volume",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::MixedVolumeModes.global",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::MixedVolumeModes.replicated",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::NoWriter.name",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::Service.service",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::VolumeHandoverCycle.because",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::VolumeHandoverCycle.volume",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::VolumeHolderUnknown.name",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::VolumeOutsideRegion.volume",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::VolumeServerTakesNoServices.volume",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    (
        "PlanError::VolumeSharedAcrossRegions.volume",
        "a name from the Store or the authored config, parsed as its kind",
    ),
    ("PreparationError::Delivery.0", "text Ployz composes"),
    (
        "ProvisionError::BootstrapVerification.0",
        "output of a command run on the Server over SSH",
    ),
    (
        "ProvisionError::InstallRefused.reason",
        "output of a command run on the Server over SSH",
    ),
    (
        "ProvisionError::PlatformFailed.0",
        "output of a command run on the Server over SSH",
    ),
    (
        "ProvisionError::SudoRequired.user",
        "output of a command run on the Server over SSH",
    ),
    (
        "ProvisionError::UnsupportedArchitecture.0",
        "output of a command run on the Server over SSH",
    ),
    (
        "ProvisionError::WhoamiFailed.0",
        "output of a command run on the Server over SSH",
    ),
    (
        "PublishError::AgentRefused.path",
        "a command path Cloud published",
    ),
    (
        "PublishError::Cache.path",
        "a file Ployz keeps beside its config",
    ),
    ("PublishError::Gone.path", "a command path Cloud published"),
    ("PublishError::Malformed.reason", "text Cloud sent"),
    (
        "PublishError::NeedsTerminal.path",
        "a command path Cloud published",
    ),
    ("PublishError::Unavailable.0", "text Cloud sent"),
    (
        "PushError::BuildIncomplete.image",
        "an image reference from the authored config or a build, parsed as one",
    ),
    (
        "PushError::BuildIncomplete.missing",
        "an image reference from the authored config or a build, parsed as one",
    ),
    ("PushError::InvalidReference.message", "text Ployz composes"),
    (
        "PushError::InvalidReference.reference",
        "an image reference from the authored config or a build, parsed as one",
    ),
    (
        "PushError::VariantUnavailable.image",
        "an image reference from the authored config or a build, parsed as one",
    ),
    (
        "PushError::VariantUnavailable.platform",
        "an image reference from the authored config or a build, parsed as one",
    ),
    (
        "RemoteBuildFailure::Failed.message",
        "text a Server's daemon sent",
    ),
    (
        "RemoteBuildFailure::Unknown.message",
        "text a Server's daemon sent",
    ),
    ("TransportError.message", "text a Server's daemon sent"),
];

/// A new raw-text field in an error message fails here until it is named above with its
/// source, so a raw value from a command line never reaches one unasked.
#[test]
fn no_error_message_repeats_raw_text_from_the_command_line() {
    use syn::visit::Visit;

    struct Messages {
        found: Vec<String>,
    }

    /// Text that came from somewhere at run time: a `&'static str` was compiled in.
    fn raw(ty: &syn::Type) -> bool {
        if let syn::Type::Reference(reference) = ty {
            return reference
                .lifetime
                .as_ref()
                .is_none_or(|lifetime| lifetime.ident != "static")
                && raw(&reference.elem);
        }
        let syn::Type::Path(path) = ty else {
            return false;
        };
        path.path.segments.last().is_some_and(|last| {
            let wrapped = matches!(
                &last.arguments,
                syn::PathArguments::AngleBracketed(arguments)
                    if arguments.args.iter().any(|argument| {
                        matches!(argument, syn::GenericArgument::Type(inner) if raw(inner))
                    })
            );
            ["String", "str", "PathBuf", "OsString", "Cow"]
                .iter()
                .any(|name| last.ident == name)
                || (["Option", "Box", "Vec"]
                    .iter()
                    .any(|name| last.ident == name)
                    && wrapped)
        })
    }

    /// Each name and position the message and its arguments read, `{name}`, `{0}`, `.name`.
    fn read(tokens: proc_macro2::TokenStream, names: &mut Vec<String>) {
        let mut dotted = false;
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Literal(literal) => {
                    if let Ok(message) = syn::parse2::<syn::LitStr>(
                        proc_macro2::TokenTree::Literal(literal.clone()).into(),
                    ) {
                        let mut rest = message.value().replace("{{", "");
                        while let Some(open) = rest.find('{') {
                            let tail = rest.get(open + 1..).unwrap_or_default().to_owned();
                            let close = tail.find('}').unwrap_or(tail.len());
                            let name = tail.get(..close).unwrap_or_default();
                            names.push(name.split(':').next().unwrap_or("").trim().to_owned());
                            rest = tail.get(close..).unwrap_or_default().to_owned();
                        }
                    } else if dotted {
                        names.push(literal.to_string());
                    }
                    dotted = false;
                }
                proc_macro2::TokenTree::Ident(ident) => {
                    names.push(ident.to_string());
                    dotted = false;
                }
                proc_macro2::TokenTree::Punct(punct) => dotted = punct.as_char() == '.',
                proc_macro2::TokenTree::Group(group) => {
                    read(group.stream(), names);
                    dotted = false;
                }
            }
        }
    }

    impl Messages {
        fn check(&mut self, item: &str, attrs: &[syn::Attribute], fields: &syn::Fields) {
            for attr in attrs.iter().filter(|attr| attr.path().is_ident("error")) {
                let Ok(list) = attr.meta.require_list() else {
                    continue;
                };
                let mut names = Vec::new();
                read(list.tokens.clone(), &mut names);
                for (at, field) in fields.iter().enumerate() {
                    let name = field
                        .ident
                        .as_ref()
                        .map_or_else(|| at.to_string(), ToString::to_string);
                    if raw(&field.ty) && names.contains(&name) {
                        self.found.push(format!("{item}.{name}"));
                    }
                }
            }
        }
    }

    impl<'ast> Visit<'ast> for Messages {
        fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
            self.check(&item.ident.to_string(), &item.attrs, &item.fields);
        }

        fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
            for variant in &item.variants {
                let name = format!("{}::{}", item.ident, variant.ident);
                self.check(&name, &variant.attrs, &variant.fields);
            }
        }
    }

    let mut dirs =
        vec![std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).to_owned()];
    let mut messages = Messages { found: Vec::new() };
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy();
            if !name.ends_with(".rs") || name.ends_with("tests.rs") || name == "testing.rs" {
                continue;
            }
            let file = syn::parse_file(&std::fs::read_to_string(&path).unwrap()).unwrap();
            messages.visit_file(&file);
        }
    }
    let named: Vec<&str> = RAW_MESSAGE_FIELDS.iter().map(|(field, _)| *field).collect();
    let mut unnamed: Vec<&String> = messages
        .found
        .iter()
        .filter(|field| !named.contains(&field.as_str()))
        .collect();
    unnamed.sort();
    unnamed.dedup();
    let stale: Vec<&&str> = named
        .iter()
        .filter(|field| !messages.found.iter().any(|found| found == **field))
        .collect();
    assert!(
        unnamed.is_empty() && stale.is_empty(),
        "parse a value typed into its type or a Shown before a message repeats it, or name where \\
         the field's text comes from in RAW_MESSAGE_FIELDS.\nunnamed:\n{}\nno longer found: {stale:?}",
        unnamed
            .iter()
            .map(|field| format!("    (\"{field}\", \"\"),"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn results_on_stdout_side_channel_on_stderr() {
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--why=test")
            .args(args)
            .env("HOME", home.path())
            .env_remove("XDG_CONFIG_HOME")
            .env("PLOYZ_CONFIG", home.path().join("config.yaml"))
            .env(
                "PLOYZ_STORE",
                format!("sqlite:{}", home.path().join("store.db").display()),
            )
            .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
            .env_remove("PLOYZ_TOKEN")
            .env_remove("PLOYZ_PROJECT")
            .env_remove("PLOYZ_ENV")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        (
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    };
    run(&["project", "new", "shop"]);

    let (stdout, stderr) = run(&["service", "add", "web", "--image", "nginx:1"]);
    assert!(stdout.starts_with("Staged new Service web"), "{stdout}");
    assert_eq!(stderr, "next: ployz deploy\n");

    let (stdout, stderr) = run(&["service", "ls"]);
    assert_eq!(
        stdout,
        "SERVICE\tPRIVATE DNS\tSOURCE\tNEXT DEPLOY\nweb\tweb\timage\tcreate\n"
    );
    assert_eq!(stderr, "");

    let (stdout, stderr) = run(&["service", "inspect", "web"]);
    assert!(stdout.starts_with("service = web\n"), "{stdout}");
    assert!(
        stdout.contains("\nimage = \"nginx:1\" (no default)\n"),
        "{stdout}"
    );
    assert!(stdout.contains("\nreplicas = 1 (default)\n"), "{stdout}");
    assert!(stdout.contains("\ndomains = "), "{stdout}");
    assert_eq!(stderr, "");

    let (stdout, stderr) = run(&["volume", "ls"]);
    assert_eq!(
        stdout,
        "VOLUME\tSTORAGE\tSHARED WRITES\tMOUNTS\tDEPLOYED\tNEXT DEPLOY\n"
    );
    assert_eq!(stderr, "No Volumes in production yet.\n");

    let (stdout, stderr) = run(&["--json", "service", "ls"]);
    let listed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(listed.is_object(), "{stdout}");
    assert_eq!(stderr, "");
}

#[test]
fn logs_reads_a_window_or_a_deployment_running_or_not() {
    for args in [
        &["ployz", "logs", "web", "--since", "2h"][..],
        &["ployz", "logs", "web", "--since", "2h", "-f"],
        &["ployz", "logs", "web", "--deployment", "dep_old"],
        &[
            "ployz",
            "logs",
            "web",
            "--deployment",
            "dep_old",
            "-n",
            "all",
            "--json",
        ],
    ] {
        assert!(
            ployz::cli::command().try_get_matches_from(args).is_ok(),
            "{args:?}"
        );
    }
    let mut command = ployz::cli::command();
    let help = command
        .find_subcommand_mut("logs")
        .unwrap()
        .render_long_help()
        .to_string();
    assert!(help.contains("running or not"), "{help}");
}

#[test]
fn ingress_help_names_both_doors_and_the_token_switch() {
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .args(["ingress", "use", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    for needle in ["public", "tunnel", "--token-stdin", "Cloudflare"] {
        assert!(stdout.contains(needle), "{needle}: {stdout}");
    }
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .args(["ingress", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("Door"), "{stdout}");
}

#[test]
fn ingress_use_needs_a_known_door_and_a_token_on_stdin_when_asked() {
    for args in [
        &["ingress", "use"][..],
        &["ingress", "use", "window"],
        &["ingress", "use", "public", "tunnel"],
    ] {
        let (code, stderr) = run_human(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
    }
    let output = ingress_use_with_stdin(&["ingress", "use", "tunnel", "--token-stdin"], b"\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("stdin holds no tunnel token"), "{stderr}");

    let output = ingress_use_with_stdin(
        &["ingress", "use", "tunnel", "--token-stdin"],
        b"not-a-real-token\n",
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(
            "Paste the whole token from the tunnel's install command. Tokens start with eyJ."
        ),
        "{stderr}"
    );

    let output = ingress_use_with_stdin(
        &["--json", "ingress", "use", "tunnel", "--token-stdin"],
        &vec![b'x'; 9000],
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(2), "{stdout}");
    assert!(stdout.contains("over 8192 bytes"), "{stdout}");
}

fn ingress_use_with_stdin(args: &[&str], stdin: &[u8]) -> std::process::Output {
    use std::io::Write;
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.yaml");
    let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .args(args)
        .args(["--ployz-config", config.to_str().unwrap()])
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("PLOYZ_CONTEXT")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_STORE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn private_access_help_names_the_key_switch_and_the_tailscale_steps() {
    let help = |args: &[&str]| {
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--why=test")
            .args(args)
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stdout}");
        stdout
    };
    let on = help(&["private-access", "on", "--help"]);
    for needle in ["--auth-key-stdin", "--accept-routes", "tailnet"] {
        assert!(on.contains(needle), "{needle}: {on}");
    }
    let off = help(&["private-access", "off", "--help"]);
    assert!(off.contains("auth key"), "{off}");
    assert!(!off.contains("--auth-key-stdin"), "{off}");
    assert!(help(&["private-access", "--help"]).contains("router"));
}

#[test]
fn private_access_on_reads_its_key_from_stdin_and_off_takes_none() {
    let output = ingress_use_with_stdin(&["private-access", "on", "--auth-key-stdin"], b"\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("stdin holds no auth key"), "{stderr}");

    let output = ingress_use_with_stdin(
        &["--json", "private-access", "on", "--auth-key-stdin"],
        &vec![b'x'; 5000],
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(2), "{stdout}");
    assert!(stdout.contains("over 4096 bytes"), "{stdout}");
    assert!(!stdout.contains(&"x".repeat(32)), "{stdout}");

    for args in [
        &["private-access", "off", "--auth-key-stdin"][..],
        &[
            "private-access",
            "on",
            "tskey-auth-pasted-on-the-command-line",
        ],
    ] {
        let (code, stderr) = run_human(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
    }
}

#[test]
fn volume_set_waits_only_for_a_size() {
    let parse = |args: &[&str]| {
        ployz::cli::command()
            .try_get_matches_from(["ployz", "volume", "set", "data"].iter().chain(args))
    };
    for args in [
        &["--size", "20"][..],
        &["--size", "20", "--wait"],
        &["--docker"],
        &["--shared-writes"],
    ] {
        parse(args).unwrap();
    }
    for args in [
        &["--wait"][..],
        &["--docker", "--wait"],
        &["--shared-writes", "--wait"],
        &["--size", "20", "--docker"],
    ] {
        assert!(parse(args).is_err(), "{args:?}");
    }
}

#[test]
fn server_rm_empty_name_uses_server_wording() {
    let (code, json, _) = run_json(&["server", "rm", "", "--confirm", "", "--json"]);
    assert_eq!(code, Some(2), "{json}");
    assert_eq!(
        json.pointer("/error/code"),
        Some(&serde_json::json!("invalid_argument"))
    );
    assert!(message(&json).contains("Name a Server"), "{json}");
}

#[test]
fn the_sdk_does_not_reach_up_into_the_cli() {
    let sdk = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/sdk"));
    let mut found = Vec::new();
    let mut dirs = vec![sdk.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy();
            if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            if source.contains("crate::cli") || source.contains("super::super::cli") {
                found.push(path.display().to_string());
            }
        }
    }
    assert!(
        found.is_empty(),
        "the SDK is the library tier and the CLI presents it; move what these share into the SDK:\n{}",
        found.join("\n")
    );
}
