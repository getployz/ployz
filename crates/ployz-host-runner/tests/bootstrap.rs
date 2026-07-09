mod support;

use std::fs;

use ployz_host_runner::cli::{HostRunnerCommand, load_command};
use ployz_host_runner::steps::JoinToken;
use support::bootstrap::unique_temp_path;

#[test]
fn host_runner_startup_reads_join_token_file_without_consuming_it() {
    let token_file = unique_temp_path("ployz-host-runner-join-token");
    fs::write(&token_file, "join_once\n").expect("join token file can be written");

    let command = load_command(vec![
        "--join-token-file".into(),
        token_file.as_os_str().to_os_string(),
    ])
    .expect("startup reads join token");
    let HostRunnerCommand::Start(startup) = command else {
        panic!("expected startup command, got {command:?}");
    };
    let join = startup.join.as_ref().expect("join token is loaded");

    assert_eq!(
        &join.token,
        &JoinToken::try_new("join_once").expect("expected token is valid")
    );
    assert_eq!(join.file, token_file);
    assert_eq!(format!("{:?}", join.token), "JoinToken(\"[redacted]\")");
    assert!(token_file.exists());
    fs::remove_file(token_file).expect("test token file can be removed");
}
