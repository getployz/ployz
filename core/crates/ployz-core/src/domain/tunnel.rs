//! Wiring of the reserved Tunnel Door: a Tunnel Origin that owns a network namespace
//! on its own fenced bridge and passes each request to a socket that serves only
//! public routes, and the Tunnel Connector joined to it, running cloudflared on a
//! token the daemon opened.

use std::collections::BTreeMap;

use crate::{
    Ciphertext, ContainerLabels, ContainerPath, INGRESS_RUNTIME_MOUNT, MachinePath,
    QualifiedService, RawVolumeSource, RequestedServiceSpec, ServiceMode, ServiceMount,
    ServiceMountGraph, ServiceNetwork, ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference,
    UpdateConfig, caddy_container,
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
/// The directory, inside the Ingress Proxy's runtime directory, that holds the socket
/// Caddy serves the Tunnel Door on. The Tunnel Origin mounts this directory alone, so
/// it sees neither Caddy's admin socket nor anything else Caddy keeps there.
pub const TUNNEL_ORIGIN_MACHINE_DIR: &str = "/run/ployz/ingress/tunnel";
/// The socket's file name under [`TUNNEL_ORIGIN_MACHINE_DIR`].
pub const TUNNEL_ORIGIN_SOCKET: &str = "origin.sock";
const TUNNEL_ORIGIN_MOUNT: &str = "/run/origin";
/// The port the Tunnel Origin listens on in the connector's network namespace, and
/// the one the Tunnel Door's sites are keyed under in Caddy's config, where they
/// listen on the origin socket alone.
pub const TUNNEL_SITE_PORT: u16 = 8480;
/// The port Cloudflare's edge takes tunnel connections on, over QUIC and HTTP/2.
pub const CLOUDFLARE_EDGE_PORT: u16 = 7844;
/// Cloudflare's published edge ranges, checked 2026-10-13 against
/// <https://www.cloudflare.com/ips-v4> and <https://www.cloudflare.com/ips-v6>.
pub const CLOUDFLARE_RANGES: [&str; 22] = [
    "173.245.48.0/20",
    "103.21.244.0/22",
    "103.22.200.0/22",
    "103.31.4.0/22",
    "141.101.64.0/18",
    "108.162.192.0/18",
    "190.93.240.0/20",
    "188.114.96.0/20",
    "197.234.240.0/22",
    "198.41.128.0/17",
    "162.158.0.0/15",
    "104.16.0.0/13",
    "104.24.0.0/14",
    "172.64.0.0/13",
    "131.0.72.0/22",
    "2400:cb00::/32",
    "2606:4700::/32",
    "2803:f800::/32",
    "2405:b500::/32",
    "2405:8100::/32",
    "2a06:98c0::/29",
    "2c0f:f248::/32",
];

/// The origin socket as Caddy names its listener: under its runtime mount, writable by
/// every user so the connector's uid may connect.
#[must_use]
pub fn tunnel_origin_listener() -> String {
    format!("unix/{INGRESS_RUNTIME_MOUNT}/tunnel/{TUNNEL_ORIGIN_SOCKET}|0222")
}
/// Why a Machine orders no certificate while every Server uses the Tunnel Door. The
/// daemon records it on each wanted hostname, and a domain's status reads it back.
pub const TUNNEL_SERVES_CERTIFICATE: &str =
    "Cloudflare serves the certificate while every Server uses the Tunnel Door";

/// The origin the connector proxies every request to: the Tunnel Origin's listener,
/// which a tunnel managed in Cloudflare's dashboard names as `http://localhost:8480`.
#[must_use]
pub fn tunnel_origin() -> String {
    format!("http://127.0.0.1:{TUNNEL_SITE_PORT}")
}

/// Build the reserved Tunnel Origin: the pinned Caddy, admin off, listening on
/// [`TUNNEL_SITE_PORT`] and passing each request with its Host header to the origin
/// socket. It owns the Tunnel Connector's network namespace on its own bridge.
///
/// Caddy would say `http` in `X-Forwarded-Proto`, the scheme it was reached by, so
/// it passes on the one the connector reported, which the origin socket redirects by.
///
/// The daemon's image pins in `ployzd/src/images.rs` are its only production
/// input. The CLI must not call it.
#[must_use]
pub fn tunnel_origin_service_spec(image: &str) -> RequestedServiceSpec {
    let mut container = caddy_container(image.to_owned());
    container.command = [
        "caddy",
        "reverse-proxy",
        "--from",
        &format!(":{TUNNEL_SITE_PORT}"),
        "--to",
        &format!("unix/{TUNNEL_ORIGIN_MOUNT}/{TUNNEL_ORIGIN_SOCKET}"),
        "--header-up",
        "X-Forwarded-Proto: {http.request.header.X-Forwarded-Proto}",
    ]
    .map(str::to_owned)
    .into();
    container.environment = BTreeMap::new();
    container.network = Some(ServiceNetwork::Tunnel);
    RequestedServiceSpec {
        name: QualifiedService::system_tunnel_origin().name,
        mode: ServiceMode::Global,
        container,
        placement: Default::default(),
        ports: Vec::new(),
        mount_graph: ServiceMountGraph::parse(
            read_only_bind(
                "tunnel-origin",
                TUNNEL_ORIGIN_MACHINE_DIR,
                true,
                TUNNEL_ORIGIN_MOUNT,
            ),
            Default::default(),
        )
        .expect("built-in tunnel origin mounts are valid"),
        pre_deploy: None,
        update: UpdateConfig::default(),
    }
}

/// Build the reserved Tunnel Connector that runs the tunnel `sealed_token` names, in
/// the Tunnel Origin's network namespace.
///
/// A tunnel managed in Cloudflare's dashboard takes its routes from Cloudflare and
/// ignores the origin given here, so the connector's reach is what holds: the Tunnel
/// Origin's bridge, which the Machine's firewall lets reach only DNS and Cloudflare's
/// edge, and the Tunnel Origin's listener, which serves only the routes Caddy
/// publishes.
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
        &tunnel_origin(),
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
    container.network = Some(ServiceNetwork::TunnelOrigin);
    RequestedServiceSpec {
        name: QualifiedService::system_tunnel().name,
        mode: ServiceMode::Global,
        container,
        placement: Default::default(),
        ports: Vec::new(),
        mount_graph: ServiceMountGraph::parse(
            read_only_bind(
                "tunnel-token",
                TUNNEL_TOKEN_MACHINE_DIR,
                false,
                TUNNEL_TOKEN_MOUNT,
            ),
            Default::default(),
        )
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

/// One directory on the Machine mounted read-only at `target`.
fn read_only_bind(
    name: &str,
    machine_path: &str,
    create_machine_path: bool,
    target: &str,
) -> ServiceVolumeGraph {
    let reference = ServiceVolumeReference::parse(name).expect("static volume is valid");
    ServiceVolumeGraph::parse(
        vec![ServiceVolume {
            reference: reference.clone(),
            source: RawVolumeSource::Bind {
                machine_path: MachinePath::parse(machine_path).expect("static path is valid"),
                create_machine_path,
                propagation: None,
                recursive: None,
            }
            .admit()
            .expect("built-in tunnel volume is valid"),
        }],
        vec![ServiceMount {
            volume: reference,
            target: ContainerPath::parse(target).expect("static mount path is valid"),
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
    fn caddy_names_the_origin_socket_the_tunnel_origin_mounts() {
        assert_eq!(
            tunnel_origin_listener(),
            "unix//run/ingress/tunnel/origin.sock|0222"
        );
    }

    #[test]
    fn the_connector_joins_the_origin_and_dials_its_listener() {
        let spec = tunnel_service_spec("cloudflared:2026", &sealed());
        assert_eq!(spec.container.network, Some(ServiceNetwork::TunnelOrigin));
        assert!(spec.ports.is_empty());
        assert_eq!(tunnel_sealed_token(&spec.container.labels), Some(sealed()));
        let command = spec.container.command.join(" ");
        assert!(command.contains("--url http://127.0.0.1:8480"), "{command}");
        let [mount] = spec.mount_graph.volume_graph().mounts() else {
            panic!("the connector mounts only its token");
        };
        assert!(mount.read_only);
        assert_eq!(mount.target.to_string(), TUNNEL_TOKEN_MOUNT);
    }

    #[test]
    fn the_origin_has_its_own_bridge_and_passes_its_listener_to_the_socket() {
        let spec = tunnel_origin_service_spec("caddy:2");
        assert_eq!(spec.container.network, Some(ServiceNetwork::Tunnel));
        assert!(spec.ports.is_empty());
        assert!(spec.container.environment.is_empty());
        assert_eq!(
            spec.container.command.join(" "),
            "caddy reverse-proxy --from :8480 --to unix//run/origin/origin.sock \
             --header-up X-Forwarded-Proto: {http.request.header.X-Forwarded-Proto}"
        );
        let [mount] = spec.mount_graph.volume_graph().mounts() else {
            panic!("the origin mounts only the socket's directory");
        };
        assert!(mount.read_only);
        assert_eq!(mount.target.to_string(), TUNNEL_ORIGIN_MOUNT);
    }
}
