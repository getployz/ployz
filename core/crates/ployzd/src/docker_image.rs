//! Docker image preparation shared by workloads and managed services.

use bollard::{
    Docker, auth::DockerCredentials, errors::Error, query_parameters::CreateImageOptionsBuilder,
};
use futures_util::TryStreamExt;
use ployz_core::{PullPolicy, RegistryAuth};

/// How Container creation obtains its image.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ImagePull<'auth> {
    /// Prepare the image as the spec's pull policy asks, with these credentials.
    Policy(Option<&'auth RegistryAuth>),
    /// A deploy's pull phase already placed the image; creation uses it as is.
    Prepared,
}

/// Make `image` present as `policy` asks, pulling with `auth` for a private image.
pub(crate) async fn prepare_image(
    docker: &Docker,
    image: &str,
    policy: PullPolicy,
    auth: Option<&RegistryAuth>,
) -> Result<(), Error> {
    let pull = match policy {
        PullPolicy::Always => true,
        PullPolicy::Never => false,
        PullPolicy::Missing => match docker.inspect_image(image).await {
            Ok(_) => false,
            Err(error) if is_not_found(&error) => true,
            Err(error) => return Err(error),
        },
    };
    if pull {
        pull_image(docker, image, auth).await?;
    }
    Ok(())
}

/// Pull `image` from its registry and answer the manifest digest Docker reports for it,
/// when it reports one.
async fn pull_image(
    docker: &Docker,
    image: &str,
    auth: Option<&RegistryAuth>,
) -> Result<Option<String>, Error> {
    // Docker pulls every tag when the reference has no tag or digest.
    let image = if !image.contains('@') && !last_part(image).contains(':') {
        std::borrow::Cow::Owned(format!("{image}:latest"))
    } else {
        std::borrow::Cow::Borrowed(image)
    };
    docker
        .create_image(
            Some(
                CreateImageOptionsBuilder::default()
                    .from_image(image.as_ref())
                    .build(),
            ),
            None,
            auth.map(|auth| DockerCredentials {
                username: auth.username.clone(),
                password: Some(auth.password.clone()),
                ..Default::default()
            }),
        )
        .try_fold(None, |digest, progress| async move {
            let reported = progress
                .status
                .as_deref()
                .and_then(|status| status.strip_prefix("Digest: "))
                .map(str::to_owned);
            Ok(reported.or(digest))
        })
        .await
}

/// `image` with its tag and digest dropped.
pub(crate) fn repository(image: &str) -> &str {
    let name = image.split_once('@').map_or(image, |(name, _)| name);
    match last_part(name).rsplit_once(':') {
        Some((_, tag)) => &name[..name.len() - tag.len() - 1],
        None => name,
    }
}

fn last_part(image: &str) -> &str {
    image.rsplit('/').next().unwrap_or(image)
}

/// Why this Machine does not run an image under a spec nothing signed.
#[derive(Debug, thiserror::Error)]
pub enum UnsignedImage {
    #[error("this Server could not pull the image with no login")]
    Pull(#[source] Error),
    #[error("the image's registry {0} could not be found")]
    Unresolved(String, #[source] std::io::Error),
    #[error("the image's registry {0} answers on a local or private address")]
    Private(String),
    #[error("the registry named no digest for the image")]
    NoDigest,
    #[error("the image changed while its Container was created")]
    Moved,
    #[error("this Server has not pulled it, and its pull policy is never")]
    NeverPulled,
}

/// Pull `image` from a public registry with no login, whatever is here under its name,
/// and answer the ID of the image that pull placed. The ID is read by the digest the
/// pull reported, so a tag another caller moves meanwhile changes nothing.
pub(crate) async fn pull_public(docker: &Docker, image: &str) -> Result<String, UnsignedImage> {
    require_public_registry(image).await?;
    let pulled = pull_image(docker, image, None)
        .await
        .map_err(UnsignedImage::Pull)?
        .ok_or(UnsignedImage::NoDigest)?;
    docker
        .inspect_image(&format!("{}@{pulled}", repository(image)))
        .await
        .map_err(UnsignedImage::Pull)?
        .id
        .ok_or(UnsignedImage::NoDigest)
}

/// The host and port of the registry Docker pulls `image` from, or `None` for Docker Hub.
/// Docker takes the first part for a host when it holds a dot, a colon or a capital
/// letter, or is `localhost`.
fn registry(image: &str) -> Option<&str> {
    let (first, _) = image.split_once('/')?;
    (first == "localhost" || first.contains(['.', ':']) || first.to_lowercase() != first)
        .then_some(first)
}

/// Refuse a registry on this Machine or its private networks: reaching one proves
/// nothing about who else can.
async fn require_public_registry(image: &str) -> Result<(), UnsignedImage> {
    let Some(registry) = registry(image) else {
        return Ok(());
    };
    let target = if registry
        .rsplit(']')
        .next()
        .unwrap_or(registry)
        .contains(':')
    {
        registry.to_owned()
    } else {
        format!("{registry}:443")
    };
    let mut addresses = tokio::net::lookup_host(&target)
        .await
        .map_err(|error| UnsignedImage::Unresolved(registry.to_owned(), error))?
        .peekable();
    if addresses.peek().is_none() || addresses.any(|address| !public(address.ip())) {
        return Err(UnsignedImage::Private(registry.to_owned()));
    }
    Ok(())
}

/// Whether `ip` is an address anyone on the internet reaches. Anything this cannot
/// place there counts as private.
fn public(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [first, second, third, _] = ip.octets();
            // 100.64.0.0/10, the shared range tailnets use.
            let shared = first == 100 && second & 0xc0 == 64;
            // 192.0.0.0/24 for protocol use, and 198.18.0.0/15 for benchmarks.
            let reserved =
                (first, second, third) == (192, 0, 0) || (first == 198 && second & 0xfe == 18);
            // 0.0.0.0/8 is this network; 224.0.0.0/3 is multicast and what follows it.
            !(first == 0
                || first >= 224
                || ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_documentation()
                || shared
                || reserved)
        }
        std::net::IpAddr::V6(ip) => {
            let embedded = |high: u16, low: u16| {
                let [a, b] = high.to_be_bytes();
                let [c, d] = low.to_be_bytes();
                public(std::net::Ipv4Addr::new(a, b, c, d).into())
            };
            match (ip.to_ipv4_mapped(), ip.segments()) {
                (Some(mapped), _) => public(mapped.into()),
                // NAT64 and 6to4 carry the IPv4 address the packet ends up at.
                (None, [0x64, 0xff9b, 0, 0, 0, 0, high, low] | [0x2002, high, low, ..]) => {
                    embedded(high, low)
                }
                // Every public IPv6 address is in 2000::/3. 2001:db8::/32 and 3fff::/20
                // are for documents.
                (None, [first, second, ..]) => {
                    first & 0xe000 == 0x2000
                        && (first, second) != (0x2001, 0xdb8)
                        && (first, second & 0xf000) != (0x3fff, 0)
                }
            }
        }
    }
}

pub(crate) fn is_not_found(error: &Error) -> bool {
    crate::docker::classify(error) == crate::docker::Status::Absent
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Router, http::StatusCode};
    use tokio::net::TcpListener;

    use base64::Engine as _;

    use super::*;

    #[tokio::test]
    async fn an_unsigned_image_comes_only_from_a_registry_at_a_public_address() {
        for image in [
            "nginx:1.29",
            "library/nginx",
            "8.8.8.8/api:1",
            "[2606:4700::1111]:5000/api",
            "[64:ff9b::808:808]:5000/api",
        ] {
            require_public_registry(image).await.unwrap();
        }
        for image in [
            "127.0.0.1:5000/api:1",
            "10.0.0.5/api:1",
            "192.168.1.5:5000/api@sha256:aa",
            "100.64.0.9/api",
            "169.254.169.254/api",
            "[::1]:5000/api",
            "[fd00::1]:7572/api",
            "[::ffff:10.0.0.5]:5000/api",
            "0.1.2.3/api",
            "192.0.0.8/api",
            "198.18.0.1/api",
            "203.0.113.7/api",
            "240.0.0.1/api",
            "[64:ff9b::a00:5]:5000/api",
            "[64:ff9b:1::a00:5]:5000/api",
            "[2002:a00:5::1]:5000/api",
            "[fec0::1]:5000/api",
            "[ff02::1]:5000/api",
            "[::10.0.0.5]:5000/api",
            "[3fff::1]:5000/api",
        ] {
            assert!(
                matches!(
                    require_public_registry(image).await,
                    Err(UnsignedImage::Private(_))
                ),
                "{image}"
            );
        }
    }

    #[test]
    fn an_image_names_its_registry_as_docker_reads_it() {
        for (image, host) in [
            ("nginx", None),
            ("library/nginx:1.29", None),
            ("ghcr.io/acme/api:1", Some("ghcr.io")),
            ("localhost/api", Some("localhost")),
            ("registry:5000/api", Some("registry:5000")),
            // A first part with a capital letter is a host to Docker, not a Hub account.
            ("Registry/api:1", Some("Registry")),
        ] {
            assert_eq!(registry(image), host, "{image}");
        }
    }

    #[tokio::test]
    async fn pulls_only_the_requested_tag_or_digest() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let app = Router::new().fallback(move |uri: axum::http::Uri| {
            let captured = captured.clone();
            async move {
                let image = reqwest::Url::parse(&format!("http://docker{uri}"))
                    .unwrap()
                    .query_pairs()
                    .find(|(key, _)| key == "fromImage")
                    .unwrap()
                    .1
                    .into_owned();
                captured.lock().unwrap().push(image);
                (StatusCode::OK, "{}\n")
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let docker = Docker::connect_with_http(
            &format!("http://{address}"),
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap();
        for (image, expected) in [
            ("whoami", "whoami:latest"),
            ("traefik/whoami", "traefik/whoami:latest"),
            ("localhost:5000/app", "localhost:5000/app:latest"),
            ("localhost:5000/app:v1", "localhost:5000/app:v1"),
            ("[::1]:5000/app", "[::1]:5000/app:latest"),
            ("app@sha256:abcd", "app@sha256:abcd"),
            ("app:v1@sha256:abcd", "app:v1@sha256:abcd"),
        ] {
            prepare_image(&docker, image, PullPolicy::Always, None)
                .await
                .unwrap();
            assert_eq!(requests.lock().unwrap().pop().as_deref(), Some(expected));
        }
        server.abort();
    }

    #[tokio::test]
    async fn a_private_pull_carries_its_credentials() {
        let headers = Arc::new(Mutex::new(Vec::new()));
        let captured = headers.clone();
        let app = Router::new().fallback(move |request: axum::http::HeaderMap| {
            let captured = captured.clone();
            async move {
                captured
                    .lock()
                    .unwrap()
                    .push(request.get("x-registry-auth").cloned());
                (StatusCode::OK, "{}\n")
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let docker = Docker::connect_with_http(
            &format!("http://{address}"),
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap();
        let auth = RegistryAuth {
            username: Some("octocat".into()),
            password: "token".into(),
        };
        prepare_image(&docker, "ghcr.io/acme/api", PullPolicy::Always, Some(&auth))
            .await
            .unwrap();
        prepare_image(&docker, "nginx", PullPolicy::Always, None)
            .await
            .unwrap();
        server.abort();
        let headers = headers.lock().unwrap().clone();
        let [Some(private), public] = headers.as_slice() else {
            panic!("expected two pulls, the first with credentials: {headers:?}");
        };
        let decoded = base64::engine::general_purpose::URL_SAFE
            .decode(private.as_bytes())
            .unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(decoded.get("username"), Some(&"octocat".into()));
        assert_eq!(decoded.get("password"), Some(&"token".into()));
        assert!(
            public
                .as_ref()
                .is_none_or(|header| !header.to_str().unwrap().contains("token"))
        );
    }

    #[tokio::test]
    async fn pull_policies_preserve_errors_and_drain_the_stream() {
        for (policy, inspect_status, pull_body, expected_paths, succeeds) in [
            (
                PullPolicy::Never,
                StatusCode::INTERNAL_SERVER_ERROR,
                "",
                vec![],
                true,
            ),
            (
                PullPolicy::Missing,
                StatusCode::OK,
                "",
                vec!["inspect"],
                true,
            ),
            (
                PullPolicy::Missing,
                StatusCode::NOT_FOUND,
                "{}\n{}\n",
                vec!["inspect", "pull"],
                true,
            ),
            (
                PullPolicy::Missing,
                StatusCode::INTERNAL_SERVER_ERROR,
                "",
                vec!["inspect"],
                false,
            ),
            (
                PullPolicy::Always,
                StatusCode::OK,
                "{}\n{}\n",
                vec!["pull"],
                true,
            ),
            (
                PullPolicy::Always,
                StatusCode::OK,
                "{}\ninvalid-json\n",
                vec!["pull"],
                false,
            ),
        ] {
            let paths = Arc::new(Mutex::new(Vec::new()));
            let requests = paths.clone();
            let app = Router::new().fallback(move |uri: axum::http::Uri| {
                let requests = requests.clone();
                async move {
                    if uri.path().ends_with("/images/create") {
                        requests.lock().unwrap().push("pull");
                        (StatusCode::OK, pull_body)
                    } else {
                        assert!(uri.path().ends_with("/images/test/json"));
                        requests.lock().unwrap().push("inspect");
                        (inspect_status, "{}")
                    }
                }
            });
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let docker = Docker::connect_with_http(
                &format!("http://{address}"),
                5,
                bollard::API_DEFAULT_VERSION,
            )
            .unwrap();
            let result = prepare_image(&docker, "test", policy, None).await;
            server.abort();
            assert_eq!(result.is_ok(), succeeds, "{policy:?}: {result:?}");
            assert_eq!(*paths.lock().unwrap(), expected_paths, "{policy:?}");
        }
    }
}
