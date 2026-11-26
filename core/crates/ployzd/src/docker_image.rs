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
    /// A drain copied this image, by its content, here; creation runs exactly it, whatever
    /// the spec's tag names now. Unsigned, it runs only once this Machine pulled it from
    /// the spec's repository by digest, with no login.
    Pinned(&'auth ployz_core::ImageDigest),
    /// A Free Build loaded this image, and its receipt held: creation runs exactly it from
    /// this Machine's store, never pulled, whatever the spec's image names.
    Free(&'auth FreeImage),
}

/// An image a Free Build of this Cluster loaded, by its ID: a receipt whose MAC held under
/// this Machine's Sealing Key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FreeImage(ployz_core::ImageDigest);

impl FreeImage {
    /// The image `receipt` names, once its MAC holds under this Machine's Sealing Key.
    ///
    /// # Errors
    /// Returns [`FreeImageRefusal::NotThisCluster`] for a receipt no Machine of this
    /// Cluster made, and the key's own error when this Machine holds no usable key.
    pub(crate) fn check(
        receipt: &ployz_core::FreeReceipt,
        sealing: &crate::sealed_env::SealingKeyStore,
    ) -> Result<Self, crate::docker::Error> {
        if sealing.made_free_receipt(receipt)? {
            Ok(Self(receipt.image.clone()))
        } else {
            Err(crate::docker::Error::FreeImage(
                FreeImageRefusal::NotThisCluster(receipt.image.clone()),
            ))
        }
    }

    pub(crate) fn id(&self) -> &ployz_core::ImageDigest {
        &self.0
    }

    /// `image` as if its receipt held, for a test past the check.
    #[cfg(test)]
    pub(crate) fn for_test(image: ployz_core::ImageDigest) -> Self {
        Self(image)
    }
}

/// Why an unsigned creation won't run the image its Free receipt names.
#[derive(Debug, thiserror::Error)]
pub enum FreeImageRefusal {
    #[error("no Server of this Cluster made the receipt for image {0}")]
    NotThisCluster(ployz_core::ImageDigest),
    #[error("image {0} isn't on this Server; copy it here, then create the Container again")]
    Absent(ployz_core::ImageDigest),
}

/// `image`'s repository, registry and port kept, pinned to `id`: `repo:tag` becomes
/// `repo@sha256:…`.
pub(crate) fn pinned_reference(image: &str, id: &ployz_core::ImageDigest) -> String {
    format!("{}@{id}", ployz_core::image_repository(image))
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

/// Pull `image` from its registry. A reference that names a digest is pulled by the digest
/// alone, `repo:tag@sha256:…` as `repo@sha256:…`, so the pull writes no tag.
async fn pull_image(
    docker: &Docker,
    image: &str,
    auth: Option<&RegistryAuth>,
) -> Result<(), Error> {
    let image = match ployz_core::ImageRef::new(image).digest {
        Some(digest) => format!("{}@{digest}", ployz_core::image_repository(image)),
        None => tagged(image),
    };
    docker
        .create_image(
            Some(
                CreateImageOptionsBuilder::default()
                    .from_image(&image)
                    .build(),
            ),
            None,
            auth.map(|auth| DockerCredentials {
                username: auth.username.clone(),
                password: Some(auth.password.clone()),
                ..Default::default()
            }),
        )
        .try_for_each(|_| async { Ok(()) })
        .await
}

/// `image`, a reference with no digest, naming `latest` when it names no tag: Docker pulls
/// every tag of a bare repository.
fn tagged(image: &str) -> String {
    if last_part(image).contains(':') {
        image.to_owned()
    } else {
        format!("{image}:latest")
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
/// and answer the ID of the image that pull placed. A tag is resolved to its digest first,
/// with no login, and only the digest is pulled: the pull writes no name, so a tag here
/// keeps naming what a caller with Full access put under it.
pub(crate) async fn pull_public(docker: &Docker, image: &str) -> Result<String, UnsignedImage> {
    require_public_registry(image).await?;
    let pinned = match ployz_core::ImageRef::new(image).digest {
        Some(digest) => format!("{}@{digest}", ployz_core::image_repository(image)),
        None => {
            let digest = docker
                .inspect_registry_image(&tagged(image), None)
                .await
                .map_err(UnsignedImage::Pull)?
                .descriptor
                .digest
                .ok_or(UnsignedImage::NoDigest)?;
            format!("{}@{digest}", ployz_core::image_repository(image))
        }
    };
    pull_image(docker, &pinned, None)
        .await
        .map_err(UnsignedImage::Pull)?;
    docker
        .inspect_image(&pinned)
        .await
        .map_err(UnsignedImage::Pull)?
        .id
        .ok_or(UnsignedImage::NoDigest)
}

/// Why this Machine does not pull an image with a sealed registry login.
#[derive(Debug, thiserror::Error)]
pub enum LoginRefused {
    #[error(transparent)]
    Uppercase(#[from] ployz_core::UppercaseHost),
    #[error(
        "the registry credentials are for {sealed}, and the image is on {image}. Enter them again for {image}"
    )]
    OtherHost { sealed: String, image: String },
    #[error(
        "Docker on this Server reaches {0} without TLS, so it would send the login in clear text"
    )]
    Insecure(String),
    #[error(
        "Docker on this Server pulls Docker Hub images through a mirror, which would get the login"
    )]
    Mirrored,
    #[error("could not read Docker's registry settings")]
    Unread(#[source] Error),
}

/// The login `login` holds, to pull `image` with: only when it was sealed for the host
/// Docker pulls `image` from, and Docker sends it to that host alone, over TLS.
pub(crate) async fn open_login(
    docker: &Docker,
    sealing: &crate::sealed_env::SealingKeyStore,
    image: &str,
    login: &ployz_core::SealedLogin,
) -> Result<RegistryAuth, crate::docker::Error> {
    let host = ployz_core::login_host(image).map_err(LoginRefused::from)?;
    if host != login.host {
        return Err(LoginRefused::OtherHost {
            sealed: login.host.clone(),
            image: host.to_owned(),
        }
        .into());
    }
    let settings = docker
        .info()
        .await
        .map_err(LoginRefused::Unread)?
        .registry_config
        .unwrap_or_default();
    let index = settings.index_configs.unwrap_or_default();
    if host == ployz_core::DOCKER_HUB {
        let mirrored = index
            .get(host)
            .and_then(|hub| hub.mirrors.as_ref())
            .into_iter()
            .chain(settings.mirrors.as_ref())
            .any(|mirrors| !mirrors.is_empty());
        if mirrored {
            return Err(LoginRefused::Mirrored.into());
        }
    } else if insecure(host, &index, settings.insecure_registry_cidrs.as_deref()).await {
        return Err(LoginRefused::Insecure(host.to_owned()).into());
    }
    Ok(sealing.open_login(login)?)
}

/// Whether Docker reaches registry `host` without TLS, as it decides: the setting it
/// holds under that name, else whether an address the name resolves to is in a range
/// it was told is insecure, `127.0.0.0/8` and `::1/128` by default.
async fn insecure(
    host: &str,
    index: &std::collections::HashMap<String, bollard::models::IndexInfo>,
    ranges: Option<&[String]>,
) -> bool {
    if let Some(secure) = index.get(host).and_then(|setting| setting.secure) {
        return !secure;
    }
    let ranges: Vec<ipnet::IpNet> = ranges
        .unwrap_or_default()
        .iter()
        .filter_map(|range| range.parse().ok())
        .collect();
    // Docker counts a name it can't resolve as secure; then the pull fails on its own.
    let Ok(addresses) = tokio::net::lookup_host(registry_target(host)).await else {
        return false;
    };
    addresses
        .map(|address| address.ip())
        .any(|ip| ranges.iter().any(|range| range.contains(&ip)))
}

/// `registry` as an address to resolve: HTTPS's port when it names none.
fn registry_target(registry: &str) -> String {
    if registry
        .rsplit(']')
        .next()
        .unwrap_or(registry)
        .contains(':')
    {
        registry.to_owned()
    } else {
        format!("{registry}:443")
    }
}

/// Refuse a registry on this Machine or its private networks: reaching one proves
/// nothing about who else can.
async fn require_public_registry(image: &str) -> Result<(), UnsignedImage> {
    let Some(registry) = ployz_core::image_registry(image) else {
        return Ok(());
    };
    let target = registry_target(registry);
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

    /// A fake Docker whose `info` reports `registry` as its registry settings.
    async fn docker_with(registry: serde_json::Value) -> (Docker, tokio::task::JoinHandle<()>) {
        let app = Router::new().fallback(move |uri: axum::http::Uri| {
            let registry = registry.clone();
            async move {
                assert!(uri.path().ends_with("/info"), "only info is read: {uri}");
                axum::Json(serde_json::json!({ "RegistryConfig": registry }))
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
        (docker, server)
    }

    #[tokio::test]
    async fn a_sealed_login_opens_only_for_its_registry_reached_over_tls() {
        use crate::docker::Error as Refused;
        let cluster = crate::sealed_env::test_support::Cluster::new();
        let store = cluster.store();
        let auth = RegistryAuth {
            username: Some("octocat".into()),
            password: "token".into(),
        };
        let (docker, server) = docker_with(serde_json::json!({
            "InsecureRegistryCIDRs": ["127.0.0.0/8"],
            "IndexConfigs": {
                "docker.io": { "Name": "docker.io", "Secure": true, "Official": true },
                "plain.example:5000": { "Name": "plain.example:5000", "Secure": false },
            },
        }))
        .await;
        let open = |image: &'static str, host: &str| {
            let login = cluster.seal_login(host, &auth);
            let (docker, store) = (&docker, &store);
            async move { open_login(docker, store, image, &login).await }
        };
        assert_eq!(
            open("ghcr.io/acme/api@sha256:aa", "ghcr.io").await.unwrap(),
            auth
        );
        assert_eq!(open("acme/api:1", "docker.io").await.unwrap(), auth);
        for (image, host) in [
            ("evil.example/acme/api", "ghcr.io"),
            ("ghcr.io/acme/api", "docker.io"),
            ("acme/api", "ghcr.io"),
        ] {
            let refused = open(image, host).await.unwrap_err();
            assert!(
                matches!(
                    refused,
                    Refused::RegistryLogin(LoginRefused::OtherHost { .. })
                ),
                "{image}: {refused:?}"
            );
        }
        // Docker reads an uppercase first part as a host, whatever the login says.
        let refused = open("GHCR.io/acme/api", "GHCR.io").await.unwrap_err();
        assert!(
            matches!(refused, Refused::RegistryLogin(LoginRefused::Uppercase(_))),
            "{refused:?}"
        );
        // Docker would send it in clear text: named insecure, or in an insecure range.
        for (image, host) in [
            ("plain.example:5000/api", "plain.example:5000"),
            ("127.0.0.1:5000/api", "127.0.0.1:5000"),
            ("localhost:5000/api", "localhost:5000"),
        ] {
            let refused = open(image, host).await.unwrap_err();
            assert!(
                matches!(refused, Refused::RegistryLogin(LoginRefused::Insecure(_))),
                "{image}: {refused:?}"
            );
        }
        // Sealed to another Cluster, it doesn't open here.
        let other = crate::sealed_env::test_support::Cluster::new().seal_login("ghcr.io", &auth);
        let refused = open_login(&docker, &store, "ghcr.io/acme/api", &other)
            .await
            .unwrap_err();
        assert!(
            matches!(
                refused,
                Refused::Sealed(crate::sealed_env::OpenError::RegistryLogin(_))
            ),
            "{refused:?}"
        );
        server.abort();

        // A mirror would get a Docker Hub login; another registry's still opens.
        let (docker, server) = docker_with(serde_json::json!({
            "Mirrors": ["https://mirror.example/"],
            "IndexConfigs": {
                "docker.io": { "Name": "docker.io", "Mirrors": ["https://mirror.example/"], "Secure": true },
            },
        }))
        .await;
        let hub = cluster.seal_login("docker.io", &auth);
        let refused = open_login(&docker, &store, "acme/api", &hub)
            .await
            .unwrap_err();
        assert!(
            matches!(refused, Refused::RegistryLogin(LoginRefused::Mirrored)),
            "{refused:?}"
        );
        let ghcr = cluster.seal_login("ghcr.io", &auth);
        assert_eq!(
            open_login(&docker, &store, "ghcr.io/acme/api", &ghcr)
                .await
                .unwrap(),
            auth
        );
        server.abort();
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
