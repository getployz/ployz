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
        .args(args)
        .args(["--ployz-config", config.to_str().unwrap()])
        .env("HOME", home.path())
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

/// One waiting server as Cloud lists it.
fn waiting_row(id: &str, name: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": name,
        "status": status,
        "observed_ip": "203.0.113.9",
        "fleet_token": "amber-falcon",
        "report": {
            "hostname": name,
            "provider": "hetzner",
            "os": { "id": "ubuntu", "version": "24.04" },
            "kernel": "6.8.0",
            "arch": "x86_64",
            "virt": "kvm",
            "cpuCount": 4,
            "memoryTotalBytes": 8_000_000_000_u64,
            "disks": [{ "path": "/dev/sda", "sizeBytes": 40_000_000_000_u64, "role": "root" }],
            "docker": { "kind": "absent" },
            "cgroup": "v2",
            "zfs": { "kind": "ineligible", "code": "no_disk", "message": "no spare disk" },
            "blockers": [{ "code": "port_in_use", "message": "port 443 is in use" }],
            "warnings": [],
        },
        "first_seen_at": "2026-10-16T00:00:00.000Z",
        "last_check_in_at": "2026-10-16T00:05:00.000Z",
    })
}

/// What the fake Cloud answers: the waiting list and its status, the Servers it can't
/// reach, and, for each reject in turn, a status and an optional short count. A reject
/// past the end answers 200 with every id it was sent.
struct WaitingCloud {
    rows: Vec<serde_json::Value>,
    list: &'static str,
    unreachable: Vec<String>,
    rejects: Vec<(&'static str, Option<usize>)>,
}

impl WaitingCloud {
    /// web-1, web-2, db-1, two hosts named twin and one named after db-1's id.
    fn six() -> Self {
        Self::of(vec![
            waiting_row("00000000-0000-4000-8000-000000000001", "web-1", "blocked"),
            waiting_row("00000000-0000-4000-8000-000000000002", "web-2", "waiting"),
            waiting_row("00000000-0000-4000-8000-000000000003", "db-1", "waiting"),
            waiting_row("00000000-0000-4000-8000-000000000004", "twin", "waiting"),
            waiting_row("00000000-0000-4000-8000-000000000005", "twin", "waiting"),
            waiting_row(
                "00000000-0000-4000-8000-000000000006",
                "00000000-0000-4000-8000-000000000003",
                "waiting",
            ),
        ])
    }

    fn of(rows: Vec<serde_json::Value>) -> Self {
        Self {
            rows,
            list: "200 OK",
            unreachable: Vec::new(),
            rejects: Vec::new(),
        }
    }
}

/// Serve `cloud` on a local port, recording each route with the body of a POST, and
/// answering anything else with 404.
fn waiting_cloud(
    routes: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    cloud: WaitingCloud,
) -> String {
    use std::io::{BufRead, Read, Write};
    let WaitingCloud {
        rows,
        list,
        unreachable,
        rejects,
    } = cloud;
    let listed = serde_json::json!({ "waiting_servers": rows });
    let mut rejects = rejects.into_iter();
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
                "GET /api/cli/waiting-servers" => (list, listed.clone()),
                "POST /api/cli/server-access" => (
                    "200 OK",
                    serde_json::json!({ "connections": [], "unreachable": unreachable }),
                ),
                "POST /api/cli/reject-servers" => {
                    let sent: serde_json::Value = serde_json::from_slice(&sent).unwrap();
                    let ids = sent
                        .get("ids")
                        .and_then(serde_json::Value::as_array)
                        .unwrap()
                        .len();
                    let (status, short) = rejects.next().unwrap_or(("200 OK", None));
                    (
                        status,
                        serde_json::json!({ "rejected": short.unwrap_or(ids) }),
                    )
                }
                _ => ("404 Not Found", serde_json::json!({ "error": "not found" })),
            };
            let recorded = match route.starts_with("POST") {
                true => format!("{route} {}", String::from_utf8_lossy(&sent)),
                false => route,
            };
            routes.lock().unwrap().push(recorded);
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
fn server_ls_waiting_show_and_reject_read_waiting_servers_from_cloud() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cloud = waiting_cloud(routes.clone(), WaitingCloud::six());
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];
    let posted = || -> Vec<String> {
        routes
            .lock()
            .unwrap()
            .iter()
            .filter(|route| route.starts_with("POST /api/cli/reject-servers"))
            .cloned()
            .collect()
    };

    let (code, json, _) = run_json_with(&["server", "ls", "--waiting", "--json"], &token);
    assert_eq!(code, Some(0), "{json}");
    let names: Vec<_> = json
        .pointer("/waiting_servers")
        .and_then(serde_json::Value::as_array)
        .unwrap()
        .iter()
        .map(|row| row["name"].clone())
        .collect();
    assert_eq!(
        names,
        [
            "web-1",
            "web-2",
            "db-1",
            "twin",
            "twin",
            "00000000-0000-4000-8000-000000000003"
        ],
        "{json}"
    );

    let (code, json, _) = run_json_with(&["server", "show", "web-1", "--json"], &token);
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(json.pointer("/waiting_server/name").unwrap(), "web-1");
    assert_eq!(
        json.pointer("/waiting_server/fleet_token").unwrap(),
        "amber-falcon"
    );

    let (code, json, _) = run_json_with(&["server", "show", "nobody", "--json"], &token);
    assert_eq!(code, Some(1), "{json}");

    let (code, json, _) = run_json_with(
        &[
            "server",
            "show",
            "00000000-0000-4000-8000-000000000003",
            "--json",
        ],
        &token,
    );
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(json.pointer("/waiting_server/name").unwrap(), "db-1");

    let (code, json, _) = run_json_with(&["server", "show", "twin", "--json"], &token);
    assert_eq!(code, Some(2), "{json}");
    assert_eq!(json.pointer("/error/code").unwrap(), "ambiguous");
    assert_eq!(
        message(&json),
        "2 waiting servers are named twin. Show one by its id: \
         00000000-0000-4000-8000-000000000004 (203.0.113.9), \
         00000000-0000-4000-8000-000000000005 (203.0.113.9)"
    );

    let (code, json, _) = run_json_with(&["server", "reject", "web-*", "--json"], &token);
    assert_eq!(code, Some(2), "{json}");
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "confirmation_required"
    );
    assert_eq!(
        json.pointer("/error/details/waiting_servers").unwrap(),
        &serde_json::json!([
            {
                "id": "00000000-0000-4000-8000-000000000001",
                "name": "web-1",
                "ip": "203.0.113.9",
                "fleet_token": "amber-falcon",
            },
            {
                "id": "00000000-0000-4000-8000-000000000002",
                "name": "web-2",
                "ip": "203.0.113.9",
                "fleet_token": "amber-falcon",
            },
        ])
    );
    assert!(posted().is_empty(), "{:?}", posted());

    let (code, json, _) = run_json_with(&["server", "reject", "web-*", "--yes", "--json"], &token);
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(json.get("rejected"), Some(&serde_json::json!(2)), "{json}");
    assert_eq!(json.get("matched"), Some(&serde_json::json!(2)), "{json}");
    assert_eq!(
        posted(),
        [concat!(
            r#"POST /api/cli/reject-servers {"ids":["#,
            r#""00000000-0000-4000-8000-000000000001","#,
            r#""00000000-0000-4000-8000-000000000002"]}"#
        )]
    );

    let (code, json, _) =
        run_json_with(&["server", "reject", "nomatch", "--yes", "--json"], &token);
    assert_eq!(code, Some(1), "{json}");
    assert_eq!(json.pointer("/error/code").unwrap(), "not_found");
    assert_eq!(
        message(&json),
        "No waiting server matches nomatch. Nothing rejected."
    );
    assert_eq!(posted().len(), 1);

    let (code, json, _) = run_json_with(
        &[
            "server",
            "reject",
            "00000000-0000-4000-8000-000000000003",
            "--yes",
            "--json",
        ],
        &token,
    );
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(
        posted().last().unwrap(),
        r#"POST /api/cli/reject-servers {"ids":["00000000-0000-4000-8000-000000000003"]}"#
    );

    let (code, json, _) = run_json_with(
        &[
            "server",
            "ls",
            "--waiting",
            "--context",
            "staging",
            "--json",
        ],
        &token,
    );
    assert_eq!(code, Some(2), "{json}");
}

/// Reject requests recorded by [`waiting_cloud`], as the ids each one sent.
/// Run the binary for human output with `env`; returns (exit code, stdout, stderr).
fn run_output(args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, Option<String>, String) {
    let home = tempfile::tempdir().unwrap();
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .env("HOME", home.path())
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

fn rejected_ids(routes: &std::sync::Mutex<Vec<String>>) -> Vec<Vec<String>> {
    routes
        .lock()
        .unwrap()
        .iter()
        .filter_map(|route| route.strip_prefix("POST /api/cli/reject-servers "))
        .map(|body| {
            serde_json::from_str::<serde_json::Value>(body)
                .unwrap()
                .get("ids")
                .and_then(serde_json::Value::as_array)
                .unwrap()
                .iter()
                .map(|id| id.as_str().unwrap().to_owned())
                .collect()
        })
        .collect()
}

#[test]
fn server_reject_matches_only_the_name_that_prints_like_another() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cloud = waiting_cloud(
        routes.clone(),
        WaitingCloud::of(vec![
            waiting_row(
                "00000000-0000-4000-8000-000000000001",
                "web\u{7}",
                "waiting",
            ),
            waiting_row(
                "00000000-0000-4000-8000-000000000002",
                "web\u{FFFD}",
                "waiting",
            ),
        ]),
    );
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];

    let (code, json, _) = run_json_with(&["server", "ls", "--waiting", "--json"], &token);
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(json.pointer("/waiting_servers/0/name").unwrap(), "web\u{7}");

    let (code, json, _) = run_json_with(&["server", "show", "web\u{7}", "--json"], &token);
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(json.pointer("/waiting_server/name").unwrap(), "web\u{7}");

    let (code, json, _) = run_json_with(
        &["server", "reject", "web\u{FFFD}", "--yes", "--json"],
        &token,
    );
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(
        rejected_ids(&routes),
        [["00000000-0000-4000-8000-000000000002"]]
    );
    assert_eq!(
        json.pointer("/waiting_servers/0/name").unwrap(),
        "web\u{FFFD}"
    );
}

#[test]
fn server_waiting_commands_ignore_a_context_from_the_environment() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cloud = waiting_cloud(routes.clone(), WaitingCloud::six());
    let token = [
        ("PLOYZ_TOKEN", "ployz_secret"),
        ("PLOYZ_CLOUD_URL", &cloud),
        ("PLOYZ_CONTEXT", "staging"),
    ];

    let (code, json, _) = run_json_with(&["server", "ls", "--waiting", "--json"], &token);
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(json.pointer("/waiting_servers/0/name").unwrap(), "web-1");

    let (code, json, _) = run_json_with(&["server", "show", "web-1", "--json"], &token);
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(json.pointer("/waiting_server/name").unwrap(), "web-1");

    let (code, json, _) = run_json_with(&["server", "reject", "web-2", "--yes", "--json"], &token);
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(
        rejected_ids(&routes),
        [["00000000-0000-4000-8000-000000000002"]]
    );

    let (code, stdout, _) = run_output(&["server", "ls", "--waiting"], &token);
    let stdout = stdout.unwrap();
    assert_eq!(code, Some(0), "{stdout}");
    let header: Vec<_> = stdout.lines().next().unwrap().split_whitespace().collect();
    assert_eq!(header, ["ID", "NAME", "PROVIDER", "IP", "STATUS", "AGE"]);
    let first: Vec<_> = stdout.lines().nth(1).unwrap().split_whitespace().collect();
    assert_eq!(
        first.get(..2).unwrap(),
        ["00000000-000", "web-1"],
        "{stdout}"
    );

    let (code, _, stderr) = run_output(&["server", "reject", "web-1", "--yes"], &token);
    assert_eq!(code, Some(0), "{stderr}");
    let header: Vec<_> = stderr.lines().next().unwrap().split_whitespace().collect();
    assert_eq!(header, ["ID", "NAME", "IP", "TOKEN"]);
}

#[test]
fn server_reject_reports_what_went_through_before_a_batch_failed() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let rows = (0..1001)
        .map(|index| {
            waiting_row(
                &format!("00000000-0000-4000-8000-{index:012}"),
                &format!("bulk-{index}"),
                "waiting",
            )
        })
        .collect();
    let cloud = waiting_cloud(
        routes.clone(),
        WaitingCloud {
            rejects: vec![("200 OK", None), ("503 Service Unavailable", None)],
            ..WaitingCloud::of(rows)
        },
    );
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];

    let (code, json, _) = run_json_with(&["server", "reject", "bulk-*", "--yes", "--json"], &token);
    assert_eq!(code, Some(3), "{json}");
    let sent = rejected_ids(&routes);
    assert_eq!(sent.iter().map(Vec::len).collect::<Vec<_>>(), [1000, 1]);
    assert_eq!(json.get("rejected").unwrap(), 1000, "{json}");
    assert_eq!(json.get("matched").unwrap(), 1001, "{json}");
    assert_eq!(
        json.get("uncertain").unwrap(),
        &serde_json::json!(sent.get(1).unwrap()),
        "{json}"
    );
    assert_eq!(
        json.get("unattempted").unwrap(),
        &serde_json::json!([]),
        "{json}"
    );
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "unavailable",
        "{json}"
    );
}

#[test]
fn server_reject_short_count_says_the_rest_had_already_left() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cloud = waiting_cloud(
        routes,
        WaitingCloud {
            rejects: vec![("200 OK", Some(1)), ("200 OK", Some(1))],
            ..WaitingCloud::six()
        },
    );
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];

    let (code, json, _) = run_json_with(&["server", "reject", "web-*", "--yes", "--json"], &token);
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(json.get("rejected").unwrap(), 1, "{json}");
    assert_eq!(json.get("matched").unwrap(), 2, "{json}");

    let human = || {
        run_output(&["server", "reject", "web-*", "--yes"], &token)
            .1
            .unwrap()
    };
    let short = human();
    assert!(
        short.contains("1 of 2 rejected; the rest had already left the list."),
        "{short}"
    );
    let full = human();
    assert!(
        full.contains("2 rejected. They won't come back under the same Fleet Token."),
        "{full}"
    );
}

#[test]
fn server_show_falls_back_to_the_waiting_list_when_no_server_answers() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cloud = waiting_cloud(
        routes,
        WaitingCloud {
            unreachable: vec!["b".repeat(32)],
            ..WaitingCloud::six()
        },
    );
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];

    let (code, json, _) = run_json_with(&["server", "show", "web-1", "--json"], &token);
    assert_eq!(code, Some(0), "{json}");
    assert_eq!(json.pointer("/waiting_server/name").unwrap(), "web-1");

    let (code, json, _) = run_json_with(&["server", "show", "nobody", "--json"], &token);
    assert_eq!(code, Some(1), "{json}");
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "unavailable",
        "{json}"
    );
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
        .args(["--json", "server", "ls", "--connect", &connect])
        .args(["--ployz-config", config.to_str().unwrap()])
        .env("HOME", home.path())
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
    assert!(
        stdout.contains("\nimage = \"nginx:1\" (default -)\n"),
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
fn server_show_says_when_the_waiting_list_could_not_be_read() {
    let routes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cloud = waiting_cloud(
        routes,
        WaitingCloud {
            list: "503 Service Unavailable",
            ..WaitingCloud::six()
        },
    );
    let token = [("PLOYZ_TOKEN", "ployz_secret"), ("PLOYZ_CLOUD_URL", &cloud)];
    let (code, json, _) = run_json_with(&["server", "show", "nobody", "--json"], &token);
    assert_ne!(code, Some(0), "{json}");
    assert_eq!(
        message(&json),
        "No Server matches nobody, and the servers waiting to join could not be read."
    );
}

#[test]
fn checkin_status_signed_out_says_who_without_a_scope_error() {
    let (code, json, stderr) = run_json(&["status", "--json"]);
    assert_eq!(code, Some(0), "{json} {stderr}");
    assert_eq!(
        json.get("identity"),
        Some(&serde_json::Value::Null),
        "{json}"
    );
    assert_eq!(
        json.get("attention"),
        Some(&serde_json::json!([])),
        "{json}"
    );
    assert_eq!(
        json.get("next").and_then(serde_json::Value::as_str),
        Some("ployz login"),
        "{json}"
    );
}

#[test]
fn checkin_help_names_install_and_the_token_file() {
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["server", "checkin", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    for needle in [
        "--install",
        "--token-file",
        "/etc/ployz/fleet-token",
        "--cloud-url",
    ] {
        assert!(stdout.contains(needle), "{needle}: {stdout}");
    }
}

#[test]
fn server_token_verbs_need_a_sign_in() {
    for args in [
        &["server", "token", "create", "--json"][..],
        &["server", "token", "ls", "--json"],
        &["server", "token", "revoke", "amber-falcon", "--json"],
    ] {
        let (code, json, _) = run_json(args);
        assert_eq!(code, Some(1), "{args:?}: {json}");
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
}

#[test]
fn server_token_create_takes_a_known_expiry_and_max_only_with_auto() {
    for args in [
        &["server", "token", "create", "--expires", "2d"][..],
        &["server", "token", "create", "--max", "5"],
        &["server", "token", "create", "--auto"],
        &["server", "token", "create", "--auto", "--max", "0"],
        &["server", "token", "revoke"],
    ] {
        let (code, stderr) = run_human(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
    }
}

/// A Cloud that answers one request with `status` and `body`; yields the request it got.
fn token_cloud(status: u16, body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{BufRead, BufReader, Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let cloud = format!("http://{}", listener.local_addr().unwrap());
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
            if line == "\r\n" {
                break;
            }
            request.push_str(&line);
        }
        let mut sent = vec![0; length];
        reader.read_exact(&mut sent).unwrap();
        request.push_str(&String::from_utf8(sent).unwrap());
        let reply = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        reader.get_mut().write_all(reply.as_bytes()).unwrap();
        sender.send(request).unwrap();
    });
    (cloud, received)
}

/// Run against `cloud` with an Organization Token; returns (exit code, stdout, stderr).
fn run_token_cloud(cloud: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.yaml");
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .args(["--ployz-config", config.to_str().unwrap()])
        .env("HOME", home.path())
        .env_remove("PLOYZ_CONTEXT")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_STORE")
        .env("PLOYZ_TOKEN", "ployz_secret")
        .env("PLOYZ_CLOUD_URL", cloud)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn sent_body(request: &str) -> serde_json::Value {
    serde_json::from_str(&request[request.find('{').unwrap()..]).unwrap()
}

#[test]
fn server_token_create_prints_the_secret_once_with_its_install_line() {
    let (cloud, request) = token_cloud(
        200,
        r#"{"fleet_token":{"id":"0d6f1f3e-0000-4000-8000-000000000001","name":"blue-heron","token":"pflt_secret","expires_at":"2026-10-23T00:00:00.000Z"}}"#,
    );
    let (code, stdout, stderr) = run_token_cloud(
        &cloud,
        &[
            "server",
            "token",
            "create",
            "--name",
            "blue-heron",
            "--expires",
            "7d",
            "--auto",
            "--max",
            "5",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stdout, "pflt_secret\n");
    let request = request.recv().unwrap();
    assert!(
        request.starts_with("POST /api/cli/fleet-tokens "),
        "{request}"
    );
    assert_eq!(
        sent_body(&request),
        serde_json::json!({ "name": "blue-heron", "expires_in": "7d", "auto_enroll_max": 5 })
    );
    for line in [
        "Created blue-heron\n",
        "Anyone with this token can add servers. Keep it secret.",
        &format!(
            "curl -fsSL https://ployz.sh | sudo PLOYZ_CLOUD_URL='{cloud}' PLOYZ_ENROLL_TOKEN=pflt_secret sh"
        ),
        "Shown once. Expires in 7d.",
    ] {
        assert!(stderr.contains(line), "{line:?} missing from {stderr}");
    }
}

#[test]
fn server_token_create_defaults_to_a_day_without_auto() {
    let (cloud, request) = token_cloud(
        200,
        r#"{"fleet_token":{"id":"0d6f1f3e-0000-4000-8000-000000000001","name":"amber-falcon","token":"pflt_secret","expires_at":"2026-10-17T00:00:00.000Z"}}"#,
    );
    let (code, stdout, stderr) = run_token_cloud(&cloud, &["server", "token", "create"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stdout, "pflt_secret\n");
    assert_eq!(
        sent_body(&request.recv().unwrap()),
        serde_json::json!({ "expires_in": "1d" })
    );
    assert!(stderr.contains("Shown once. Expires in 24h."), "{stderr}");
    assert!(!stderr.contains("Keep it secret"), "{stderr}");
}

#[test]
fn server_token_create_json_carries_the_secret_and_install_line() {
    let (cloud, _) = token_cloud(
        200,
        r#"{"fleet_token":{"id":"0d6f1f3e-0000-4000-8000-000000000001","name":"amber-falcon","token":"pflt_secret","expires_at":"2026-10-17T00:00:00.000Z"}}"#,
    );
    let (code, stdout, stderr) = run_token_cloud(&cloud, &["server", "token", "create", "--json"]);
    assert_eq!(code, Some(0), "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json.pointer("/fleet_token/token").unwrap(), "pflt_secret");
    assert_eq!(json.pointer("/fleet_token/name").unwrap(), "amber-falcon");
    assert!(
        json.pointer("/install")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .ends_with("PLOYZ_ENROLL_TOKEN=pflt_secret sh"),
        "{json}"
    );
}

#[test]
fn server_token_create_auto_json_carries_the_keep_it_secret_warning() {
    let (cloud, _) = token_cloud(
        200,
        r#"{"fleet_token":{"id":"0d6f1f3e-0000-4000-8000-000000000001","name":"blue-heron","token":"pflt_secret","expires_at":"2026-10-17T00:00:00.000Z"}}"#,
    );
    let (code, stdout, stderr) = run_token_cloud(
        &cloud,
        &[
            "server", "token", "create", "--auto", "--max", "5", "--json",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        json.pointer("/warnings/0").unwrap(),
        "Anyone with this token can add servers. Keep it secret.",
        "{json}"
    );
}

#[test]
fn server_token_forbidden_reads_as_unauthenticated() {
    let (cloud, _) = token_cloud(
        403,
        r#"{"_tag":"PublicError","code":"FORBIDDEN","message":"This token can't manage Fleet Tokens."}"#,
    );
    let (code, json, stderr) = run_json_with(
        &["server", "token", "ls", "--json"],
        &[
            ("PLOYZ_TOKEN", "ployz_secret"),
            ("PLOYZ_CLOUD_URL", cloud.as_str()),
        ],
    );
    assert_ne!(code, Some(0), "{stderr}");
    assert_eq!(
        json.pointer("/error/code").unwrap(),
        "unauthenticated",
        "{json}"
    );
    assert_eq!(message(&json), "This token can't manage Fleet Tokens.");
}

#[test]
fn server_token_ls_json_keys_the_tokens_by_noun() {
    let (cloud, request) = token_cloud(
        200,
        r#"{"fleet_tokens":[{"id":"0d6f1f3e-0000-4000-8000-000000000001","name":"blue-heron","expires_at":"2026-10-23T00:00:00.000Z","expired":false,"auto_enroll_max":5,"last_used_at":null,"created_at":"2026-10-16T00:00:00.000Z"}]}"#,
    );
    let (code, stdout, stderr) = run_token_cloud(&cloud, &["server", "token", "ls", "--json"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        request
            .recv()
            .unwrap()
            .starts_with("GET /api/cli/fleet-tokens ")
    );
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json.pointer("/fleet_tokens/0/name").unwrap(), "blue-heron");
    assert_eq!(json.pointer("/fleet_tokens/0/auto_enroll_max").unwrap(), 5);
    assert!(
        json.pointer("/fleet_tokens/0/id").is_none(),
        "tokens are named, not numbered: {json}"
    );
}

#[test]
fn server_token_revoke_reports_the_waiting_servers_it_removed() {
    let (cloud, request) = token_cloud(200, r#"{"waiting_removed":2}"#);
    let (code, stdout, stderr) =
        run_token_cloud(&cloud, &["server", "token", "revoke", "blue-heron"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        request
            .recv()
            .unwrap()
            .starts_with("DELETE /api/cli/fleet-tokens/blue-heron ")
    );
    assert_eq!(stdout, "Revoked blue-heron. 2 waiting servers removed.\n");
}

#[test]
fn server_token_revoke_of_an_unknown_name_names_it() {
    let (cloud, _) = token_cloud(
        404,
        r#"{"_tag":"PublicError","code":"NOT_FOUND","message":"No such Fleet Token."}"#,
    );
    let (code, json, stderr) = run_json_with(
        &["server", "token", "revoke", "ghost", "--json"],
        &[
            ("PLOYZ_TOKEN", "ployz_secret"),
            ("PLOYZ_CLOUD_URL", cloud.as_str()),
        ],
    );
    assert_eq!(code, Some(1), "{stderr}");
    assert_eq!(json.pointer("/error/code").unwrap(), "not_found", "{json}");
    assert_eq!(message(&json), "No Fleet Token named ghost.");
    assert_eq!(
        json.pointer("/error/details/next").unwrap(),
        "ployz server token ls",
        "{json}"
    );
}

#[test]
fn server_token_refusals_reach_the_reader_as_cloud_sent_them() {
    let (cloud, _) = token_cloud(
        422,
        r#"{"_tag":"PublicError","code":"VALIDATION_FAILED","message":"Only a signed-in person can make or revoke tokens; run `ployz login`."}"#,
    );
    let (code, json, stderr) = run_json_with(
        &["server", "token", "create", "--json"],
        &[
            ("PLOYZ_TOKEN", "ployz_secret"),
            ("PLOYZ_CLOUD_URL", cloud.as_str()),
        ],
    );
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(
        message(&json),
        "Only a signed-in person can make or revoke tokens; run `ployz login`."
    );
}
