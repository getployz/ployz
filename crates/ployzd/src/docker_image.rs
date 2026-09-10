//! Docker image preparation shared by workloads and managed services.

use bollard::{Docker, errors::Error, query_parameters::CreateImageOptionsBuilder};
use futures_util::TryStreamExt;
use ployz_core::PullPolicy;

pub(crate) async fn prepare_image(
    docker: &Docker,
    image: &str,
    policy: PullPolicy,
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
        // Docker pulls every tag when the reference has no tag or digest.
        let image =
            if !image.contains('@') && !image.rsplit('/').next().unwrap_or(image).contains(':') {
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
                None,
            )
            .try_for_each(|_| async { Ok(()) })
            .await?;
    }
    Ok(())
}

pub(crate) fn is_not_found(error: &Error) -> bool {
    matches!(
        error,
        Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Router, http::StatusCode};
    use tokio::net::TcpListener;

    use super::*;

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
            prepare_image(&docker, image, PullPolicy::Always)
                .await
                .unwrap();
            assert_eq!(requests.lock().unwrap().pop().as_deref(), Some(expected));
        }
        server.abort();
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
            let result = prepare_image(&docker, "test", policy).await;
            server.abort();
            assert_eq!(result.is_ok(), succeeds, "{policy:?}: {result:?}");
            assert_eq!(*paths.lock().unwrap(), expected_paths, "{policy:?}");
        }
    }
}
