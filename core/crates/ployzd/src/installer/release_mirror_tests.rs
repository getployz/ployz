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
    net::TcpListener,
};

use super::get_if_present;

/// A resolver that fails, finds no address, or never answers.
#[derive(Clone, Copy)]
enum Dns {
    Fails,
    Empty,
    Hangs,
}

impl Resolve for Dns {
    fn resolve(&self, _: Name) -> Resolving {
        let dns = *self;
        Box::pin(async move {
            match dns {
                Dns::Fails => Err("no such host".into()),
                Dns::Empty => Ok(Box::new(std::iter::empty()) as Addrs),
                Dns::Hangs => std::future::pending().await,
            }
        })
    }
}

fn client(dns: Option<Dns>) -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_millis(200));
    match dns {
        Some(dns) => builder.dns_resolver(Arc::new(dns)),
        None => builder,
    }
    .build()
    .unwrap()
}

/// Serves `status` with body `body` to every request, counting them.
async fn http(status: &'static str, body: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (format!("http://{address}"), requests)
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

/// Fetch through `primary`, then a fallback mirror; the fallback's request count and the result.
async fn through(
    client: &reqwest::Client,
    primary: &str,
) -> (usize, Result<Option<Vec<u8>>, reqwest::Error>) {
    let (fallback, requests) = http("200 OK", "fallback").await;
    let result = ployz_core::from_release_mirrors(&[primary, &fallback], "v1.2.3/asset", |url| {
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
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    fell_back(through(&client(None), &format!("https://{address}")).await);
}

#[tokio::test]
async fn a_name_that_does_not_resolve_falls_back() {
    let primary = "https://github.test";
    fell_back(through(&client(Some(Dns::Fails)), primary).await);
    // A host without IPv4 finds no usable address for an IPv4-only name.
    fell_back(through(&client(Some(Dns::Empty)), primary).await);
}

#[tokio::test]
async fn an_untrusted_certificate_never_falls_back() {
    let address = self_signed().await;
    let error = stayed(through(&client(None), &format!("https://{address}")).await);
    assert!(!error.is_timeout(), "{error:?}");
}

#[tokio::test]
async fn a_connect_timeout_never_falls_back() {
    let error = stayed(through(&client(Some(Dns::Hangs)), "https://github.test").await);
    assert!(error.is_timeout(), "{error:?}");
}

#[tokio::test]
async fn a_server_error_never_falls_back() {
    let (primary, _) = http("500 Internal Server Error", "broken").await;
    let error = stayed(through(&client(None), &primary).await);
    assert_eq!(
        error.status(),
        Some(reqwest::StatusCode::INTERNAL_SERVER_ERROR)
    );
}
