use std::sync::{Arc, Mutex};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::*;

const INDEX: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const TOKEN: &str = "fixture-token";

/// One request a fixture registry saw.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
}

/// A registry on a local port. `answer` gets each request and the registry's own host and
/// returns the whole response, or `None` to say nothing.
async fn registry(
    answer: impl Fn(&Seen, &str) -> Option<String> + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(answer);
    let (log, own) = (seen.clone(), host.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (log, own, answer) = (log.clone(), own.clone(), answer.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut chunk = [0u8; 1024];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => head.extend_from_slice(chunk.get(..read).unwrap_or_default()),
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let mut lines = text.lines();
                let mut request = lines.next().unwrap_or_default().split(' ');
                let seen = Seen {
                    method: request.next().unwrap_or_default().to_owned(),
                    path: request.next().unwrap_or_default().to_owned(),
                    authorization: lines
                        .filter_map(|line| line.split_once(": "))
                        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                        .map(|(_, value)| value.to_owned()),
                };
                log.lock().unwrap().push(seen.clone());
                match answer(&seen, &own) {
                    Some(response) => {
                        let _ = stream.write_all(response.as_bytes()).await;
                    }
                    None => std::future::pending::<()>().await,
                }
            });
        }
    });
    (host, seen)
}

fn response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
    let headers: String = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn pin_on(host: &str, reference: &str) -> impl Future<Output = Result<PinnedImage, PinError>> {
    pin(
        reference,
        ClientProtocol::HttpsExcept(vec![host.to_owned()]),
    )
}

#[tokio::test]
async fn a_tag_pins_to_the_digest_the_registry_serves_it_under() {
    // A registry that hands anonymous readers a pull token, as Docker Hub does.
    let (host, seen) = registry(
        |seen, own| match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", "/v2/") => Some(response(
                "401 Unauthorized",
                &[(
                    "WWW-Authenticate",
                    &format!("Bearer realm=\"http://{own}/token\",service=\"fixture\""),
                )],
                "",
            )),
            ("GET", path) if path.starts_with("/token") => Some(response(
                "200 OK",
                &[],
                &format!("{{\"token\":\"{TOKEN}\"}}"),
            )),
            ("HEAD", "/v2/acme/web/manifests/1.0")
                if seen.authorization.as_deref() == Some(&format!("Bearer {TOKEN}")) =>
            {
                Some(response(
                    "200 OK",
                    &[
                        ("Docker-Content-Digest", INDEX),
                        ("Content-Type", "application/vnd.oci.image.index.v1+json"),
                    ],
                    "",
                ))
            }
            _ => Some(response("401 Unauthorized", &[], "")),
        },
    )
    .await;
    let pinned = pin_on(&host, &format!("{host}/acme/web:1.0"))
        .await
        .unwrap();
    assert_eq!(pinned.as_str(), format!("{host}/acme/web:1.0@{INDEX}"));
    assert!(content_addressed(pinned.as_str()));
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|seen| seen.method == "HEAD" && seen.path == "/v2/acme/web/manifests/1.0"),
        "the lookup reads the manifest for the tag, not a layer"
    );
}

#[tokio::test]
async fn a_repository_that_wants_credentials_is_not_pinned() {
    let (host, _) = registry(|seen, own| match seen.path.as_str() {
        "/v2/" => Some(response(
            "401 Unauthorized",
            &[(
                "WWW-Authenticate",
                &format!("Bearer realm=\"http://{own}/token\",service=\"fixture\""),
            )],
            "",
        )),
        path if path.starts_with("/token") => Some(response(
            "401 Unauthorized",
            &[],
            "{\"errors\":[{\"code\":\"UNAUTHORIZED\"}]}",
        )),
        _ => Some(response("401 Unauthorized", &[], "")),
    })
    .await;
    let error = pin_on(&host, &format!("{host}/acme/private:1.0"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, PinError::NeedsCredentials { .. }),
        "{error:?}"
    );
    assert_eq!(error.registry(), Some(host.as_str()));
}

#[tokio::test]
async fn a_forbidden_manifest_is_not_pinned() {
    let (host, _) = registry(|seen, _| match seen.path.as_str() {
        "/v2/" => Some(response("200 OK", &[], "")),
        _ => Some(response("403 Forbidden", &[], "")),
    })
    .await;
    let error = pin_on(&host, &format!("{host}/acme/private:1.0"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, PinError::NeedsCredentials { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_missing_tag_is_unread_not_a_credentials_refusal() {
    let (host, _) = registry(|seen, _| match seen.path.as_str() {
        "/v2/" => Some(response("200 OK", &[], "")),
        _ => Some(response("404 Not Found", &[], "")),
    })
    .await;
    let error = pin_on(&host, &format!("{host}/acme/web:nope"))
        .await
        .unwrap_err();
    assert!(matches!(error, PinError::Unread { .. }), "{error:?}");
    assert!(std::error::Error::source(&error).is_some());
}

#[tokio::test(start_paused = true)]
async fn a_registry_that_never_answers_times_out() {
    let (host, _) = registry(|_, _| None).await;
    let error = pin_on(&host, &format!("{host}/acme/web:1.0"))
        .await
        .unwrap_err();
    assert!(matches!(error, PinError::TimedOut { .. }), "{error:?}");
    assert_eq!(error.registry(), Some(host.as_str()));
}

#[tokio::test]
async fn a_reference_that_is_not_a_tag_is_not_looked_up() {
    let (host, seen) = registry(|_, _| Some(response("200 OK", &[], ""))).await;
    for reference in [
        format!("{host}/acme/web@{INDEX}"),
        format!("{host}/acme/web:1.0@{INDEX}"),
        "Not A Reference".to_owned(),
    ] {
        let error = pin_on(&host, &reference).await.unwrap_err();
        assert!(matches!(error, PinError::NotATag { .. }), "{reference}");
    }
    assert!(seen.lock().unwrap().is_empty());
}
