#[cfg(unix)]
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::process::{Command as ProcessCommand, Stdio};

use clap_complete::{Shell, generate};

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
            "cloud",
            "cloud enroll",
            // Shell tooling, not a Cluster operation.
            "completion",
            "ctx",
            "ctx connection",
            "ctx ls",
            "ctx rm",
            "ctx show",
            "ctx use",
            "ingress",
            "ingress config",
            "ingress deploy",
            "ingress logs",
            "machine",
            "machine add",
            "machine build-cache-clear",
            "machine init",
            "machine inspect",
            "machine logs",
            "machine ls",
            "machine rename",
            "machine rm",
            "machine rtt",
            "machine update",
            "machine upgrade",
            "machine upgrade inspect",
            "project",
            "project ls",
            "project rm",
            "proxy",
            "ps",
            "service",
            "service exec",
            "service inspect",
            "service logs",
            "service ls",
            "service rm",
            "service scale",
            "service start",
            "service stop",
            "version",
            "volume",
            "volume create",
            "volume inspect",
            "volume ls",
            "volume rm",
        ]
    );
}

#[test]
fn version_takes_no_output_template() {
    for flag in ["-o", "--output"] {
        assert!(
            ployz::cli::command()
                .try_get_matches_from(["ployz", "version", flag, "{{.Version}}"])
                .is_err(),
            "{flag}"
        );
    }
}

#[test]
fn listing_json_output_accepts_only_json_in_long_and_short_forms() {
    let paths: &[&[&str]] = &[
        &["ps"],
        &["service", "ls"],
        &["volume", "ls"],
        &["project", "ls"],
    ];
    for path in paths {
        for flag in ["--output", "-o"] {
            let mut args = vec!["ployz"];
            args.extend_from_slice(path);
            args.extend([flag, "json"]);
            let matches = ployz::cli::command().try_get_matches_from(args).unwrap();
            let mut leaf = &matches;
            while let Some((_, child)) = leaf.subcommand() {
                leaf = child;
            }
            assert_eq!(
                leaf.get_one::<String>("output").map(String::as_str),
                Some("json"),
                "{} {flag}",
                path.join(" ")
            );
        }

        let mut args = vec!["ployz"];
        args.extend_from_slice(path);
        args.extend(["--output", "yaml"]);
        assert!(
            ployz::cli::command().try_get_matches_from(args).is_err(),
            "{} accepted a non-JSON output format",
            path.join(" ")
        );
    }
}

#[test]
fn native_completion_is_generated_for_every_supported_shell() {
    for shell in [
        Shell::Bash,
        Shell::Elvish,
        Shell::Fish,
        Shell::PowerShell,
        Shell::Zsh,
    ] {
        let mut output = Vec::new();
        generate(shell, &mut ployz::cli::command(), "ployz", &mut output);
        let output = String::from_utf8(output).unwrap();
        assert!(!output.is_empty(), "empty {shell:?} completion");
        assert!(output.contains("ployz"), "unnamed {shell:?} completion");
    }
}

#[test]
fn machine_upgrade_requires_explicit_targets_and_has_typed_inspection() {
    let command = ployz::cli::command();
    let request = command
        .clone()
        .try_get_matches_from([
            "ployz",
            "machine",
            "upgrade",
            "1.2.3-beta.4",
            "--machine",
            "edge-a",
            "--machine",
            "0123456789abcdef0123456789abcdef",
        ])
        .unwrap();
    let upgrade = request
        .subcommand_matches("machine")
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
            .get_many::<String>("machine")
            .unwrap()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["edge-a", "0123456789abcdef0123456789abcdef"]
    );
    assert!(
        command
            .clone()
            .try_get_matches_from(["ployz", "machine", "upgrade", "stable"])
            .is_err()
    );
    assert!(
        command
            .clone()
            .try_get_matches_from([
                "ployz",
                "machine",
                "upgrade",
                "nightly",
                "--machine",
                "edge-a"
            ])
            .is_err()
    );

    let inspect = command
        .try_get_matches_from([
            "ployz",
            "machine",
            "upgrade",
            "inspect",
            "edge-a",
            "--attempt",
            "0123456789abcdef0123456789abcdef",
            "-o",
            "json",
        ])
        .unwrap();
    let inspect = inspect
        .subcommand_matches("machine")
        .unwrap()
        .subcommand_matches("upgrade")
        .unwrap()
        .subcommand_matches("inspect")
        .unwrap();
    assert_eq!(
        inspect
            .get_one::<ployz_core::MachineUpgradeAttemptId>("attempt")
            .map(ToString::to_string),
        Some("0123456789abcdef0123456789abcdef".into())
    );
    assert_eq!(
        inspect.get_one::<String>("output").map(String::as_str),
        Some("json")
    );
}

#[cfg(unix)]
#[test]
fn completion_exits_on_sigpipe_when_the_reader_closes_after_one_line() {
    use std::os::unix::process::ExitStatusExt;

    let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["completion", "bash"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut first_line = String::new();
    output.read_line(&mut first_line).unwrap();
    drop(output);

    assert!(!first_line.is_empty());
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
        vec!["machine", "update", "node"],
        vec!["machine", "init"],
        vec!["machine", "add", "root@node"],
        vec!["cloud", "enroll", "pmet_test"],
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
            args.get(2) == Some(&"update")
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
