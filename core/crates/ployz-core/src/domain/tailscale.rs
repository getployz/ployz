//! Wiring of the reserved Private Access router: Tailscale in userspace mode on the
//! ployz bridge, advertising the Cluster network to a tailnet with an auth key the
//! daemon opened. The node it registers lives on the Machine, so a restart or a reboot
//! brings the same node back, with its approved route, and the key is read only
//! when the node first joins.

use std::collections::BTreeMap;

use ipnet::Ipv4Net;

use crate::{
    Ciphertext, ConfiguredHealthcheck, ContainerLabels, ContainerPath, HealthcheckCommand,
    HealthcheckSpec, MachinePath, PrivateAccessStateId, QualifiedService, RawVolumeSource, RequestedServiceSpec, ServiceMode, ServiceMount,
    ServiceMountGraph, ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference, UpdateConfig,
    caddy_container,
};

/// The label on the router's spec that carries the sealed auth key, so every
/// Machine's observation of it lets a client or a joiner resend the same key. It sits
/// outside the `ployz.*` management namespace, which spec labels refuse.
pub const PRIVATE_ACCESS_KEY_LABEL: &str = "dev.ployz.private-access/sealed-key";
/// The label on the router's spec that names the node state it mounts, so the daemon
/// keeps only a router whose state is the one on disk now.
pub const PRIVATE_ACCESS_STATE_LABEL: &str = "dev.ployz.private-access/state-id";
/// Where the daemon keeps the opened auth key on the Machine: a directory on `/run`,
/// which is memory, so a reboot clears it.
pub const PRIVATE_ACCESS_KEY_MACHINE_DIR: &str = "/run/ployz/tailscale";
/// The key's file name under [`PRIVATE_ACCESS_KEY_MACHINE_DIR`] and its mount.
pub const PRIVATE_ACCESS_KEY_FILE: &str = "auth-key";
const PRIVATE_ACCESS_KEY_MOUNT: &str = "/run/ployz-auth";
/// Where the daemon keeps the router's node state on the Machine: the identity and
/// the keys Tailscale registered, on disk, so a restart or a reboot is the same node.
/// `ployz private-access off` deletes it, so a later `on` registers a new node.
pub const PRIVATE_ACCESS_STATE_MACHINE_DIR: &str = "/var/lib/ployz/tailscale";
const PRIVATE_ACCESS_STATE_MOUNT: &str = "/var/lib/tailscale";
/// The name a router shows in the tailnet. Tailscale numbers the ones that follow.
pub const PRIVATE_ACCESS_HOSTNAME: &str = "ployz-router";
/// Passes once the node joined the tailnet. A router Tailscale refused the key for keeps
/// restarting and its container shows as running between restarts, so only this tells a
/// router that serves from one that doesn't.
const JOINED_TAILNET_CHECK: &str =
    "tailscale status --peers=false --json | grep -q '\"BackendState\": \"Running\"'";

/// Build the reserved Private Access router that advertises `network` to the tailnet
/// `sealed_key` joins, mounting the node state `state` names.
///
/// The daemon's image pin in `ployzd/src/images.rs` is its only production input.
/// The CLI must not call it.
#[must_use]
pub fn private_access_service_spec(
    image: &str,
    network: Ipv4Net,
    sealed_key: &Ciphertext,
    state: PrivateAccessStateId,
) -> RequestedServiceSpec {
    let mut container = caddy_container(image.to_owned());
    container.command = Vec::new();
    container.healthcheck = Some(HealthcheckSpec::Configured(ConfiguredHealthcheck {
        test: HealthcheckCommand::parse(["CMD-SHELL", JOINED_TAILNET_CHECK])
            .expect("the router healthcheck is a command"),
        interval_millis: Some(5_000),
        timeout_millis: Some(5_000),
        start_period_millis: None,
        start_interval_millis: None,
        retries: Some(3),
        deadline_millis: None,
    }));
    container.environment = [
        ("TS_USERSPACE", "true".to_owned()),
        ("TS_ROUTES", network.to_string()),
        ("TS_HOSTNAME", PRIVATE_ACCESS_HOSTNAME.to_owned()),
        ("TS_STATE_DIR", PRIVATE_ACCESS_STATE_MOUNT.to_owned()),
        // Log in only when the node state holds no login, and apply the routes and
        // the hostname on every start, so a restarted node needs no key.
        ("TS_AUTH_ONCE", "true".to_owned()),
        (
            "TS_EXTRA_ARGS",
            format!("--auth-key=file:{PRIVATE_ACCESS_KEY_MOUNT}/{PRIVATE_ACCESS_KEY_FILE}"),
        ),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.into()))
    .collect();
    container.labels = ContainerLabels::parse(BTreeMap::from([
        (
            PRIVATE_ACCESS_KEY_LABEL.to_owned(),
            String::from(sealed_key.clone()),
        ),
        (PRIVATE_ACCESS_STATE_LABEL.to_owned(), String::from(state)),
    ]))
    .expect("the router labels are outside the management namespace");
    RequestedServiceSpec {
        name: QualifiedService::system_tailscale().name,
        mode: ServiceMode::Global,
        container,
        placement: Default::default(),
        ports: Vec::new(),
        mount_graph: ServiceMountGraph::parse(router_volume_graph(), Default::default())
            .expect("built-in router mounts are valid"),
        pre_deploy: None,
        update: UpdateConfig::default(),
    }
}

/// The sealed auth key a router spec carries, when it is one.
#[must_use]
pub fn private_access_sealed_key(labels: &ContainerLabels) -> Option<Ciphertext> {
    labels
        .as_map()
        .get(PRIVATE_ACCESS_KEY_LABEL)
        .cloned()
        .and_then(|label| Ciphertext::try_from(label).ok())
}

/// The node state a router spec mounts, when it names one.
#[must_use]
pub fn private_access_state(labels: &ContainerLabels) -> Option<PrivateAccessStateId> {
    labels
        .as_map()
        .get(PRIVATE_ACCESS_STATE_LABEL)
        .and_then(|label| PrivateAccessStateId::parse(label).ok())
}

/// The key, read-only, and the node state, which the router writes. The daemon makes
/// both directories with the modes it wants before the router is placed, so Docker
/// creates neither.
fn router_volume_graph() -> ServiceVolumeGraph {
    let key = ServiceVolumeReference::parse("auth-key").expect("static volume is valid");
    let state = ServiceVolumeReference::parse("node-state").expect("static volume is valid");
    ServiceVolumeGraph::parse(
        vec![
            bind_volume(key.clone(), PRIVATE_ACCESS_KEY_MACHINE_DIR),
            bind_volume(state.clone(), PRIVATE_ACCESS_STATE_MACHINE_DIR),
        ],
        vec![
            mount(&key, PRIVATE_ACCESS_KEY_MOUNT, true),
            mount(&state, PRIVATE_ACCESS_STATE_MOUNT, false),
        ],
    )
    .expect("static router Volume graph is valid")
}

fn bind_volume(reference: ServiceVolumeReference, machine_path: &str) -> ServiceVolume {
    ServiceVolume {
        reference,
        source: RawVolumeSource::Bind {
            machine_path: MachinePath::parse(machine_path).expect("static router path is valid"),
            create_machine_path: false,
            propagation: None,
            recursive: None,
        }
        .admit()
        .expect("built-in router volume is valid"),
    }
}

fn mount(volume: &ServiceVolumeReference, target: &str, read_only: bool) -> ServiceMount {
    ServiceMount {
        volume: volume.clone(),
        target: ContainerPath::parse(target).expect("static mount path is valid"),
        read_only,
        no_copy: false,
        subpath: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed() -> Ciphertext {
        Ciphertext::from_bytes(vec![9; 48])
    }

    fn state() -> PrivateAccessStateId {
        PrivateAccessStateId::parse("a".repeat(32)).unwrap()
    }

    fn network() -> Ipv4Net {
        "10.210.0.0/16".parse().unwrap()
    }

    #[test]
    fn the_router_advertises_the_cluster_network_and_reads_its_key_from_a_file() {
        let spec = private_access_service_spec("tailscale:1", network(), &sealed(), state());
        assert_eq!(spec.container.network, None);
        assert!(spec.ports.is_empty());
        assert_eq!(spec.mode, ServiceMode::Global);
        let env = |name: &str| match spec.container.environment.get(name) {
            Some(crate::EnvValue::Text(text)) => text.clone(),
            Some(other @ (crate::EnvValue::Sealed(_) | crate::EnvValue::Template(_))) => {
                panic!("{name} is plain text, not {other:?}")
            }
            None => panic!("{name} is set"),
        };
        assert_eq!(env("TS_USERSPACE"), "true");
        assert_eq!(env("TS_ROUTES"), "10.210.0.0/16");
        assert!(
            env("TS_EXTRA_ARGS").contains(&format!("file:{PRIVATE_ACCESS_KEY_MOUNT}/")),
            "the key is a file path, never a value"
        );
        assert!(!spec.container.environment.contains_key("TS_AUTHKEY"));
        let [key, state] = spec.mount_graph.volume_graph().mounts() else {
            panic!("the router mounts the key and its node state");
        };
        assert!(key.read_only);
        assert_eq!(key.target.to_string(), PRIVATE_ACCESS_KEY_MOUNT);
        assert!(!state.read_only, "the router writes its node state");
        assert_eq!(state.target.to_string(), env("TS_STATE_DIR"));
    }

    // Catches a router that logs in on every start, which registers a new node whose
    // route needs approving again, and needs the key again after it expired.
    #[test]
    fn the_node_state_lives_on_the_machine_and_the_key_is_read_once() {
        let spec = private_access_service_spec("tailscale:1", network(), &sealed(), state());
        assert_eq!(
            spec.container.environment.get("TS_AUTH_ONCE"),
            Some(&crate::EnvValue::Text("true".to_owned()))
        );
        let machine_paths: Vec<String> = spec
            .mount_graph
            .volume_graph()
            .volumes()
            .iter()
            .filter_map(|volume| match volume.source.kind() {
                RawVolumeSource::Bind { machine_path, .. } => Some(machine_path.to_string()),
                RawVolumeSource::External { .. }
                | RawVolumeSource::Ordinary { .. }
                | RawVolumeSource::Provisioned { .. }
                | RawVolumeSource::Tmpfs { .. } => None,
            })
            .collect();
        assert!(
            machine_paths.contains(&PRIVATE_ACCESS_STATE_MACHINE_DIR.to_owned()),
            "the node state is a Machine path, not a Docker volume: {machine_paths:?}"
        );
    }

    #[test]
    fn the_sealed_key_and_the_node_state_ride_in_labels_and_read_back() {
        let spec = private_access_service_spec("tailscale:1", network(), &sealed(), state());
        assert_eq!(
            private_access_sealed_key(&spec.container.labels),
            Some(sealed())
        );
        assert_eq!(private_access_state(&spec.container.labels), Some(state()));
    }

    #[test]
    fn two_networks_make_two_specs() {
        let other: Ipv4Net = "10.99.0.0/16".parse().unwrap();
        assert_ne!(
            private_access_service_spec("tailscale:1", network(), &sealed(), state()),
            private_access_service_spec("tailscale:1", other, &sealed(), state()),
        );
    }
}
