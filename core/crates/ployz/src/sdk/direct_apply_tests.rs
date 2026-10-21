use crate::sdk::test_support as support;

/// The `--json` error of a direct apply that failed against `service`.
async fn failed_apply(service: support::DeployService) -> ployz_core::RpcError {
    let (mut client, server) = support::connected(service).await;
    let error =
        crate::deploy::apply_requested(&mut client, &support::spec("web"), false, false, "test")
            .await
            .expect_err("the Container never runs");
    server.abort();
    crate::failure::Failure::from(error).report()
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
        failure.details["outcome"]["failed"]["error"]["last_run"],
        serde_json::json!({ "exit_code": 1, "last_line": "token=[redacted]" })
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
