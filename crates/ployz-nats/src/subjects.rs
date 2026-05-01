use ipnet::Ipv4Net;
use ployz_types::model::{DeployId, MachineId};
use ployz_types::spec::Namespace;

pub const DEPLOY_COMMITS_STREAM: &str = "deploy_commits";
pub const REVISIONS_STREAM: &str = "revisions";
pub const CERT_JOBS_STREAM: &str = "cert_jobs";

#[must_use]
pub fn deploy_commit(namespace: &Namespace, deploy_id: &DeployId) -> String {
    format!(
        "deploy_commits.{}.{}",
        subject_token(&namespace.0),
        subject_token(&deploy_id.0)
    )
}

#[must_use]
pub fn revision(namespace: &Namespace, service: &str, revision_hash: &str) -> String {
    format!(
        "revisions.{}.{}.{}",
        subject_token(&namespace.0),
        subject_token(service),
        subject_token(revision_hash)
    )
}

#[must_use]
pub fn deploy_lock(namespace: &Namespace) -> String {
    format!("locks.deploy.{}", kv_key_token(&namespace.0))
}

#[must_use]
pub fn cert_lock(hostname: &str) -> String {
    format!(
        "locks.cert.{}",
        kv_key_token(&hostname.to_ascii_lowercase())
    )
}

#[must_use]
pub fn subnet_lock(subnet: Ipv4Net) -> String {
    format!("locks.subnet.{}", kv_key_token(&subnet.to_string()))
}

#[must_use]
pub fn cert_renewal_job(hostname: &str) -> String {
    format!(
        "cert.jobs.renew.{}",
        subject_token(&hostname.to_ascii_lowercase())
    )
}

#[must_use]
pub fn node_command(machine_id: &MachineId, command: &str) -> String {
    format!("node.{}.cmd.{}", subject_token(&machine_id.0), command)
}

fn subject_token(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}

fn kv_key_token(value: &str) -> String {
    value.replace('/', "_").replace(':', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnet_reservation_has_a_distinct_lock_key() {
        let subnet = "10.210.1.0/24".parse().expect("valid subnet");
        assert_eq!(subnet_lock(subnet), "locks.subnet.10.210.1.0_24");
    }
}
