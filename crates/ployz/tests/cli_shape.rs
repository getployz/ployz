#[cfg(unix)]
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::process::{Command as ProcessCommand, Stdio};

use clap_complete::{Shell, generate};

#[test]
fn listing_json_output_accepts_only_json_in_long_and_short_forms() {
    let paths: &[&[&str]] = &[
        &["changes"],
        &["ls"],
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
            .map(ployz_core::MachineRelease::as_str),
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
fn remote_build_target_requires_equals_and_preserves_positional_service() {
    for (args, target, service) in [
        (
            vec!["ployz", "build", "--remote=tower", "api"],
            "tower",
            "api",
        ),
        (vec!["ployz", "build", "--remote", "api"], "", "api"),
        (
            vec!["ployz", "deploy", "--remote=tower", "api"],
            "tower",
            "api",
        ),
        (vec!["ployz", "deploy", "--remote", "api"], "", "api"),
    ] {
        let command = *args.get(1).unwrap();
        let matches = ployz::cli::command().try_get_matches_from(&args).unwrap();
        let build = matches.subcommand_matches(command).unwrap();
        assert_eq!(
            build.get_one::<String>("remote").map(String::as_str),
            Some(target)
        );
        assert_eq!(
            build
                .get_many::<String>("service")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [service]
        );
    }
    // Both commands define --local, so the refusals below are conflicts rather
    // than an unknown flag.
    for command in ["build", "deploy"] {
        let matches = ployz::cli::command()
            .try_get_matches_from(["ployz", command, "--local", "api"])
            .unwrap();
        assert!(
            matches
                .subcommand_matches(command)
                .unwrap()
                .get_flag("local")
        );
    }
    for conflicting in [
        vec!["ployz", "build", "--local", "--remote=tower"],
        vec!["ployz", "deploy", "--local", "--remote=tower"],
        vec!["ployz", "deploy", "--no-build", "--remote=tower"],
    ] {
        assert!(
            ployz::cli::command()
                .try_get_matches_from(&conflicting)
                .is_err(),
            "{conflicting:?}"
        );
    }
}
