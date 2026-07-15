use std::process::ExitCode;

use crate::cli::{HostRunnerBootstrap, HostRunnerBootstrapMode};

use super::{cloud_bootstrap, founder_bootstrap, joiner_bootstrap};

pub(crate) fn run_bootstrap_command(bootstrap: HostRunnerBootstrap) -> ExitCode {
    match bootstrap.mode {
        HostRunnerBootstrapMode::LocalGuidance => {
            eprintln!("Use local CLI setup from your workstation:");
            eprintln!("  ployz machine init USER@HOST");
            eprintln!("Or opt in to Cloud:");
            eprintln!("  sudo ployz host bootstrap cloud");
            ExitCode::FAILURE
        }
        HostRunnerBootstrapMode::CloudInteractive { cloud_host } => {
            let host = choose_cloud_host(cloud_host);
            cloud_bootstrap::interactive::run(host.as_str())
        }
        HostRunnerBootstrapMode::CloudToken {
            cloud_host,
            cloud_token,
        } => {
            let host = choose_cloud_host(cloud_host);
            cloud_bootstrap::noninteractive::run(host.as_str(), cloud_token)
        }
        HostRunnerBootstrapMode::Core => founder_bootstrap::run_local_founder_bootstrap(),
        HostRunnerBootstrapMode::Join { join_token } => joiner_bootstrap::run(&join_token),
    }
}

fn choose_cloud_host(cloud_host: Option<crate::cli::CloudHost>) -> crate::cli::CloudHost {
    cloud_host.unwrap_or_default()
}
