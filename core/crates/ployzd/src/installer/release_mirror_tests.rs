//! Each failure class against real local sockets: only a mirror that was never reached falls
//! back to the next.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket},
};

use super::get_if_present;

/// A resolver that fails, finds no address, times out, or never answers.
#[derive(Clone, Copy)]
enum Dns {
    Fails,
    Empty,
    TimesOut,
    Hangs,
}

impl Resolve for Dns {
    fn resolve(&self, _: Name) -> Resolving {
        let dns = *self;
        Box::pin(async move {
            match dns {
                Dns::Fails => Err("no such host".into()),
                Dns::Empty => Ok(Box::new(std::iter::empty()) as Addrs),
                Dns::TimesOut => Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into()),
                Dns::Hangs => std::future::pending().await,
            }
        })
    }
}

fn client(dns: Option<Dns>) -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(1));
    match dns {
        Some(dns) => builder.dns_resolver(Arc::new(dns)),
        None => builder,
    }
    .build()
    .unwrap()
}

/// Serves `status`, with any `headers`, and body `body` to every request under its URL,
/// counting them. That URL's path is unique on this host: other processes, such as other
/// tests, probe loopback ports, and a request one of them sends is answered 404, uncounted.
async fn http(
    status: &'static str,
    headers: String,
    body: &'static str,
) -> (String, Arc<AtomicUsize>) {
    static SERVERS: AtomicUsize = AtomicUsize::new(0);
    let path = format!(
        "/{}-{}",
        std::process::id(),
        SERVERS.fetch_add(1, Ordering::SeqCst)
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    let ours = format!("GET {path}/");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = [0_u8; 1024];
            let read = stream.read(&mut request).await.unwrap_or(0);
            let response = if request
                .get(..read)
                .is_some_and(|received| received.starts_with(ours.as_bytes()))
            {
                counted.fetch_add(1, Ordering::SeqCst);
                format!(
                    "HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    .to_owned()
            };
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (format!("http://{address}{path}"), requests)
}

/// The mirror a fallback reaches, counting its requests.
async fn fallback() -> (String, Arc<AtomicUsize>) {
    http("200 OK", String::new(), "fallback").await
}

/// A loopback URL whose port refuses connections while the returned socket lives. The socket
/// holds the port bound without listening, so no other process can take it.
fn refused() -> (String, TcpSocket) {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let address = socket.local_addr().unwrap();
    (format!("https://{address}"), socket)
}

/// An HTTPS listener whose certificate no client trusts.
async fn self_signed() -> SocketAddr {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).unwrap();
    let key = rustls::pki_types::PrivateKeyDer::try_from(signing_key.serialize_der()).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.der().clone()], key)
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let _ = acceptor.accept(stream).await;
        }
    });
    address
}

/// Fetch through `primary`, then `fallback`; the fallback's request count and the result.
async fn through(
    client: &reqwest::Client,
    primary: &str,
    (fallback, requests): (String, Arc<AtomicUsize>),
) -> (usize, Result<Option<Vec<u8>>, reqwest::Error>) {
    let result = ployz_core::from_release_mirrors([primary, &fallback], "v1.2.3/asset", |url| {
        get_if_present(client, url)
    })
    .await;
    (requests.load(Ordering::SeqCst), result)
}

fn fell_back((requests, result): (usize, Result<Option<Vec<u8>>, reqwest::Error>)) {
    assert_eq!(result.unwrap().as_deref(), Some(b"fallback".as_slice()));
    assert_eq!(requests, 1);
}

fn stayed((requests, result): (usize, Result<Option<Vec<u8>>, reqwest::Error>)) -> reqwest::Error {
    assert_eq!(requests, 0, "the fallback mirror was tried");
    result.unwrap_err()
}

#[tokio::test]
async fn a_refused_connection_falls_back() {
    let (primary, _port) = refused();
    fell_back(through(&client(None), &primary, fallback().await).await);
}

#[tokio::test]
async fn a_name_that_does_not_resolve_falls_back() {
    let primary = "https://github.test";
    fell_back(through(&client(Some(Dns::Fails)), primary, fallback().await).await);
    // A host without IPv4 finds no usable address for an IPv4-only name.
    fell_back(through(&client(Some(Dns::Empty)), primary, fallback().await).await);
}

#[tokio::test]
async fn an_untrusted_certificate_never_falls_back() {
    let address = self_signed().await;
    let error = stayed(
        through(
            &client(None),
            &format!("https://{address}"),
            fallback().await,
        )
        .await,
    );
    let chain = ployz_core::error_chain::inline(&error);
    assert!(chain.contains("certificate"), "{chain}");
}

#[tokio::test]
async fn a_timeout_never_falls_back() {
    let primary = "https://github.test";
    let error = stayed(through(&client(Some(Dns::Hangs)), primary, fallback().await).await);
    assert!(error.is_timeout(), "{error:?}");
    // A resolver's own timeout comes through as a DNS error, and still never falls back.
    let error = stayed(through(&client(Some(Dns::TimesOut)), primary, fallback().await).await);
    assert!(error.is_timeout(), "{error:?}");
}

#[tokio::test]
async fn a_server_error_never_falls_back() {
    let (primary, _) = http("500 Internal Server Error", String::new(), "broken").await;
    let error = stayed(through(&client(None), &primary, fallback().await).await);
    assert_eq!(
        error.status(),
        Some(reqwest::StatusCode::INTERNAL_SERVER_ERROR)
    );
}

/// GitHub redirects every asset to its asset host; that host is the one that must answer.
#[tokio::test]
async fn an_unreachable_redirect_target_falls_back() {
    let (target, _port) = refused();
    let target = target.replace("https://", "http://");
    let (primary, _) = http("302 Found", format!("location: {target}/asset\r\n"), "").await;
    fell_back(through(&client(None), &primary, fallback().await).await);
}
