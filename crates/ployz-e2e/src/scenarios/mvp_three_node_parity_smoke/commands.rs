mod cluster;
mod deploy;
mod serving;

pub(crate) use cluster::start_cluster_daemons;
pub(crate) use cluster::{MvpBootstrapEvidence, MvpDaemonEvidence, bootstrap_cluster};
pub(crate) use deploy::{MvpDeployEvidence, deploy_web_api_and_echo};
pub(crate) use serving::{
    MvpAcmeHttpsEvidence, MvpContainerDnsEvidence, MvpGatewayHttpEvidence, verify_acme_https,
    verify_container_dns, verify_gateway_http,
};

pub(super) const STATE_DIR: &str = "/var/lib/ployz-mvp/node";
pub(super) const DAEMON_CONTROL: &str = "/var/lib/ployz-mvp/node/control/daemon.sock";
pub(super) const ISLAND: &str = "prod";
pub(super) const P2PANDA_PORT: u16 = 41_001;
pub(super) const DATA_PLANE_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn shell_quote_preserves_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }
}
