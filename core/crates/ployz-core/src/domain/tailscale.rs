//! Wiring of the reserved Private Access router: Tailscale in userspace mode on the
//! ployz bridge, advertising the Cluster network to a tailnet with an auth key the
//! daemon opened.

use std::collections::BTreeMap;

use ipnet::Ipv4Net;

use crate::{
    Ciphertext, ContainerLabels, ContainerPath, MachinePath, QualifiedService, RawVolumeSource,
    RequestedServiceSpec, ServiceMode, ServiceMount, ServiceMountGraph, ServiceVolume,
    ServiceVolumeGraph, ServiceVolumeReference, UpdateConfig, caddy_container,
};

/// The label on the router's spec that carries the sealed auth key, so every
/// Machine's observation of it lets a client or a joiner resend the same key. It sits
/// outside the `ployz.*` management namespace, which spec labels refuse.
pub const PRIVATE_ACCESS_KEY_LABEL: &str = "dev.ployz.private-access/sealed-key";
/// Where the daemon keeps the opened auth key on the Machine: a directory on `/run`,
/// which is memory, so a reboot clears it.
pub const PRIVATE_ACCESS_KEY_MACHINE_DIR: &str = "/run/ployz/tailscale";
/// The key's file name under [`PRIVATE_ACCESS_KEY_MACHINE_DIR`] and its mount.
pub const PRIVATE_ACCESS_KEY_FILE: &str = "auth-key";
const PRIVATE_ACCESS_KEY_MOUNT: &str = "/run/ployz-auth";
/// The name a router shows in the tailnet. Tailscale numbers the ones that follow.
pub const PRIVATE_ACCESS_HOSTNAME: &str = "ployz-router";

/// Build the reserved Private Access router that advertises `network` to the tailnet
/// `sealed_key` joins.
///
/// The daemon's image pin in `ployzd/src/images.rs` is its only production input.
/// The CLI must not call it.
#[must_use]
pub fn private_access_service_spec(
    image: &str,
    network: Ipv4Net,
    sealed_key: &Ciphertext,
) -> RequestedServiceSpec {
    let mut container = caddy_container(image.to_owned());
    container.command = Vec::new();
    container.environment = [
        ("TS_USERSPACE", "true".to_owned()),
        ("TS_ROUTES", network.to_string()),
        ("TS_HOSTNAME", PRIVATE_ACCESS_HOSTNAME.to_owned()),
        (
            "TS_EXTRA_ARGS",
            format!("--auth-key=file:{PRIVATE_ACCESS_KEY_MOUNT}/{PRIVATE_ACCESS_KEY_FILE}"),
        ),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.into()))
    .collect();
    container.labels = ContainerLabels::parse(BTreeMap::from([(
        PRIVATE_ACCESS_KEY_LABEL.to_owned(),
        String::from(sealed_key.clone()),
    )]))
    .expect("the key label is outside the management namespace");
    RequestedServiceSpec {
        name: QualifiedService::system_tailscale().name,
        mode: ServiceMode::Global,
        container,
        placement: Default::default(),
        ports: Vec::new(),
        mount_graph: ServiceMountGraph::parse(key_volume_graph(), Default::default())
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

fn key_volume_graph() -> ServiceVolumeGraph {
    let key = ServiceVolumeReference::parse("auth-key").expect("static volume is valid");
    ServiceVolumeGraph::parse(
        vec![ServiceVolume {
            reference: key.clone(),
            source: RawVolumeSource::Bind {
                machine_path: MachinePath::parse(PRIVATE_ACCESS_KEY_MACHINE_DIR)
                    .expect("static key path is valid"),
                create_machine_path: false,
                propagation: None,
                recursive: None,
            }
            .admit()
            .expect("built-in router volume is valid"),
        }],
        vec![ServiceMount {
            volume: key,
            target: ContainerPath::parse(PRIVATE_ACCESS_KEY_MOUNT)
                .expect("static mount path is valid"),
            read_only: true,
            no_copy: false,
            subpath: None,
        }],
    )
    .expect("static router Volume graph is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed() -> Ciphertext {
        Ciphertext::from_bytes(vec![9; 48])
    }

    fn network() -> Ipv4Net {
        "10.210.0.0/16".parse().unwrap()
    }

    #[test]
    fn the_router_advertises_the_cluster_network_and_reads_its_key_from_a_file() {
        let spec = private_access_service_spec("tailscale:1", network(), &sealed());
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
        let [mount] = spec.mount_graph.volume_graph().mounts() else {
            panic!("the router has one mount");
        };
        assert!(mount.read_only);
        assert_eq!(mount.target.to_string(), PRIVATE_ACCESS_KEY_MOUNT);
    }

    #[test]
    fn the_sealed_key_rides_in_a_label_and_reads_back() {
        let spec = private_access_service_spec("tailscale:1", network(), &sealed());
        assert_eq!(
            private_access_sealed_key(&spec.container.labels),
            Some(sealed())
        );
    }

    #[test]
    fn two_networks_make_two_specs() {
        let other: Ipv4Net = "10.99.0.0/16".parse().unwrap();
        assert_ne!(
            private_access_service_spec("tailscale:1", network(), &sealed()),
            private_access_service_spec("tailscale:1", other, &sealed()),
        );
    }
}
