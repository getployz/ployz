//! Product-neutral peer bootstrap, identity, and probe primitives.

mod endpoint;
mod identity;
mod probe;
mod rpc;
mod runtime;
mod tickets;

use thiserror::Error;

use crate::IrohEndpointId;

pub use endpoint::{PeerEndpoint, bind_peer_endpoint, issue_endpoint_ticket};
pub use identity::{PeerIdentity, load_or_create_identity};
pub use probe::{
    FakePeerProbe, PeerProbe, PeerProbeDeadline, PeerProbeReceipt, preflight_membership,
};
pub use rpc::{PeerRpcClient, PeerRpcListener, PeerRpcProbe};
pub use runtime::PeerRuntime;
pub use tickets::{PeerTicket, PeerTicketPath, import_ticket, issue_ticket};

pub const PLOYZ_PEER_ALPN: &[u8] = b"ployz/peer-rpc/0";

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("peer ticket is malformed")]
    MalformedTicket,
    #[error("peer identity is malformed")]
    MalformedIdentity,
    #[error("peer identity storage failed")]
    IdentityIo { source: std::io::Error },
    #[error("peer endpoint bind failed: {message}")]
    EndpointBind { message: String },
    #[error("peer endpoint did not become online before deadline")]
    EndpointOnlineTimeout,
    #[error("peer probe failed for {endpoint}: {reason}")]
    ProbeFailed {
        endpoint: IrohEndpointId,
        reason: String,
    },
    #[error("peer rpc timed out")]
    RpcTimeout,
    #[error("peer rpc transport failed: {message}")]
    RpcTransport { message: String },
    #[error("peer rpc runtime failed: {message}")]
    RpcRuntime { message: String },
}

pub type PeerProbeResult<T> = std::result::Result<T, PeerError>;

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::SocketAddr,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use iroh::TransportAddr;

    use super::*;

    fn deterministic_identity(byte: u8) -> PeerIdentity {
        PeerIdentity::from_bytes(&[byte; 32]).expect("identity")
    }

    #[test]
    fn tickets_round_trip_without_becoming_machine_truth() {
        let identity = deterministic_identity(7);
        let addr = identity
            .endpoint_addr()
            .with_ip_addr(SocketAddr::from(([127, 0, 0, 1], 1777)));
        let ticket = issue_ticket(addr);
        let encoded = ticket.encoded();
        let parsed = import_ticket(&encoded).expect("ticket");

        assert_eq!(parsed.endpoint_id(), identity.endpoint_id());
        assert_eq!(parsed.path(), PeerTicketPath::DirectOnly);
        assert_ne!(parsed.redacted(), encoded);
        assert!(!parsed.redacted().contains(identity.endpoint_id().as_str()));
    }

    #[test]
    fn malformed_ticket_is_typed() {
        assert!(matches!(
            import_ticket("not-a-ticket"),
            Err(PeerError::MalformedTicket)
        ));
    }

    #[tokio::test]
    async fn fake_peer_probe_succeeds_and_fails_with_typed_errors() {
        let reachable = IrohEndpointId::parse("reachable-endpoint").expect("endpoint");
        let failed = IrohEndpointId::parse("failed-endpoint").expect("endpoint");
        let probe = FakePeerProbe::new()
            .reachable(reachable.clone())
            .failing(failed.clone(), "membership denied");
        let deadline = PeerProbeDeadline::new(Duration::from_millis(250));

        let receipt = probe.probe(&reachable, deadline).await.expect("reachable");
        assert_eq!(receipt.endpoint(), &reachable);
        assert_eq!(receipt.alpn(), PLOYZ_PEER_ALPN);

        assert!(matches!(
            probe.probe(&failed, deadline).await,
            Err(PeerError::ProbeFailed { endpoint, reason })
                if endpoint == failed && reason == "membership denied"
        ));
    }

    #[test]
    fn load_or_create_identity_reuses_existing_key() {
        let path = temp_identity_path();
        let first = load_or_create_identity(&path).expect("first identity");
        let second = load_or_create_identity(&path).expect("second identity");

        assert_eq!(first.endpoint_id(), second.endpoint_id());

        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn bound_endpoint_uses_loaded_identity_for_bootstrap_ticket() {
        let path = temp_identity_path();
        let identity = load_or_create_identity(&path).expect("identity");
        let endpoint = bind_peer_endpoint(&identity).await.expect("endpoint");
        let ticket =
            issue_endpoint_ticket(&endpoint, PeerProbeDeadline::new(Duration::from_secs(5)))
                .await
                .expect("ticket");

        assert_eq!(endpoint.id().to_string(), identity.endpoint_id().as_str());
        assert_eq!(ticket.endpoint_id(), identity.endpoint_id());

        endpoint.close().await;
        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn created_identity_file_is_owner_only() {
        let path = temp_identity_path();
        load_or_create_identity(&path).expect("identity");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;

        assert_eq!(mode, 0o600);

        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn existing_identity_file_is_tightened_before_reuse() {
        let path = temp_identity_path();
        fs::write(&path, [13; 32]).expect("write identity");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("loosen");

        load_or_create_identity(&path).expect("identity");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;

        assert_eq!(mode, 0o600);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn ticket_path_reports_relay_and_direct_addr() {
        let identity = deterministic_identity(9);
        let relay = "https://relay.example./".parse().expect("relay url");
        let addr = identity.endpoint_addr().with_addrs([
            TransportAddr::Relay(relay),
            TransportAddr::Ip(SocketAddr::from(([127, 0, 0, 1], 1778))),
        ]);
        let ticket = issue_ticket(addr);

        assert_eq!(ticket.path(), PeerTicketPath::DirectOrRelay);
    }

    #[tokio::test]
    async fn preflight_membership_uses_probe_result() {
        let endpoint = IrohEndpointId::parse("reachable-endpoint").expect("endpoint");
        let probe = FakePeerProbe::new().reachable(endpoint.clone());

        let receipt = preflight_membership(
            &probe,
            &endpoint,
            PeerProbeDeadline::new(Duration::from_millis(100)),
        )
        .await
        .expect("preflight");

        assert_eq!(receipt.endpoint(), &endpoint);
    }

    #[tokio::test]
    async fn peer_rpc_probe_binds_receipt_to_configured_endpoint() {
        let endpoint = IrohEndpointId::parse("node-a-endpoint").expect("endpoint");
        let probe = PeerRpcProbe::from_service(
            endpoint.clone(),
            PeerTicketPath::DiscoveryOnly,
            rpc::PeerRpcServer::spawn().expect("server").client(),
        )
        .expect("probe");

        let receipt = probe
            .probe(&endpoint, PeerProbeDeadline::new(Duration::from_secs(1)))
            .await
            .expect("preflight");

        assert_eq!(receipt.endpoint(), &endpoint);
        assert_eq!(receipt.alpn(), PLOYZ_PEER_ALPN);
    }

    #[tokio::test]
    async fn peer_rpc_probe_rejects_unconfigured_endpoint_without_rpc_call() {
        let endpoint = IrohEndpointId::parse("node-a-endpoint").expect("endpoint");
        let other = IrohEndpointId::parse("node-b-endpoint").expect("endpoint");
        let probe = PeerRpcProbe::from_service(
            endpoint,
            PeerTicketPath::DiscoveryOnly,
            rpc::PeerRpcServer::spawn().expect("server").client(),
        )
        .expect("probe");

        let error = probe
            .probe(&other, PeerProbeDeadline::new(Duration::from_secs(1)))
            .await
            .expect_err("wrong endpoint");

        assert!(matches!(
            error,
            PeerError::ProbeFailed { endpoint, reason }
                if endpoint == other && reason == "peer rpc probe was built for a different endpoint"
        ));
    }

    #[tokio::test]
    async fn iroh_irpc_preflight_round_trips_between_endpoints() {
        let server_endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0)
            .await
            .expect("server endpoint");
        let listener = PeerRpcListener::start(server_endpoint.clone()).expect("listener");
        let ticket = issue_endpoint_ticket(
            &server_endpoint,
            PeerProbeDeadline::new(Duration::from_secs(5)),
        )
        .await
        .expect("ticket");

        let client_endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0)
            .await
            .expect("client endpoint");
        let client = PeerRpcClient::connect(client_endpoint, ticket.endpoint_addr().clone());
        client
            .preflight(PeerProbeDeadline::new(Duration::from_secs(5)))
            .await
            .expect("preflight");

        listener
            .shutdown(PeerProbeDeadline::new(Duration::from_secs(5)))
            .await
            .expect("router shutdown");
    }

    fn temp_identity_path() -> std::path::PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "polis-peer-identity-{}-{id}.key",
            std::process::id()
        ))
    }
}
