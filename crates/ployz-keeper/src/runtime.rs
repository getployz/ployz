use std::time::Duration;

use ployz_core::ops::FailureMessage;
use ployz_keeper::executor::KeeperPlanFailure;

pub(crate) const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
pub(crate) const PLOYZ_NATS_CA_FILE_ENV: &str = "PLOYZ_NATS_CA_FILE";
pub(crate) const PLOYZ_JOIN_NKEY_SEED_ENV: &str = "PLOYZ_JOIN_NKEY_SEED";
pub(crate) const PLOYZ_MACHINE_PUBLIC_IP_ENV: &str = "PLOYZ_MACHINE_PUBLIC_IP";
pub(crate) const PLOYZ_MACHINE_ID_ENV: &str = "PLOYZ_MACHINE_ID";
pub(crate) const PLOYZ_GATEWAY_ENV: &str = "PLOYZ_GATEWAY";
pub(crate) const PLOYZ_DNS_ENV: &str = "PLOYZ_DNS";
pub(crate) const PLOYZ_MACHINE_BOOTSTRAP_URL_ENV: &str = "PLOYZ_MACHINE_BOOTSTRAP_URL";
pub(crate) const PLOYZ_MACHINE_JOIN_CLUSTER_NAME_ENV: &str = "PLOYZ_MACHINE_JOIN_CLUSTER_NAME";
pub(crate) const PLOYZ_MACHINE_JOIN_NATS_URL_ENV: &str = "PLOYZ_MACHINE_JOIN_NATS_URL";
pub(crate) const PLOYZ_RELEASE_MANIFEST_URL_ENV: &str = "PLOYZ_RELEASE_MANIFEST_URL";
pub(crate) const DEFAULT_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounded redeem retry while the core mints this machine's credential:
/// the join token TTL is 600 seconds, so stop well within it.
pub(crate) const REDEEM_MATERIAL_ATTEMPTS: u32 = 150;
pub(crate) const REDEEM_MATERIAL_RETRY_DELAY: Duration = Duration::from_secs(2);
pub(crate) const KEEPER_STATE_DIR: &str = "/var/lib/ployz";
pub(crate) const SUBSTRATE_VERSION_FILE: &str = "substrate-version.json";
pub(crate) const FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN: &str =
    "ployz-first-machine-bootstrap-result begin";
pub(crate) const FIRST_MACHINE_BOOTSTRAP_RESULT_END: &str =
    "ployz-first-machine-bootstrap-result end";
pub(crate) const CORE_PROMOTE_RESULT_BEGIN: &str = "ployz-core-promote-result begin";
pub(crate) const CORE_PROMOTE_RESULT_END: &str = "ployz-core-promote-result end";
pub(crate) const CLOUD_BOOTSTRAP_MAX_POLLS: u16 = 900;

pub(crate) fn failure_summary(failure: &KeeperPlanFailure) -> &str {
    match failure {
        KeeperPlanFailure::Step(step) => step.message.as_str(),
        KeeperPlanFailure::Record(record) => record.message.as_str(),
    }
}

pub(crate) fn failure_message(message: &str) -> FailureMessage {
    FailureMessage::try_new(message).expect("generated keeper failure message is non-empty")
}
