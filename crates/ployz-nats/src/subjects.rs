use base64::Engine;
use ipnet::Ipv4Net;
use ployz_types::model::{AuthorityId, DeployId, InstallationId, MachineId};
use ployz_types::spec::Namespace;

pub const DEPLOY_COMMITS_STREAM: &str = "cp_deploy_commits_auth-default";
pub const REVISIONS_STREAM: &str = "cp_revisions_auth-default";
pub const CERT_JOBS_STREAM: &str = "work_cert_auth-default";
pub const ROUTE_JOURNAL_STREAM: &str = "route_journal_auth-default";
pub const ROUTING_EVENTS_STREAM: &str = ROUTE_JOURNAL_STREAM;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsScope {
    pub installation: InstallationId,
    pub authority: AuthorityId,
}

impl NatsScope {
    #[must_use]
    pub fn new(installation: InstallationId, authority: AuthorityId) -> Self {
        Self {
            installation,
            authority,
        }
    }

    #[must_use]
    pub fn local_default() -> Self {
        Self {
            installation: InstallationId::local(),
            authority: AuthorityId::default_authority(),
        }
    }

    #[must_use]
    pub fn authority_domain(&self) -> String {
        format!("dom-{}", subject_token(self.authority.as_str()))
    }

    #[must_use]
    pub fn root_domain(&self) -> String {
        format!("dom-{}-root", subject_token(self.installation.as_str()))
    }

    #[must_use]
    pub fn authority_prefix(&self) -> String {
        format!(
            "ployz.v1.{}.{}",
            subject_token(self.installation.as_str()),
            subject_token(self.authority.as_str())
        )
    }

    #[must_use]
    pub fn substrate_prefix(&self) -> String {
        format!(
            "ployz.v1.{}.substrate",
            subject_token(self.installation.as_str())
        )
    }
}

impl Default for NatsScope {
    fn default() -> Self {
        Self::local_default()
    }
}

#[must_use]
pub fn deploy_commit(namespace: &Namespace, deploy_id: &DeployId) -> String {
    deploy_commit_in(&NatsScope::default(), namespace, deploy_id)
}

#[must_use]
pub fn deploy_commit_in(scope: &NatsScope, namespace: &Namespace, deploy_id: &DeployId) -> String {
    format!(
        "{}.cp.deploy.commit.{}.{}",
        scope.authority_prefix(),
        subject_token(&namespace.0),
        subject_token(&deploy_id.0)
    )
}

#[must_use]
pub fn route_journal_event(batch_id: &str, index: usize) -> String {
    route_journal_event_in(&NatsScope::default(), batch_id, index)
}

#[must_use]
pub fn route_journal_event_in(scope: &NatsScope, batch_id: &str, index: usize) -> String {
    format!(
        "{}.route.journal.event.default.{}.{}",
        scope.authority_prefix(),
        subject_token(batch_id),
        index
    )
}

#[must_use]
pub fn routing_event(batch_id: &str, index: usize) -> String {
    route_journal_event(batch_id, index)
}

#[must_use]
pub fn revision(namespace: &Namespace, service: &str, revision_hash: &str) -> String {
    revision_in(&NatsScope::default(), namespace, service, revision_hash)
}

#[must_use]
pub fn revision_in(
    scope: &NatsScope,
    namespace: &Namespace,
    service: &str,
    revision_hash: &str,
) -> String {
    format!(
        "{}.cp.revision.{}.{}.{}",
        scope.authority_prefix(),
        subject_token(&namespace.0),
        subject_token(service),
        subject_token(revision_hash)
    )
}

#[must_use]
pub fn deploy_lock(namespace: &Namespace) -> String {
    format!("cp.lock.deploy.{}", kv_key_token(&namespace.0))
}

#[must_use]
pub fn cert_lock(hostname: &str) -> String {
    format!(
        "cp.lock.cert.{}",
        kv_key_token(&hostname.to_ascii_lowercase())
    )
}

#[must_use]
pub fn acme_account_lock(issuer_url: &str) -> String {
    format!("cp.lock.acme_account.{}", kv_key_token(issuer_url))
}

#[must_use]
pub fn subnet_lock(subnet: Ipv4Net) -> String {
    format!("cp.lock.subnet.{}", kv_key_token(&subnet.to_string()))
}

#[must_use]
pub fn cert_renewal_job(hostname: &str) -> String {
    cert_renewal_job_in(&NatsScope::default(), hostname)
}

#[must_use]
pub fn cert_renewal_job_in(scope: &NatsScope, hostname: &str) -> String {
    format!(
        "{}.work.cert.renew.{}",
        scope.authority_prefix(),
        subject_token(&hostname.to_ascii_lowercase())
    )
}

#[must_use]
pub fn cert_renewal_schedule(hostname: &str) -> String {
    cert_renewal_schedule_in(&NatsScope::default(), hostname)
}

#[must_use]
pub fn cert_renewal_schedule_in(scope: &NatsScope, hostname: &str) -> String {
    format!(
        "{}.work.cert.schedule.{}",
        scope.authority_prefix(),
        subject_token(&hostname.to_ascii_lowercase())
    )
}

#[must_use]
pub fn substrate_node_command(machine_id: &MachineId, command: &str) -> String {
    substrate_node_command_in(&NatsScope::default(), machine_id, command)
}

#[must_use]
pub fn substrate_node_command_in(
    scope: &NatsScope,
    machine_id: &MachineId,
    command: &str,
) -> String {
    format!(
        "{}.rpc.node.{}.{}",
        scope.substrate_prefix(),
        subject_token(&machine_id.0),
        command
    )
}

#[must_use]
pub fn authority_node_command(machine_id: &MachineId, command: &str) -> String {
    authority_node_command_in(&NatsScope::default(), machine_id, command)
}

#[must_use]
pub fn authority_node_command_in(
    scope: &NatsScope,
    machine_id: &MachineId,
    command: &str,
) -> String {
    format!(
        "{}.rpc.node.{}.{}",
        scope.authority_prefix(),
        subject_token(&machine_id.0),
        command
    )
}

#[must_use]
pub fn node_command(machine_id: &MachineId, command: &str) -> String {
    authority_node_command(machine_id, command)
}

#[must_use]
pub fn node_command_listener(machine_id: &MachineId) -> String {
    node_command_listener_in(&NatsScope::default(), machine_id)
}

#[must_use]
pub fn node_command_listener_in(scope: &NatsScope, machine_id: &MachineId) -> String {
    format!(
        "ployz.v1.{}.*.rpc.node.{}.>",
        subject_token(scope.installation.as_str()),
        subject_token(&machine_id.0)
    )
}

#[must_use]
pub fn node_command_queue_group(machine_id: &MachineId) -> String {
    format!("ployzd-node-{}", subject_token(&machine_id.0))
}

pub(crate) fn subject_token(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => out.push(byte as char),
            _ => {
                out.push('%');
                out.push(hex_digit(byte >> 4));
                out.push(hex_digit(byte & 0x0f));
            }
        }
    }
    out
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'A' + (value - 10)) as char,
        _ => unreachable!("nibble is always <= 15"),
    }
}

pub(crate) fn kv_key_token(value: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_subjects_follow_future_hierarchy() {
        let scope = NatsScope::local_default();
        let namespace = Namespace("prod".into());
        let deploy_id = DeployId("deploy-1".into());
        let machine_id = MachineId("node-1".into());

        assert_eq!(
            deploy_commit_in(&scope, &namespace, &deploy_id),
            "ployz.v1.local.auth-default.cp.deploy.commit.prod.deploy-1"
        );
        assert_eq!(
            revision_in(&scope, &namespace, "api", "rev.1"),
            "ployz.v1.local.auth-default.cp.revision.prod.api.rev%2E1"
        );
        assert_eq!(
            route_journal_event_in(&scope, "batch.1", 2),
            "ployz.v1.local.auth-default.route.journal.event.default.batch%2E1.2"
        );
        assert_eq!(
            cert_renewal_job_in(&scope, "API.Example.COM"),
            "ployz.v1.local.auth-default.work.cert.renew.api%2Eexample%2Ecom"
        );
        assert_eq!(
            substrate_node_command_in(&scope, &machine_id, "status"),
            "ployz.v1.local.substrate.rpc.node.node-1.status"
        );
        assert_eq!(
            authority_node_command_in(&scope, &machine_id, "deploy.start_candidate"),
            "ployz.v1.local.auth-default.rpc.node.node-1.deploy.start_candidate"
        );
        assert_eq!(
            node_command_listener_in(&scope, &machine_id),
            "ployz.v1.local.*.rpc.node.node-1.>"
        );
    }

    #[test]
    fn subnet_reservation_has_a_distinct_lock_key() {
        let subnet = "10.210.1.0/24".parse().expect("valid subnet");
        assert_eq!(subnet_lock(subnet), "cp.lock.subnet.MTAuMjEwLjEuMC8yNA");
    }

    #[test]
    fn subject_tokens_do_not_collapse_punctuation() {
        assert_ne!(subject_token("foo.bar"), subject_token("foo_bar"));
        assert_eq!(subject_token("foo.bar"), "foo%2Ebar");
    }

    #[test]
    fn kv_key_tokens_are_collision_resistant_and_valid() {
        let url_key = kv_key_token("https://pebble:14000/dir");
        assert_ne!(url_key, kv_key_token("https___pebble_14000_dir"));
        assert!(url_key.bytes().all(|byte| {
            matches!(
                byte,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'
            )
        }));
    }
}
