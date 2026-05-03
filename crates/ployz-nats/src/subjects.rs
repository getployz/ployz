use base64::Engine;
use ipnet::Ipv4Net;
use ployz_types::model::{DeployId, InstanceId, MachineId};
use ployz_types::spec::Namespace;

pub const DEPLOY_COMMITS_STREAM: &str = "deploy_commits";
pub const REVISIONS_STREAM: &str = "revisions";
pub const CERT_JOBS_STREAM: &str = "cert_jobs";
pub const ROUTING_EVENTS_STREAM: &str = "routing_events";

/// Local view stream that aggregates per-machine `state_machine_<id>`
/// streams via JetStream `sources`. Lives in each node's `local_jetstream`
/// (hub on StorageCandidate, leaf on Mirror). Subscribers read from this
/// stream to materialize the full machines view locally.
pub const MACHINE_STATE_VIEW_STREAM: &str = "machine_state_view";

/// Local view stream that aggregates per-machine `state_instances_<id>`
/// streams. Same shape as `MACHINE_STATE_VIEW_STREAM`.
pub const INSTANCE_STATE_VIEW_STREAM: &str = "instance_state_view";

/// Hub-domain KV holding the catalog of per-machine state stream names.
/// Sole writer is the coordinator handling `handle_machine_add` /
/// `handle_machine_remove`. Mirrored to leaves via the standard durable
/// KV mirror plumbing in `buckets.rs`.
pub const MACHINE_DIRECTORY_BUCKET: &str = "machine_directory";

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

/// Per-machine stream name carrying the machine's own `MachineMembership`
/// record. Hub-owned. The owning machine is the only writer; every other
/// node sources this stream into its local `MACHINE_STATE_VIEW_STREAM`.
#[must_use]
pub fn machine_state_stream(machine_id: &MachineId) -> String {
    format!("state_machine_{}", subject_token(&machine_id.0))
}

#[must_use]
pub fn machine_state_subject(machine_id: &MachineId) -> String {
    format!("state.{}.machine", subject_token(&machine_id.0))
}

#[must_use]
pub fn machine_tombstone_subject(machine_id: &MachineId) -> String {
    format!("state.{}.machine_tombstone", subject_token(&machine_id.0))
}

/// Per-machine stream name carrying the instances hosted by this machine.
/// Hub-owned. Same single-writer invariant as `machine_state_stream`.
#[must_use]
pub fn instance_state_stream(machine_id: &MachineId) -> String {
    format!("state_instances_{}", subject_token(&machine_id.0))
}

#[must_use]
pub fn instance_state_subject(
    machine_id: &MachineId,
    namespace: &Namespace,
    instance_id: &InstanceId,
) -> String {
    format!(
        "state.{}.instance.{}.{}",
        subject_token(&machine_id.0),
        subject_token(&namespace.0),
        subject_token(&instance_id.0)
    )
}

#[must_use]
pub fn instance_tombstone_subject(
    machine_id: &MachineId,
    namespace: &Namespace,
    instance_id: &InstanceId,
) -> String {
    format!(
        "state.{}.instance_tombstone.{}.{}",
        subject_token(&machine_id.0),
        subject_token(&namespace.0),
        subject_token(&instance_id.0)
    )
}

/// Subjects for one machine's state streams. The two streams' subject sets
/// don't overlap.
#[must_use]
pub fn machine_state_stream_subjects(machine_id: &MachineId) -> Vec<String> {
    let token = subject_token(&machine_id.0);
    vec![
        format!("state.{token}.machine"),
        format!("state.{token}.machine_tombstone"),
    ]
}

#[must_use]
pub fn instance_state_stream_subjects(machine_id: &MachineId) -> Vec<String> {
    let token = subject_token(&machine_id.0);
    vec![
        format!("state.{token}.instance.>"),
        format!("state.{token}.instance_tombstone.>"),
    ]
}

/// Filter patterns for `instance_state_view` consumers that only care
/// about a single namespace. Returns BOTH the live and tombstone
/// patterns so deletes for that namespace also reach the consumer —
/// without the tombstone half, removed instances would linger in the
/// filtered snapshot until a later upsert overwrote the live subject.
#[must_use]
pub fn instance_state_namespace_filters(namespace: &Namespace) -> Vec<String> {
    let token = subject_token(&namespace.0);
    vec![
        format!("state.*.instance.{token}.>"),
        format!("state.*.instance_tombstone.{token}.>"),
    ]
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
    fn machine_state_subjects_are_distinct_from_tombstone() {
        let id = MachineId("m1".into());
        assert_ne!(machine_state_subject(&id), machine_tombstone_subject(&id));
    }

    #[test]
    fn machine_state_stream_subjects_match_state_and_tombstone() {
        let id = MachineId("m1".into());
        let subjects = machine_state_stream_subjects(&id);
        assert!(subjects.contains(&"state.m1.machine".to_string()));
        assert!(subjects.contains(&"state.m1.machine_tombstone".to_string()));
    }

    #[test]
    fn instance_state_subject_encodes_namespace_for_server_side_filter() {
        let machine = MachineId("m1".into());
        let namespace = Namespace("ns_alpha".into());
        let instance = InstanceId("inst-1".into());
        let subject = instance_state_subject(&machine, &namespace, &instance);
        assert_eq!(subject, "state.m1.instance.ns_alpha.inst-1");
    }

    #[test]
    fn instance_state_namespace_filters_cover_live_and_tombstone() {
        let namespace = Namespace("ns_alpha".into());
        let filters = instance_state_namespace_filters(&namespace);
        assert_eq!(
            filters,
            vec![
                "state.*.instance.ns_alpha.>".to_string(),
                "state.*.instance_tombstone.ns_alpha.>".to_string(),
            ]
        );
    }

    #[test]
    fn instance_subjects_disambiguate_across_namespaces() {
        let machine = MachineId("m1".into());
        let instance = InstanceId("inst-1".into());
        let alpha = instance_state_subject(&machine, &Namespace("alpha".into()), &instance);
        let beta = instance_state_subject(&machine, &Namespace("beta".into()), &instance);
        assert_ne!(alpha, beta);
    }

    #[test]
    fn instance_tombstone_subject_distinct_from_live_subject() {
        let machine = MachineId("m1".into());
        let namespace = Namespace("alpha".into());
        let instance = InstanceId("inst-1".into());
        assert_ne!(
            instance_state_subject(&machine, &namespace, &instance),
            instance_tombstone_subject(&machine, &namespace, &instance),
        );
    }

    #[test]
    fn per_machine_stream_names_isolate_writers() {
        let a = MachineId("m1".into());
        let b = MachineId("m2".into());
        assert_ne!(machine_state_stream(&a), machine_state_stream(&b));
        assert_ne!(instance_state_stream(&a), instance_state_stream(&b));
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
