#[cfg(unix)]
use std::process::{Command as ProcessCommand, Stdio};

#[test]
fn command_tree_is_exactly_the_cluster_operations_without_aliases() {
    fn collect(command: &clap::Command, parent: &str, paths: &mut Vec<String>) {
        for child in command.get_subcommands() {
            let path = format!("{parent}{}", child.get_name());
            assert_eq!(
                child.get_all_aliases().count(),
                0,
                "{path} declares an alias"
            );
            collect(child, &format!("{path} "), paths);
            paths.push(path);
        }
    }
    let mut paths = Vec::new();
    collect(&ployz::cli::command(), "", &mut paths);
    paths.sort_unstable();
    // `ployz debug` exists only in a verify-cluster build.
    if cfg!(feature = "verify-faults") {
        paths.retain(|path| path != "debug" && path != "debug volume-rpc");
    }
    assert_eq!(
        paths,
        [
            "build",
            "cloud",
            "cloud reset",
            // Shell tooling, not a Cluster operation.
            "completion",
            "ctx",
            "ctx ls",
            "ctx rm",
            "ctx use",
            "deploy",
            "deployment",
            "deployment cancel",
            "deployment ls",
            "deployment retry",
            "deployment show",
            "deployment start",
            "diff",
            "discard",
            "domain",
            "domain add",
            "domain check",
            "domain ls",
            "domain rm",
            "domain set",
            "env",
            "env branch",
            "env copy",
            "env default",
            "env keep",
            "env ls",
            "env never-sync",
            "env new",
            "env pr",
            "env rm",
            "env setup",
            "env shutdown",
            "env sync",
            "exec",
            "explain",
            "get",
            "github",
            "github connect",
            "github disconnect",
            "github ls",
            "link",
            "login",
            "logout",
            "logs",
            "org",
            "org build-order",
            "org ls",
            "org rm",
            "org use",
            "project",
            "project ls",
            "project new",
            "project rename",
            "project rm",
            "ps",
            "publish",
            "schema",
            "server",
            "server add",
            "server build-cache-clear",
            "server clean",
            "server drain",
            "server forget",
            "server inspect",
            "server logs",
            "server ls",
            "server rm",
            "server set",
            "server upgrade",
            "service",
            "service add",
            "service inspect",
            "service ls",
            "service port-forward",
            "service rename",
            "service restart",
            "service rm",
            "service start",
            "service stop",
            "set",
            "status",
            "token",
            "token ls",
            "token new",
            "token rm",
            "unset",
            "up",
            "volume",
            "volume add",
            "volume inspect",
            "volume ls",
            "volume mirror",
            "volume mirror rm",
            "volume move",
            "volume release",
            "volume rename",
            "volume rm",
            "volume runs",
            "volume set",
            "volume sync",
        ]
    );
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
    run_json_with(args, &[])
}

/// [`run_json`] with extra environment variables.
fn run_json_with(args: &[&str], envs: &[(&str, &str)]) -> (Option<i32>, serde_json::Value, String) {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.yaml");
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .args(["--ployz-config", config.to_str().unwrap()])
        .env("HOME", home.path())
        .env_remove("PLOYZ_CONTEXT")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_STORE")
        .envs(envs.iter().copied())
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
    assert_eq!(json, serde_json::json!({ "contexts": [] }));

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
            .args(args)
            .args(["--json", "--ployz-config", config.to_str().unwrap()])
            .env("HOME", home.path())
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
        "ui::row(",
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
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.yaml");
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .args(["--ployz-config", config.to_str().unwrap()])
        .env("HOME", home.path())
        .env_remove("PLOYZ_CONTEXT")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_STORE")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stderr).unwrap(),
    )
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
    ] {
        let (code, stderr) = run_human(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        if !args.contains(&"--json") {
            assert!(stderr.starts_with("error: "), "{args:?}: {stderr}");
        }
    }
}

#[test]
fn a_cut_valid_list_still_names_the_closest_setting() {
    let (code, stderr) = run_human(&["explain", "web.restart_policy"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("\nvalid: did you mean restartPolicy?\nvalid: "),
        "{stderr}"
    );
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
            .arg(group)
            .env("HOME", home.path())
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
        &["status", "--json"],
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
fn root_help_ends_with_the_catalog_pointer() {
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
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
fn piped_output_exits_on_sigpipe_when_the_reader_is_gone() {
    use std::os::unix::process::ExitStatusExt;

    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["schema"])
        .stdout(Stdio::from(writer))
        .spawn()
        .unwrap();
    assert_eq!(child.wait().unwrap().signal(), Some(13));
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
        vec!["server", "add", "--token", "pmet_test"],
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

#[test]
fn results_on_stdout_side_channel_on_stderr() {
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
            .args(args)
            .env("HOME", home.path())
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
    assert!(stdout.contains("\nimage = \"nginx:1\"\n"), "{stdout}");
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
