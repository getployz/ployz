use crate::cluster::Call;
use crate::sdk::test_support as support;

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

const PORT_TAKEN: &str = "failed to bind host port 0.0.0.0:80/tcp: address already in use";

/// Server one, with web traffic `on` before the command.
fn server_one(on: bool) -> ployz_core::MachineObservation {
    let mut one = support::machine('a', "one");
    one.machine.accepts_ingress = on;
    one
}

/// Run `ployz server set one --accepts-ingress=true` against `service`, whose Servers list
/// the Containers created on them. Its result, and whether Server one's own record holds
/// web traffic afterwards.
async fn turn_web_traffic_on(
    service: support::DeployService,
) -> (Result<(), crate::failure::Failure>, bool) {
    let id = support::machine('a', "one").machine.id;
    set_web_traffic_on(service, id.as_str(), None).await
}

/// [`turn_web_traffic_on`] with Server one selected as `typed`, renamed to `name` in the same
/// command. The in-memory client routes only by Machine ID, so the target is the ID `typed`
/// resolved to.
async fn set_web_traffic_on(
    service: support::DeployService,
    typed: &str,
    name: Option<&str>,
) -> (Result<(), crate::failure::Failure>, bool) {
    let (mut client, server) = support::connected_in_memory(service.lists_created()).await;
    let id = support::machine('a', "one").machine.id;
    let mut args = vec!["ployz", "server", "set", typed, "--accepts-ingress=true"];
    args.extend(name.iter().flat_map(|name| ["--name", name]));
    let root = crate::cli::command().try_get_matches_from(args).unwrap();
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
            name: name.map(|name| ployz_core::MachineName::parse(name).unwrap()),
            ..Default::default()
        },
        None,
        format!("ployz server set {typed} --accepts-ingress=true"),
        recovery,
    )
    .await;
    let details = client
        .call_shared::<ployz_core::op::Inspect>(
            ployz_core::InspectRequest::default(),
            Call::once().to(&ployz_core::MachineTarget::from(&id)),
        )
        .await
        .unwrap();
    server.abort();
    (result, details.machine.unwrap().accepts_ingress)
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

/// A rename in the same command leaves the typed name selecting nothing, so `next:` selects
/// the Server by Machine ID, and `retry:` doesn't repeat it (#3449).
#[tokio::test(start_paused = true)]
async fn renamed_web_traffic_reruns_by_machine_id() {
    let id = support::machine('a', "one").machine.id;
    let rerun = [
        "ployz",
        "server",
        "set",
        id.as_str(),
        "--accepts-ingress=true",
    ];
    // Back off (the command turned it on), and left on (it was already on).
    for on_before in [false, true] {
        let (result, _) = set_web_traffic_on(
            support::DeployService::new(server_one(on_before)).fail_starts(PORT_TAKEN),
            "one",
            Some("two"),
        )
        .await;
        let report = result.unwrap_err().report();
        let next = report.details.get("next").and_then(|next| next.as_str());
        let next = next.unwrap_or_else(|| panic!("{report:?}"));
        assert_eq!(shell_words::split(next).unwrap(), rerun, "{report:?}");
        assert!(report.details.get("retry").is_none(), "{report:?}");
    }
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

/// A proxy that keeps crashing stays on its Server, where Docker keeps restarting it, so
/// web traffic stays on and the error says how to see why.
#[tokio::test(start_paused = true)]
async fn web_traffic_whose_proxy_keeps_crashing_stays_on() {
    let (result, on) =
        turn_web_traffic_on(support::DeployService::new(server_one(false)).restart_containers())
            .await;
    let error = result.unwrap_err();
    assert!(on, "web traffic stays on");
    let report = error.report();
    assert!(
        report
            .message
            .starts_with("Server updated; the Ingress Proxy did not follow"),
        "{report:?}"
    );
    assert!(report.message.contains("keeps crashing"), "{report:?}");
}

/// A create that failed for a reason other than a taken port, such as Docker refusing it,
/// proves nothing about what runs there, so web traffic stays on.
#[tokio::test(start_paused = true)]
async fn web_traffic_whose_proxy_create_failed_otherwise_stays_on() {
    let (result, on) = turn_web_traffic_on(
        support::DeployService::new(server_one(false))
            .fail_starts("invalid mount config for type \"bind\""),
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

/// Web traffic Cloud turned on just before this command, whose replicated row doesn't hold it
/// yet, stays on: the Server's own record says the command didn't turn it on.
#[tokio::test(start_paused = true)]
async fn web_traffic_turned_on_elsewhere_first_stays_on() {
    let (result, on) = turn_web_traffic_on(
        support::DeployService::new(server_one(true))
            .fail_starts(PORT_TAKEN)
            .rows_lag_ingress(1),
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

/// Another operator's Ingress Proxy that took the slot this command's removed attempt left
/// serves there, so web traffic stays on.
#[tokio::test(start_paused = true)]
async fn web_traffic_whose_failed_proxy_another_one_replaced_stays_on() {
    let (result, on) = turn_web_traffic_on(
        support::DeployService::new(server_one(false))
            .restart_containers()
            .rival_proxy(),
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

/// A taken port may be held by another operator's Ingress Proxy, which serves there, so web
/// traffic stays on.
#[tokio::test(start_paused = true)]
async fn web_traffic_whose_port_another_proxy_holds_stays_on() {
    let (result, on) = turn_web_traffic_on(
        support::DeployService::new(server_one(false))
            .fail_starts(PORT_TAKEN)
            .rival_proxy(),
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

/// A proxy that can't start on another Server says nothing about this one.
#[tokio::test(start_paused = true)]
async fn web_traffic_whose_proxy_fails_on_another_server_stays_on() {
    let mut two = support::machine('b', "two");
    two.machine.accepts_ingress = true;
    let two_id = two.machine.id;
    let (result, on) = turn_web_traffic_on(
        support::DeployService::new(server_one(false))
            .with_machines(vec![server_one(false), two])
            .fail_starts_on(two_id, PORT_TAKEN),
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
    assert!(
        report.details.get("retry").is_none(),
        "only the off command is offered: {report:?}"
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
