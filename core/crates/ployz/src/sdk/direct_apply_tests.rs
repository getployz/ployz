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

/// `ployz server add` founding a Cluster without Cloud, whose Ingress Proxy crash-loops,
/// tells a person the exit code and the last line.
#[tokio::test]
async fn a_founding_server_add_whose_ingress_keeps_restarting_prints_its_exit_and_last_line() {
    let failure =
        failed(support::DeployService::new(support::machine('a', "one")).restart_containers())
            .await;
    let failure = crate::handlers::server::init::ingress_incomplete(
        failure,
        "ployz server set one --accepts-ingress=true".into(),
    );
    let text = crate::ui::plain(&failure);
    assert!(
        text.starts_with(
            "error: Server initialized; ingress deployment incomplete: web: The Container exited with code 1 and is restarting.\n  cause: token=[redacted]\n"
        ),
        "{text}"
    );
    assert_eq!(failure.causes(), ["token=[redacted]"]);
}

/// Cloud enrollment behind `ployz server add` whose Ingress Proxy crash-loops tells a
/// person the exit code and the last line.
#[tokio::test]
async fn a_cloud_enrollment_whose_ingress_keeps_restarting_prints_its_exit_and_last_line() {
    let failure =
        failed(support::DeployService::new(support::machine('a', "one")).restart_containers())
            .await;
    let failure = crate::handlers::cloud::ingress_incomplete(
        crate::cloud_enroll::Command::ServerAdd,
        failure,
    );
    let text = crate::ui::plain(&failure);
    assert!(
        text.starts_with(
            "error: Server initialized; Ingress deployment incomplete: web: The Container exited with code 1 and is restarting. Rerun the same ployz server add command without --reset, keeping all other options, to reconcile it.\n  cause: token=[redacted]\n"
        ),
        "{text}"
    );
}

/// A failure with no deploy evidence keeps each path's headline as it was.
#[test]
fn an_ingress_failure_without_deploy_evidence_keeps_its_headline() {
    let failure = || crate::failure::Failure::unavailable("down");
    let init = crate::handlers::server::init::ingress_incomplete(failure(), String::new());
    assert_eq!(
        init.report().message,
        "Server initialized; ingress deployment incomplete."
    );
    let cloud = crate::handlers::cloud::ingress_incomplete(
        crate::cloud_enroll::Command::ServerAdd,
        failure(),
    );
    assert_eq!(
        cloud.report().message,
        "Server initialized; Ingress deployment incomplete. Rerun the same ployz server add command without --reset, keeping all other options, to reconcile it."
    );
    let enrolled = crate::handlers::cloud::ingress_incomplete(
        crate::cloud_enroll::Command::ServerEnroll,
        failure(),
    );
    assert_eq!(
        enrolled.report().message,
        "Server initialized; Ingress deployment incomplete. Run ployz server enroll again, or use Enroll in Servers > Waiting to join, to reconcile it."
    );
    let set =
        crate::handlers::server::ingress_incomplete("Server updated", failure(), String::new());
    assert_eq!(
        set.report().message,
        "Server updated; the Ingress Proxy did not follow."
    );
}

/// A run that exited printing nothing has no last line, so no earlier run's line
/// stands in for it, on the headline or under it.
#[tokio::test]
async fn a_direct_apply_whose_last_run_printed_nothing_names_no_line() {
    let failure = failed(
        support::DeployService::new(support::machine('a', "one"))
            .reboot_containers_through_a_silent_run(),
    )
    .await;
    let report = failure.report();
    assert_eq!(
        report.message,
        "web: The Container exited with code 1 and is restarting."
    );
    assert_eq!(report.cause, Vec::<String>::new());
    let text = crate::ui::plain(&crate::handlers::server::ingress_incomplete(
        "Server updated",
        failure,
        "ployz server set one --accepts-ingress=true".into(),
    ));
    assert!(
        text.starts_with(
            "error: Server updated; the Ingress Proxy did not follow: web: The Container exited with code 1 and is restarting.\n"
        ),
        "{text}"
    );
    assert!(!text.contains("cause:"), "{text}");
}

/// Cloud enrollment publishes past an Ingress Proxy Apply that ran but left a Server
/// unobserved, and stops before publication on one that failed.
#[tokio::test]
async fn an_apply_that_leaves_a_server_unobserved_is_partial_and_a_failed_one_is_not() {
    let mut down = support::machine('b', "two");
    down.membership = ployz_core::MembershipObservation::Down;
    down.membership_evidence = Some(ployz_core::MembershipEvidence::Down);
    let (mut client, server) = support::connected(
        support::DeployService::empty().with_machines(vec![support::machine('a', "one"), down]),
    )
    .await;
    let partial =
        crate::deploy::apply_requested(&mut client, &support::spec("web"), false, false, "test")
            .await
            .expect_err("Server two goes unobserved");
    server.abort();
    assert!(
        matches!(partial, crate::deploy::ApplyError::Partial(_)),
        "{:?}",
        crate::failure::Failure::from(partial).report()
    );

    let (mut client, server) = support::connected(
        support::DeployService::new(support::machine('a', "one")).restart_containers(),
    )
    .await;
    let failed =
        crate::deploy::apply_requested(&mut client, &support::spec("web"), false, false, "test")
            .await
            .expect_err("the Container never runs");
    server.abort();
    assert!(matches!(failed, crate::deploy::ApplyError::Failed(_)));
}

const PORT_TAKEN: &str = "failed to bind host port 0.0.0.0:80/tcp: address already in use";

/// Server one, with web traffic `on` before the command.
fn server_one(on: bool) -> ployz_core::MachineObservation {
    let mut one = support::machine('a', "one");
    one.machine.accepts_ingress = on;
    one
}

/// Run `ployz server set one --accepts-ingress=true` against `service`. Its result, and
/// whether web traffic is on for Server one afterwards.
async fn turn_web_traffic_on(
    service: support::DeployService,
) -> (Result<(), crate::failure::Failure>, bool) {
    let (mut client, server) = support::connected_in_memory(service).await;
    let id = support::machine('a', "one").machine.id;
    let root = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "server",
            "set",
            id.as_str(),
            "--accepts-ingress=true",
        ])
        .unwrap();
    let recovery = root
        .subcommand_matches("server")
        .and_then(|server| server.subcommand_matches("set"))
        .unwrap()
        .clone();
    let result = crate::handlers::server::apply_update(
        &mut client,
        ployz_core::MachineTarget::from(&id),
        ployz_core::MachineUpdate {
            accepts_ingress: Some(true),
            ..Default::default()
        },
        None,
        crate::ingress::IngressImage::Given("caddy:2".into()),
        format!("ployz server set {id} --accepts-ingress=true"),
        recovery,
    )
    .await;
    let machines = client.machines().await.unwrap();
    server.abort();
    let on = machines
        .iter()
        .find(|observed| observed.machine.id == id)
        .unwrap()
        .machine
        .accepts_ingress;
    (result, on)
}

/// Web traffic the command turned on goes back off when its Ingress Proxy is seen failing
/// to start there, as the dashboard switch does, and the error says so with the real cause.
#[tokio::test(start_paused = true)]
async fn web_traffic_whose_proxy_cannot_take_its_port_goes_back_off() {
    let (result, on) =
        turn_web_traffic_on(support::DeployService::new(server_one(false)).fail_starts(PORT_TAKEN))
            .await;
    let error = result.unwrap_err();
    assert!(!on, "web traffic went back off");
    let report = error.report();
    assert_eq!(
        report.message,
        "Web traffic stays off on one: port 80 or 443 is in use. Free it, then try again."
    );
    assert!(
        report.cause.iter().any(|cause| cause.contains(PORT_TAKEN)),
        "{report:?}"
    );
    assert!(report.details.get("retry").is_some(), "{report:?}");
}

/// Any other failed start says what failed after the same words.
#[tokio::test(start_paused = true)]
async fn web_traffic_whose_proxy_keeps_restarting_goes_back_off() {
    let (result, on) =
        turn_web_traffic_on(support::DeployService::new(server_one(false)).restart_containers())
            .await;
    let error = result.unwrap_err();
    assert!(!on, "web traffic went back off");
    assert!(
        error
            .report()
            .message
            .starts_with("Web traffic stays off on one; its Ingress Proxy didn't start: "),
        "{:?}",
        error.report()
    );
}

/// A start whose outcome the CLI never saw, here because the Server's Containers couldn't be
/// listed, leaves web traffic on and says the change landed.
#[tokio::test(start_paused = true)]
async fn web_traffic_whose_proxy_start_went_unseen_stays_on() {
    let one = server_one(false);
    let id = one.machine.id;
    let (result, on) =
        turn_web_traffic_on(support::DeployService::new(one).fail_listing_on(id)).await;
    let error = result.unwrap_err();
    assert!(on, "web traffic stays on");
    assert!(
        error
            .report()
            .message
            .starts_with("Server updated; the Ingress Proxy did not follow"),
        "{:?}",
        error.report()
    );
}

/// A start whose answer was lost, as when the Server stops answering, may have run, so web
/// traffic stays on.
#[tokio::test(start_paused = true)]
async fn web_traffic_whose_proxy_start_went_unanswered_stays_on() {
    let (result, on) = turn_web_traffic_on(
        support::DeployService::new(server_one(false)).fail_starts("transport error"),
    )
    .await;
    let error = result.unwrap_err();
    assert!(on, "web traffic stays on");
    assert!(
        error
            .report()
            .message
            .starts_with("Server updated; the Ingress Proxy did not follow"),
        "{:?}",
        error.report()
    );
}

/// Web traffic that was already on before the command stays on: the command didn't turn it on.
#[tokio::test(start_paused = true)]
async fn web_traffic_already_on_stays_on_when_its_proxy_cannot_start() {
    let (result, on) =
        turn_web_traffic_on(support::DeployService::new(server_one(true)).fail_starts(PORT_TAKEN))
            .await;
    let error = result.unwrap_err();
    assert!(on, "web traffic stays on");
    assert!(
        error
            .report()
            .message
            .starts_with("Server updated; the Ingress Proxy did not follow"),
        "{:?}",
        error.report()
    );
}

/// When turning web traffic back off fails too, the error says both, and how to turn it off.
#[tokio::test(start_paused = true)]
async fn web_traffic_that_cannot_go_back_off_says_both_failures() {
    let (result, on) = turn_web_traffic_on(
        support::DeployService::new(server_one(false))
            .fail_starts(PORT_TAKEN)
            .refuse_role_off("the Server's record is busy"),
    )
    .await;
    let error = result.unwrap_err();
    assert!(on, "web traffic is still on");
    let report = error.report();
    assert!(
        report.message.starts_with(
            "The Ingress Proxy didn't start on one, and turning web traffic back off failed, so it may still be on: "
        ),
        "{report:?}"
    );
    assert!(
        report.cause.iter().any(|cause| cause.contains(PORT_TAKEN)),
        "{report:?}"
    );
    assert_eq!(
        report.details.pointer("/rollback_error/message"),
        Some(&serde_json::json!("the Server's record is busy"))
    );
    let id = support::machine('a', "one").machine.id;
    let next = report
        .details
        .get("next")
        .and_then(|next| next.as_str())
        .unwrap();
    assert_eq!(
        shell_words::split(next).unwrap(),
        [
            "ployz",
            "server",
            "set",
            id.as_str(),
            "--accepts-ingress=false"
        ]
    );
}

/// A start that works leaves web traffic on, as before.
#[tokio::test(start_paused = true)]
async fn web_traffic_whose_proxy_starts_stays_on() {
    let (result, on) = turn_web_traffic_on(support::DeployService::new(server_one(false))).await;
    result.expect("the Ingress Proxy starts");
    assert!(on, "web traffic stays on");
}
