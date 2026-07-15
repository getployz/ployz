//! Interactive Cloud Bootstrap through a browser-approved session.

use std::process::ExitCode;
use std::time::Duration;

use crate::cloud_client::CloudClient;
use ployz_sdk_types::{
    CloudBootstrapClientInfo, CloudBootstrapDecision, CloudBootstrapSessionCreateRequest,
    CloudBootstrapSessionCreated, CloudBootstrapSessionPollRequest,
};

pub(crate) fn run(cloud_host: &str) -> ExitCode {
    let Some(attempt) = super::flow::prepare_cloud_bootstrap_attempt() else {
        return ExitCode::FAILURE;
    };
    let client = CloudClient::new(cloud_host);
    if let Some(exit_code) = super::flow::resume_terminal_callback(&attempt, &client) {
        return exit_code;
    }

    let machine = super::flow::cloud_machine_facts();
    let created = match client.post_json::<_, CloudBootstrapSessionCreated>(
        "/api/bootstrap/sessions",
        None,
        &CloudBootstrapSessionCreateRequest {
            attempt_id: attempt.attempt_id.clone(),
            client: CloudBootstrapClientInfo::current(env!("CARGO_PKG_VERSION")),
            machine: machine.clone(),
        },
    ) {
        Ok(created) => created,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    println!("Open this link to connect this machine:");
    println!("{}", created.browser_url);
    println!("Waiting for approval...");

    let poll_request = CloudBootstrapSessionPollRequest {
        attempt_id: attempt.attempt_id.clone(),
        session_secret: created.session_secret,
        machine,
    };
    super::flow::run_cloud_bootstrap_decisions(
        &attempt,
        &client,
        super::flow::CloudBootstrapFlow::InteractiveSession,
        Duration::from_secs(created.poll_after_seconds.into()),
        || {
            client.post_json::<_, CloudBootstrapDecision>(
                "/api/bootstrap/sessions/poll",
                None,
                &poll_request,
            )
        },
    )
}
