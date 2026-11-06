use std::collections::BTreeSet;

use ployz_core::MachineId;

use super::Misses;

fn id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

#[test]
fn two_misses_in_a_row_name_a_peer_and_one_answer_clears_it() {
    let (a, b) = (id('a'), id('b'));
    let probed = BTreeSet::from([a, b]);
    let mut misses = Misses::default();

    assert!(misses.round(&probed, &BTreeSet::from([b])).is_empty());
    assert_eq!(misses.round(&probed, &BTreeSet::from([b])), vec![a]);
    assert_eq!(misses.round(&probed, &BTreeSet::from([b])), vec![a]);
    assert!(misses.round(&probed, &probed).is_empty());
}

#[test]
fn an_answer_between_misses_starts_the_count_again() {
    let a = id('a');
    let probed = BTreeSet::from([a]);
    let mut misses = Misses::default();

    assert!(misses.round(&probed, &BTreeSet::new()).is_empty());
    assert!(misses.round(&probed, &probed).is_empty());
    assert!(misses.round(&probed, &BTreeSet::new()).is_empty());
}

#[test]
fn a_peer_no_longer_probed_is_forgotten() {
    let a = id('a');
    let probed = BTreeSet::from([a]);
    let mut misses = Misses::default();

    misses.round(&probed, &BTreeSet::new());
    assert_eq!(misses.round(&probed, &BTreeSet::new()), vec![a]);
    assert!(misses.round(&BTreeSet::new(), &BTreeSet::new()).is_empty());
    assert!(misses.round(&probed, &BTreeSet::new()).is_empty());
}

mod dialer {
    use std::{convert::Infallible, future::Ready, net::Ipv6Addr, time::Duration};

    use ployz_core::ManagementAddress;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::{
        body::Body,
        codegen::{Service, http},
        server::NamedService,
        transport::Server,
    };

    use super::super::Dialer;

    const LOOPBACK: ManagementAddress = ManagementAddress(Ipv6Addr::LOCALHOST);
    const SHORT: Duration = Duration::from_millis(300);

    /// A daemon that refuses every call with one gRPC status, as a trailers-only reply.
    #[derive(Clone)]
    struct Refusing(&'static str);

    impl Service<http::Request<Body>> for Refusing {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: http::Request<Body>) -> Self::Future {
            let mut response = http::Response::new(Body::empty());
            let headers = response.headers_mut();
            headers.insert(
                "content-type",
                http::HeaderValue::from_static("application/grpc"),
            );
            headers.insert("grpc-status", http::HeaderValue::from_static(self.0));
            std::future::ready(Ok(response))
        }
    }

    impl NamedService for Refusing {
        const NAME: &'static str = "ployz.rpc.v1.MachineRpc";
    }

    async fn serve(code: &'static str) -> u16 {
        let listener = TcpListener::bind("[::1]:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(
            Server::builder()
                .add_service(Refusing(code))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        port
    }

    #[tokio::test]
    async fn a_refusal_from_the_daemon_counts_as_an_answer() {
        // PermissionDenied, and an Unavailable the daemon itself sent.
        for code in ["7", "14"] {
            let dialer = Dialer::with_timeout(serve(code).await, SHORT);
            assert!(dialer.answers(LOOPBACK).await, "grpc-status {code}");
            assert_eq!(
                dialer.channels.lock().unwrap().len(),
                1,
                "an answer keeps its channel"
            );
        }
    }

    #[tokio::test]
    async fn a_refused_connection_is_a_miss() {
        let listener = TcpListener::bind("[::1]:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(!Dialer::with_timeout(port, SHORT).answers(LOOPBACK).await);
    }

    #[tokio::test]
    async fn a_daemon_that_never_replies_is_a_miss() {
        // The kernel accepts the connection, but nothing speaks HTTP/2 on it.
        let listener = TcpListener::bind("[::1]:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dialer = Dialer::with_timeout(port, SHORT);
        let started = std::time::Instant::now();
        assert!(!dialer.answers(LOOPBACK).await);
        assert!(started.elapsed() < SHORT * 3);
        // The stalled connection isn't kept: the next probe dials afresh.
        assert!(dialer.channels.lock().unwrap().is_empty());
        drop(listener);
    }
}
