use ployz::build::command::{
    BuildCancelCommand, BuildExecutorCommand, BuildExecutorRunMode, BuildSubmitCommand,
};
use ployz::commands::{PloyzctlCommand, parse_command};
use ployz_core::build::BuildAdapter;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn submit_parses_exact_source_two_platforms_and_dockerfile() {
    let command = parse_command(
        [
            "build",
            "submit",
            "--git",
            "https://git.example/repo.git",
            "--commit",
            SHA,
            "--git-username",
            "builder",
            "--git-secret-env",
            "BUILD_GIT_TOKEN",
            "--subdir",
            "app",
            "--platform",
            "linux/amd64",
            "--platform",
            "linux/arm64",
            "--dockerfile",
            "app.Dockerfile",
            "--target",
            "release",
            "--operation-id",
            "op_build_fixed",
            "--detach",
        ]
        .map(str::to_owned),
    )
    .expect("build submit parses");
    let PloyzctlCommand::BuildSubmit(BuildSubmitCommand {
        operation_id,
        git,
        commit,
        git_username,
        git_secret_env,
        subdir,
        adapter,
        platforms,
        detach,
    }) = command
    else {
        panic!("expected build submit");
    };
    assert_eq!(operation_id.as_str(), "op_build_fixed");
    assert_eq!(git.as_str(), "https://git.example/repo.git");
    assert_eq!(commit.as_str(), SHA);
    assert_eq!(git_username.as_str(), "builder");
    assert_eq!(git_secret_env, "BUILD_GIT_TOKEN");
    assert_eq!(subdir.expect("subdir").as_str(), "app");
    assert_eq!(platforms.iter().count(), 2);
    assert!(detach);
    let BuildAdapter::Dockerfile { dockerfile, target } = adapter else {
        panic!("expected Dockerfile adapter");
    };
    assert_eq!(dockerfile.as_str(), "app.Dockerfile");
    assert_eq!(target.expect("target").as_str(), "release");
}

#[test]
fn submit_generates_narrow_id_and_parses_railpack() {
    let command = parse_command(base_railpack_args()).expect("build submit parses");
    let PloyzctlCommand::BuildSubmit(command) = command else {
        panic!("expected build submit");
    };
    assert!(command.operation_id.as_str().starts_with("op_build_"));
    assert!(matches!(command.adapter, BuildAdapter::Railpack { .. }));
}

#[test]
fn adapter_and_source_validation_rejects_ambiguous_or_unsafe_inputs() {
    let cases = [
        Vec::new(),
        vec![
            "--railpack",
            "--cache-scope",
            "scope",
            "--dockerfile",
            "Dockerfile",
        ],
    ];
    for adapter in cases {
        let mut args = common_args();
        args.extend(adapter.into_iter().map(str::to_owned));
        assert!(parse_command(args).is_err());
    }

    let mut args = base_railpack_args();
    let secret_index = args
        .iter()
        .position(|arg| arg == "BUILD_GIT_TOKEN")
        .expect("secret env argument");
    *args
        .get_mut(secret_index)
        .expect("secret env argument exists") = "SECRET=value".to_owned();
    assert!(parse_command(args).is_err());

    for (flag, invalid) in [("--commit", "not-a-sha"), ("--subdir", "../outside")] {
        let mut args = base_railpack_args();
        args.extend([flag.to_owned(), invalid.to_owned()]);
        assert!(parse_command(args).is_err(), "{flag} must reject {invalid}");
    }

    let mut duplicate = base_railpack_args();
    duplicate.extend(["--platform", "linux/x86_64"].map(str::to_owned));
    assert!(parse_command(duplicate).is_err());
}

#[test]
fn cancel_defaults_reason_and_accepts_override() {
    let command = parse_command(["build", "cancel", "op_build_one"].map(str::to_owned))
        .expect("cancel parses");
    let PloyzctlCommand::BuildCancel(BuildCancelCommand {
        operation_id,
        reason,
    }) = command
    else {
        panic!("expected cancel");
    };
    assert_eq!(operation_id.as_str(), "op_build_one");
    assert_eq!(reason.as_str(), "operator requested build cancellation");

    let command = parse_command(
        [
            "build",
            "cancel",
            "op_build_one",
            "--reason",
            "no longer needed",
        ]
        .map(str::to_owned),
    )
    .expect("cancel reason parses");
    let PloyzctlCommand::BuildCancel(command) = command else {
        panic!("expected cancel");
    };
    assert_eq!(command.reason.as_str(), "no longer needed");
}

#[test]
fn external_executor_once_and_watch_parse_exact_identity_and_lifecycle() {
    let command = parse_command(
        [
            "build",
            "once",
            "--pool-id",
            "pool_ci",
            "--executor-id",
            "executor_1",
            "--workspace-root",
            "/tmp/ployz-builds",
            "--wait-timeout-seconds",
            "47",
        ]
        .map(str::to_owned),
    )
    .expect("build once parses");
    let PloyzctlCommand::BuildExecutor(BuildExecutorCommand {
        pool_id,
        executor_id,
        workspace_root,
        mode: BuildExecutorRunMode::Once { wait_timeout },
    }) = command
    else {
        panic!("expected external Build Executor");
    };
    assert_eq!(pool_id.as_str(), "pool_ci");
    assert_eq!(executor_id.as_str(), "executor_1");
    assert_eq!(
        workspace_root.expect("workspace"),
        std::path::Path::new("/tmp/ployz-builds")
    );
    assert_eq!(wait_timeout, std::time::Duration::from_secs(47));

    let command = parse_command(
        [
            "build",
            "watch",
            "--pool-id",
            "pool_ci",
            "--executor-id",
            "executor_1",
        ]
        .map(str::to_owned),
    )
    .expect("build watch parses");
    assert!(matches!(
        command,
        PloyzctlCommand::BuildExecutor(BuildExecutorCommand {
            mode: BuildExecutorRunMode::Watch,
            ..
        })
    ));

    assert!(
        parse_command(
            [
                "build",
                "once",
                "--pool-id",
                "pool_ci",
                "--executor-id",
                "executor_1",
                "--wait-timeout-seconds",
                "0",
            ]
            .map(str::to_owned),
        )
        .is_err()
    );
}

fn common_args() -> Vec<String> {
    [
        "build",
        "submit",
        "--git",
        "https://git.example/repo.git",
        "--commit",
        SHA,
        "--git-username",
        "builder",
        "--git-secret-env",
        "BUILD_GIT_TOKEN",
        "--platform",
        "linux/amd64",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn base_railpack_args() -> Vec<String> {
    let mut args = common_args();
    args.extend(["--railpack", "--cache-scope", "project-cache"].map(str::to_owned));
    args
}
