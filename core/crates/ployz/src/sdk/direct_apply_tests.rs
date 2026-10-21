use crate::sdk::test_support as support;

/// The failure of a direct apply against `service`.
async fn failed(service: support::DeployService) -> crate::failure::Failure {
    let (mut client, server) = support::connected(service).await;
    let error =
        crate::deploy::apply_requested(&mut client, &support::spec("web"), false, false, "test")
            .await
            .expect_err("the Container never runs");
    server.abort();
    error.into()
}

/// The `--json` error of a direct apply that failed against `service`.
async fn failed_apply(service: support::DeployService) -> ployz_core::RpcError {
    failed(service).await.report()
}

/// A direct apply, with no Store, names a crash-looping Container's exit code and last
/// line from its Server's Log Store, in the same words as a Store-followed deploy.
#[tokio::test]
async fn a_direct_apply_whose_container_keeps_restarting_names_its_exit_and_last_line() {
    let failure = failed_apply(
        support::DeployService::new(support::machine('a', "one")).restart_containers(),
    )
    .await;
    assert_eq!(
        failure.message,
        "web: The Container exited with code 1 and is restarting."
    );
    assert_eq!(failure.cause, ["token=[redacted]"]);
    assert_eq!(
        failure.details.pointer("/outcome/failed/error/last_run"),
        Some(&serde_json::json!({ "exit_code": 1, "last_line": "token=[redacted]" }))
    );
}

#[tokio::test]
async fn a_direct_apply_whose_container_exited_names_its_last_line() {
    let failure =
        failed_apply(support::DeployService::new(support::machine('a', "one")).crash_containers())
            .await;
    assert_eq!(failure.message, "web: The Container exited with code 1.");
    assert_eq!(failure.cause, ["token=[redacted]"]);
}

#[tokio::test]
async fn a_direct_apply_whose_container_ran_out_of_memory_still_says_so() {
    let failure = failed_apply(
        support::DeployService::new(support::machine('a', "one")).oom_kill_containers(),
    )
    .await;
    assert_eq!(failure.message, "web: The Container ran out of memory.");
}

/// A Server change whose Ingress Proxy follow-up crash-loops tells a person reading
/// the CLI the exit code and the last line, as `ployz deploy` does.
#[tokio::test]
async fn an_ingress_follow_up_that_keeps_restarting_prints_its_exit_and_last_line() {
    let failure =
        failed(support::DeployService::new(support::machine('a', "one")).restart_containers())
            .await;
    let failure = crate::handlers::server::ingress_incomplete(
        "Server updated",
        failure,
        "ployz server set one --accepts-ingress=true".into(),
    );
    let text = crate::ui::plain(&failure);
    assert!(
        text.starts_with(
            "error: Server updated; the Ingress Proxy did not follow: web: The Container exited with code 1 and is restarting.\n  cause: token=[redacted]\n"
        ),
        "{text}"
    );
    assert_eq!(failure.causes(), ["token=[redacted]"]);
}

/// The last line is the one the exited run printed, not the next run's first.
#[tokio::test]
async fn a_direct_apply_whose_container_restarted_names_the_exited_runs_last_line() {
    let failure =
        failed_apply(support::DeployService::new(support::machine('a', "one")).reboot_containers())
            .await;
    assert_eq!(
        failure.message,
        "web: The Container exited with code 1 and is restarting."
    );
    assert_eq!(failure.cause, ["token=[redacted]"]);
}
