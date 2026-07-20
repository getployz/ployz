use ployz::commands::{PloyzctlCommand, parse_command};

#[test]
fn github_actions_build_is_one_closed_command() {
    assert!(matches!(
        parse_command(["build", "github-actions"].map(str::to_owned)),
        Ok(PloyzctlCommand::BuildGithubActions)
    ));
    assert!(
        parse_command(["build", "github-actions", "--pool-id", "pool"].map(str::to_owned)).is_err()
    );
}

#[test]
fn github_actions_build_has_a_stable_telemetry_name() {
    let command =
        parse_command(["build", "github-actions"].map(str::to_owned)).expect("command parses");
    assert_eq!(command.telemetry_name(), Some("build github-actions"));
}
