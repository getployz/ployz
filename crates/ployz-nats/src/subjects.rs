use base64::Engine;
use ipnet::Ipv4Net;
use ployz_types::model::{DeployId, MachineId};
use ployz_types::spec::Namespace;

pub const DEPLOY_COMMITS_STREAM: &str = "deploy_commits";
pub const REVISIONS_STREAM: &str = "revisions";
pub const CERT_JOBS_STREAM: &str = "cert_jobs";
pub const ROUTING_EVENTS_STREAM: &str = "routing_events";

#[must_use]
pub fn deploy_commit(namespace: &Namespace, deploy_id: &DeployId) -> String {
    format!(
        "deploy_commits.{}.{}",
        subject_token(&namespace.0),
        subject_token(&deploy_id.0)
    )
}

#[must_use]
pub fn routing_event(batch_id: &str, index: usize) -> String {
    format!("routing.events.{}.{}", subject_token(batch_id), index)
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
pub fn acme_account_lock(issuer_url: &str) -> String {
    format!("locks.acme_account.{}", kv_key_token(issuer_url))
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
pub fn cert_renewal_schedule(hostname: &str) -> String {
    format!(
        "cert.jobs.schedule.{}",
        subject_token(&hostname.to_ascii_lowercase())
    )
}

#[must_use]
pub fn node_command(machine_id: &MachineId, command: &str) -> String {
    format!("node.{}.cmd.{}", subject_token(&machine_id.0), command)
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
    fn subnet_reservation_has_a_distinct_lock_key() {
        let subnet = "10.210.1.0/24".parse().expect("valid subnet");
        assert_eq!(subnet_lock(subnet), "locks.subnet.MTAuMjEwLjEuMC8yNA");
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
