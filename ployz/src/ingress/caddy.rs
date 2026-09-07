//! Concrete Caddy deployment wiring for the Ingress Proxy.

use oci_client::errors::OciDistributionError;
use oci_client::{Client, ParseError, Reference, client::ClientConfig, secrets::RegistryAuth};
use semver::Version;
use thiserror::Error;

/// Failure while discovering the current Caddy image for ingress deployment.
#[derive(Debug, Error)]
pub enum IngressImageError {
    /// The configured image reference could not be parsed.
    #[error("parse Caddy image reference: {0}")]
    Reference(#[from] ParseError),
    /// Docker Hub tags could not be listed.
    #[error("list Docker Hub Caddy tags: {}", crate::setup_retry::detail(.0))]
    ListTags(#[from] OciDistributionError),
    #[error("{0}")]
    Timeout(String),
}

/// Discover the latest stable Caddy 2 image used for ingress.
///
/// # Errors
///
/// Returns [`IngressImageError`] when the image reference is invalid or Docker
/// Hub cannot list its tags.
pub async fn latest_image() -> Result<String, IngressImageError> {
    let reference = "docker.io/library/caddy:latest".parse::<Reference>()?;
    let mut client = Client::new(ClientConfig {
        connect_timeout: Some(std::time::Duration::from_secs(5)),
        read_timeout: Some(std::time::Duration::from_secs(5)),
        ..ClientConfig::default()
    });
    discover_image(&mut client, &reference).await
}

async fn discover_image(
    client: &mut Client,
    reference: &Reference,
) -> Result<String, IngressImageError> {
    let response = crate::setup_retry::run(
        client,
        "Discovering Caddy image at Docker Hub",
        crate::setup_retry::WAIT,
        |error| {
            matches!(error, oci_client::errors::OciDistributionError::RequestError(error)
            if crate::setup_retry::transient_http(error))
        },
        async |client| {
            client
                .list_tags(reference, &RegistryAuth::Anonymous, None, None)
                .await
        },
    )
    .await
    .map_err(|error| match error {
        crate::setup_retry::Error::Permanent(error) => IngressImageError::ListTags(error),
        crate::setup_retry::Error::Exhausted(message) => IngressImageError::Timeout(message),
    })?;
    Ok(select_image(&response.tags))
}

#[must_use]
fn select_image(tags: &[String]) -> String {
    tags.iter()
        .filter_map(|tag| {
            Version::parse(tag).ok().filter(|version| {
                version.major == 2
                    && version.pre.is_empty()
                    && version.build.is_empty()
                    && version.to_string() == *tag
            })
        })
        .max()
        .map_or_else(
            || "caddy:latest".into(),
            |version| format!("caddy:{version}"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discovery_retries_a_held_request_and_stops_on_registry_rejection() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        for status in [200, 403] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let reference = format!("{}/library/caddy:latest", listener.local_addr().unwrap())
                .parse::<Reference>()
                .unwrap();
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = calls.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = [0; 4096];
                    let length = socket.read(&mut request).await.unwrap();
                    assert!(length > 0);
                    let tags = request
                        .get(..length)
                        .unwrap()
                        .starts_with(b"GET /v2/library/caddy/tags/list");
                    if tags
                        && count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
                        && status == 200
                    {
                        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    }
                    let body = r#"{"name":"library/caddy","tags":["2.10.0"]}"#;
                    let _ = socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                }
            });
            let mut client = Client::new(ClientConfig {
                protocol: oci_client::client::ClientProtocol::Http,
                read_timeout: Some(std::time::Duration::from_millis(50)),
                ..ClientConfig::default()
            });
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                discover_image(&mut client, &reference),
            )
            .await
            .unwrap();
            if status == 200 {
                assert_eq!(result.unwrap(), "caddy:2.10.0");
                assert!(calls.load(std::sync::atomic::Ordering::SeqCst) >= 2);
            } else {
                assert!(matches!(result, Err(IngressImageError::ListTags(_))));
                assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            }
            server.abort();
        }
    }

    #[test]
    fn selects_only_the_greatest_bare_two_x_y_tag() {
        assert_eq!(
            select_image(&[
                "2.9.1".into(),
                "2.10.0".into(),
                "2.11.0-rc.1".into(),
                "2.10".into(),
                "latest".into(),
                "3.0.0".into(),
            ]),
            "caddy:2.10.0"
        );
        assert_eq!(select_image(&["latest".into()]), "caddy:latest");
    }
}
