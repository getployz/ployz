use base64::Engine;
use ipnet::Ipv4Net;
use ployz_model::{AuthorityId, DeployId, InstallationId, MachineId, StorageParticipation};
use ployz_spec::Namespace;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsScope {
    installation: InstallationId,
    authority: AuthorityId,
}

impl NatsScope {
    #[must_use]
    pub(crate) fn new(installation: InstallationId, authority: AuthorityId) -> Self {
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
    pub fn local_for_storage_participation(participation: &StorageParticipation) -> Self {
        let authority = participation
            .authority_id()
            .cloned()
            .unwrap_or_else(AuthorityId::default_authority);
        Self::new(InstallationId::local(), authority)
    }

    #[must_use]
    pub fn installation(&self) -> &InstallationId {
        &self.installation
    }

    #[must_use]
    pub fn authority(&self) -> &AuthorityId {
        &self.authority
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
    pub(crate) fn authority_prefix(&self) -> String {
        format!(
            "ployz.v1.{}.{}",
            subject_token(self.installation.as_str()),
            subject_token(self.authority.as_str())
        )
    }

    #[must_use]
    pub(crate) fn substrate_prefix(&self) -> String {
        format!(
            "ployz.v1.{}.substrate",
            subject_token(self.installation.as_str())
        )
    }
}

#[must_use]
pub(crate) fn deploy_commit_in(
    scope: &NatsScope,
    namespace: &Namespace,
    deploy_id: &DeployId,
) -> String {
    format!(
        "{}.cp.deploy.commit.{}.{}",
        scope.authority_prefix(),
        subject_token(&namespace.as_str()),
        subject_token(&deploy_id.as_str())
    )
}

#[must_use]
pub(crate) fn deploy_commit_filter_in(scope: &NatsScope) -> String {
    format!("{}.cp.deploy.commit.>", scope.authority_prefix())
}

#[must_use]
pub(crate) fn routing_event_in(scope: &NatsScope, event_id: &str) -> String {
    format!(
        "{}.routing.event.{}",
        scope.authority_prefix(),
        subject_token(event_id),
    )
}

#[must_use]
pub(crate) fn routing_event_filter_in(scope: &NatsScope) -> String {
    format!("{}.routing.event.>", scope.authority_prefix())
}

#[must_use]
pub fn deploy_lock(namespace: &Namespace) -> String {
    format!("cp.lock.deploy.{}", kv_key_token(&namespace.as_str()))
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
pub(crate) fn cert_renewal_job_in(scope: &NatsScope, hostname: &str) -> String {
    format!(
        "{}.work.cert.renew.{}",
        scope.authority_prefix(),
        subject_token(&hostname.to_ascii_lowercase())
    )
}

#[must_use]
pub(crate) fn cert_renewal_filter_in(scope: &NatsScope) -> String {
    format!("{}.work.cert.renew.>", scope.authority_prefix())
}

#[must_use]
pub(crate) fn cert_renewal_schedule_in(scope: &NatsScope, hostname: &str) -> String {
    format!(
        "{}.work.cert.schedule.{}",
        scope.authority_prefix(),
        subject_token(&hostname.to_ascii_lowercase())
    )
}

#[must_use]
pub(crate) fn cert_renewal_schedule_filter_in(scope: &NatsScope) -> String {
    format!("{}.work.cert.schedule.>", scope.authority_prefix())
}

#[must_use]
pub(crate) fn substrate_node_command_in(
    scope: &NatsScope,
    machine_id: &MachineId,
    command: &str,
) -> String {
    format!(
        "{}.rpc.node.{}.{}",
        scope.substrate_prefix(),
        subject_token(&machine_id.as_str()),
        command
    )
}

#[must_use]
pub(crate) fn authority_node_command_in(
    scope: &NatsScope,
    machine_id: &MachineId,
    command: &str,
) -> String {
    format!(
        "{}.rpc.node.{}.{}",
        scope.authority_prefix(),
        subject_token(&machine_id.as_str()),
        command
    )
}

#[must_use]
pub(crate) fn node_command_listener_in(scope: &NatsScope, machine_id: &MachineId) -> String {
    format!(
        "ployz.v1.{}.*.rpc.node.{}.>",
        subject_token(scope.installation.as_str()),
        subject_token(&machine_id.as_str())
    )
}

#[must_use]
pub(crate) fn node_command_queue_group(machine_id: &MachineId) -> String {
    format!("ployzd-node-{}", subject_token(&machine_id.as_str()))
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
        let namespace = Namespace::new("prod");
        let deploy_id = DeployId::new("deploy-1");
        let machine_id = MachineId::new("node-1");

        assert_eq!(
            deploy_commit_in(&scope, &namespace, &deploy_id),
            "ployz.v1.local.auth-default.cp.deploy.commit.prod.deploy-1"
        );
        assert_eq!(
            routing_event_in(&scope, "deploy:deploy-1:2"),
            "ployz.v1.local.auth-default.routing.event.deploy%3Adeploy-1%3A2"
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
