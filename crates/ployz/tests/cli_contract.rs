use std::fs;
use std::process::{Command, Output};

#[test]
fn bare_cli_and_help_advertise_v2_operator_commands() {
    for args in [Vec::new(), vec!["--help"]] {
        let output = run(&args, None);

        assert!(output.status.success(), "{}", stderr(&output));
        let stdout = stdout(&output);
        assert!(stdout.contains("telemetry"));
        assert!(stdout.contains("  init"));
        assert!(stdout.contains("  machine"));
        assert!(stdout.contains("  token"));
        assert!(stdout.contains("  namespace"));
        assert!(stdout.contains("  deploy"));
        assert!(stdout.contains("  ops"));
        assert!(stdout.contains("  logs"));
        assert!(stdout.contains("  status"));
        assert!(stdout.contains("  doctor"));
        for removed in ["core", "host"] {
            assert!(!stdout.contains(&format!("  {removed}")));
        }
    }
}

#[test]
fn diagnostics_reachability_failures_are_nonzero_and_name_the_handoffs() {
    for verb in ["status", "doctor"] {
        let home = tempfile::tempdir().expect("temporary home");
        let output = run(&[verb], Some(home.path()));

        assert!(!output.status.success());
        assert!(stdout(&output).is_empty());
        let stderr = stderr(&output);
        assert!(stderr.contains("cluster\tunreachable"));
        assert!(stderr.contains("`ployz network status`"));
        assert!(stderr.contains("re-join"));
    }
}

#[test]
fn version_is_available_without_creating_configuration() {
    let home = tempfile::tempdir().expect("temporary home");
    let output = run(&["--version"], Some(home.path()));

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).starts_with("ployz "));
    assert!(!home.path().join(".ployz").exists());
}

#[test]
fn transport_command_requires_its_typed_arguments() {
    let output = run(&["deploy"], None);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("required arguments were not provided"));
}

#[test]
fn telemetry_preference_persists_without_creating_an_identity() {
    for (verb, expected) in [("enable", true), ("disable", false)] {
        let home = tempfile::tempdir().expect("temporary home");
        let output = run(&["telemetry", verb], Some(home.path()));

        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(stdout(&output), format!("Telemetry {verb}d.\n"));
        let stored: serde_json::Value = serde_json::from_slice(
            &fs::read(home.path().join(".ployz/config")).expect("telemetry config exists"),
        )
        .expect("telemetry config is JSON");
        assert_eq!(stored.get("telemetry"), Some(&expected.into()));
        assert!(
            stored
                .get("install_id")
                .is_some_and(serde_json::Value::is_null)
        );
    }
}

fn run(args: &[&str], home: Option<&std::path::Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command.args(args);
    if let Some(home) = home {
        command.env("HOME", home);
    }
    command.output().expect("ployz binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
