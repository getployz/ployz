#[cfg(unix)]
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::process::{Command as ProcessCommand, Stdio};

use clap_complete::{Shell, generate};

#[test]
fn listing_json_output_accepts_only_json_in_long_and_short_forms() {
    let paths: &[&[&str]] = &[
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
