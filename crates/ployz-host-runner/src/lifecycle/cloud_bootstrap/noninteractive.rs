//! Noninteractive Cloud Bootstrap through a pre-rendered invite token.

use std::process::ExitCode;
use std::time::Duration;

use crate::cloud_client::CloudClient;
use ployz_sdk_types::{
    CloudBootstrapClientInfo, CloudBootstrapDecision, CloudBootstrapToken,
    CloudBootstrapTokenRedeemRequest,
};

pub(crate) fn run(cloud_host: &str, cloud_token: CloudBootstrapToken) -> ExitCode {
    let Some(attempt) = super::flow::prepare_cloud_bootstrap_attempt() else {
        return ExitCode::FAILURE;
    };
    let client = CloudClient::new(cloud_host);
    if let Some(exit_code) = super::flow::resume_terminal_callback(&attempt, &client) {
        return exit_code;
    }

    let request = CloudBootstrapTokenRedeemRequest {
        attempt_id: attempt.attempt_id.clone(),
        client: CloudBootstrapClientInfo::current(env!("CARGO_PKG_VERSION")),
        machine: super::flow::cloud_machine_facts(),
    };
    super::flow::run_cloud_bootstrap_decisions(
        &attempt,
        &client,
        super::flow::CloudBootstrapFlow::NoninteractiveToken,
        Duration::ZERO,
        || {
            client.post_json::<_, CloudBootstrapDecision>(
                "/api/bootstrap/tokens/redeem",
                Some(cloud_token.secret()),
                &request,
            )
        },
    )
}
