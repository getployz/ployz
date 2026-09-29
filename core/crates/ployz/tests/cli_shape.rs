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
    assert_eq!(
        paths,
        [
            "billing",
            "billing manage",
            "billing upgrade",
            "build",
            "cloud",
            "cloud reset",
            // Shell tooling, not a Cluster operation.
            "completion",
            "ctx",
            "ctx ls",
            "ctx rm",
            "ctx use",
            "diff",
            "discard",
            "env",
            "env new",
            "explain",
            "get",
            "ingress",
            "ingress deploy",
            "login",
            "logout",
            "org",
            "org ls",
            "org use",
            "project",
            "project new",
            "publish",
            "schema",
            "server",
            "server add",
            "server build-cache-clear",
            "server inspect",
            "server logs",
            "server ls",
            "server rm",
            "server set",
            "server upgrade",
            "service",
            "service add",
            "service exec",
            "service inspect",
            "service logs",
            "service ls",
            "service proxy",
            "service ps",
            "service rm",
            "service scale",
            "service start",
            "service stop",
            "set",
            "token",
            "token ls",
            "token new",
            "token rm",
            "unset",
            "volume",
            "volume create",
            "volume inspect",
            "volume ls",
            "volume rm",
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
fn sessions_tunnels_shell_code_and_build_refuse_json() {
    for args in [
        &["service", "exec", "--json", "api"][..],
        &["service", "proxy", "--json", "api", "8080"],
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
        assert_eq!(code, Some(1), "{args:?}: {stderr}");
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
        .env_remove("PLOYZ_CLOUD_URL")
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
fn cloud_commands_act_with_ployz_token_or_the_signed_in_device() {
    for args in [
        &["token", "ls", "--json"][..],
        &["org", "ls", "--json"],
        &["billing", "--json"],
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
    let (code, json, _) = run_json_with(&["billing", "--json"], &token);
    assert_eq!(code, Some(1));
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "unavailable",
        "{json}"
    );
    assert!(!json.to_string().contains("ployz_secret"), "{json}");

    let (code, json, _) = run_json_with(&["org", "use", "acme", "--json"], &token);
    assert_eq!(code, Some(1));
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "unsupported",
        "{json}"
    );

    let (code, json, _) = run_json(&["token", "new", "ci", "--expires-in", "0", "--json"]);
    assert_eq!(code, Some(2), "{json}");
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
        &["ployz", "deploy"][..],
        &["ployz", "changes"],
        &["ployz", "build"],
        &["ployz", "run", "nginx"],
        &["ployz", "service", "run", "nginx"],
        &["ployz", "service", "logs", "--file", "compose.yaml", "api"],
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
fn ingress_deploy_accepts_repeated_constraints_and_rejects_legacy_machine_selection() {
    let matches = ployz::cli::command()
        .try_get_matches_from([
            "ployz",
            "ingress",
            "deploy",
            "--constraint",
            "node.labels.region==west",
            "--constraint",
            "node.labels.retired!=true",
        ])
        .unwrap();
    let deploy = matches
        .subcommand_matches("ingress")
        .unwrap()
        .subcommand_matches("deploy")
        .unwrap();
    assert_eq!(
        deploy
            .get_many::<String>("constraint")
            .unwrap()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["node.labels.region==west", "node.labels.retired!=true"]
    );
    assert!(
        ployz::cli::command()
            .try_get_matches_from(["ployz", "ingress", "deploy", "--machine", "edge",])
            .is_err()
    );
}
