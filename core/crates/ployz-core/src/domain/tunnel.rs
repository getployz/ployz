//! Wiring of the reserved Tunnel Connector: cloudflared in the Ingress holder's
//! network, dialling Caddy's loopback listener with a token the daemon opened.

use std::collections::BTreeMap;

use crate::{
    Ciphertext, ContainerLabels, ContainerPath, MachinePath, QualifiedService, RawVolumeSource,
    RequestedServiceSpec, ServiceMode, ServiceMount, ServiceMountGraph, ServiceNetwork,
    ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference, UpdateConfig, caddy_container,
};

/// The label on the Tunnel Connector's spec that carries the sealed tunnel token, so
/// every Machine's observation of it lets a client or a joiner resend the same token.
/// It sits outside the `ployz.*` management namespace, which spec labels refuse.
pub const TUNNEL_TOKEN_LABEL: &str = "dev.ployz.ingress/sealed-token";
/// Where the daemon keeps the opened token on the Machine: a directory on `/run`,
/// readable by the connector's user only.
pub const TUNNEL_TOKEN_MACHINE_DIR: &str = "/run/ployz/tunnel";
/// The token's file name under [`TUNNEL_TOKEN_MACHINE_DIR`] and its mount.
pub const TUNNEL_TOKEN_FILE: &str = "token";
const TUNNEL_TOKEN_MOUNT: &str = "/run/tunnel";
/// The uid the pinned cloudflared image runs as. The token file is owned by it.
pub const TUNNEL_USER_ID: u32 = 65532;
/// Caddy's loopback listener the connector proxies every request to.
pub const TUNNEL_ORIGIN: &str = "http://127.0.0.1:8480";
/// Why a Machine orders no certificate while every Server uses the Tunnel Door. The
/// daemon records it on each wanted hostname, and a domain's status reads it back.
pub const TUNNEL_SERVES_CERTIFICATE: &str =
    "Cloudflare serves the certificate while every Server uses the Tunnel Door";

/// Build the reserved Tunnel Connector that joins the Ingress holder's network and
/// runs the tunnel `sealed_token` names.
///
/// The daemon's image pins in `ployzd/src/images.rs` are its only production
/// input. The CLI must not call it.
#[must_use]
pub fn tunnel_service_spec(image: &str, sealed_token: &Ciphertext) -> RequestedServiceSpec {
    let mut container = caddy_container(image.to_owned());
    container.entrypoint = vec!["cloudflared".into()];
    container.command = [
        "tunnel",
        "--no-autoupdate",
        "run",
        "--url",
        TUNNEL_ORIGIN,
        "--token-file",
        &format!("{TUNNEL_TOKEN_MOUNT}/{TUNNEL_TOKEN_FILE}"),
    ]
    .map(str::to_owned)
    .into();
    container.environment = BTreeMap::new();
    container.labels = ContainerLabels::parse(BTreeMap::from([(
        TUNNEL_TOKEN_LABEL.to_owned(),
        String::from(sealed_token.clone()),
    )]))
    .expect("the token label is outside the management namespace");
    container.network = Some(ServiceNetwork::IngressHolder);
    RequestedServiceSpec {
        name: QualifiedService::system_tunnel().name,
        mode: ServiceMode::Global,
        container,
        placement: Default::default(),
        ports: Vec::new(),
        mount_graph: ServiceMountGraph::parse(token_volume_graph(), Default::default())
            .expect("built-in tunnel mounts are valid"),
        pre_deploy: None,
        update: UpdateConfig::default(),
    }
}

/// The sealed token a Tunnel Connector spec carries, when it is one.
#[must_use]
pub fn tunnel_sealed_token(labels: &ContainerLabels) -> Option<Ciphertext> {
    labels
        .as_map()
        .get(TUNNEL_TOKEN_LABEL)
        .cloned()
        .and_then(|label| Ciphertext::try_from(label).ok())
}

fn token_volume_graph() -> ServiceVolumeGraph {
    let token = ServiceVolumeReference::parse("tunnel-token").expect("static volume is valid");
    ServiceVolumeGraph::parse(
        vec![ServiceVolume {
            reference: token.clone(),
            source: RawVolumeSource::Bind {
                machine_path: MachinePath::parse(TUNNEL_TOKEN_MACHINE_DIR)
                    .expect("static token path is valid"),
                create_machine_path: false,
                propagation: None,
                recursive: None,
            }
            .admit()
            .expect("built-in tunnel volume is valid"),
        }],
        vec![ServiceMount {
            volume: token,
            target: ContainerPath::parse(TUNNEL_TOKEN_MOUNT).expect("static mount path is valid"),
            read_only: true,
            no_copy: false,
            subpath: None,
        }],
    )
    .expect("static tunnel Volume graph is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed() -> Ciphertext {
        Ciphertext::from_bytes(vec![9; 48])
    }

    #[test]
    fn the_connector_joins_the_holder_and_carries_its_sealed_token_as_a_label() {
        let spec = tunnel_service_spec("cloudflared:2026", &sealed());
        assert_eq!(spec.container.network, Some(ServiceNetwork::IngressHolder));
        assert!(spec.ports.is_empty());
        assert_eq!(tunnel_sealed_token(&spec.container.labels), Some(sealed()));
        assert!(
            spec.container
                .command
                .join(" ")
                .contains(&format!("--url {TUNNEL_ORIGIN}"))
        );
        let [mount] = spec.mount_graph.volume_graph().mounts() else {
            panic!("the connector has one mount");
        };
        assert!(mount.read_only);
        assert_eq!(mount.target.to_string(), TUNNEL_TOKEN_MOUNT);
    }
}
