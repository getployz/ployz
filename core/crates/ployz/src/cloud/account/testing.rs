//! What the Cloud tests share: a fake Cloud, a stored sign-in and a token credential.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
};

use super::credential::{Credential, TokenFrom};
use crate::cloud::login::{CredentialStore, LoginError, Secret};
use crate::cloud::origin::CloudOrigin;

/// A fake Cloud on loopback: `reply("POST /api/auth/device/token", body)` → (status, JSON).
pub(crate) fn fake_cloud(
    reply: impl Fn(&str, &str) -> (u16, serde_json::Value) + Send + 'static,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let route = request_line.rsplit_once(' ').unwrap().0;
            let (status, json) = reply(route, &String::from_utf8_lossy(&body));
            let json = json.to_string();
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
                json.len()
            )
            .unwrap();
        }
    });
    url
}

/// A device signed in to `cloud` as Organization `acme`.
pub(crate) fn signed_in_store(cloud: &str) -> (tempfile::TempDir, CredentialStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::beside(&dir.path().join("config.yaml"));
    let stored = serde_json::json!({
        "state": "signed_in",
        "cloud": cloud,
        "token": "session-token",
        "account": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
        "organization": { "id": "o1", "slug": "acme" },
    });
    std::fs::write(dir.path().join("cloud.json"), stored.to_string()).unwrap();
    (dir, store)
}

pub(crate) fn token(cloud: &str) -> Credential {
    Credential::Token {
        cloud: cloud.to_owned(),
        token: Secret::new("ployz_secret".to_owned()),
        from: TokenFrom::Env,
    }
}

/// One of every [`LoginError`], named by its variant, with an in-flight failure that never
/// connected standing in for both transport variants.
pub(crate) fn every_login_error(status: u16) -> Vec<(&'static str, LoginError)> {
    let transport = || reqwest::Client::new().get("not a url").build().unwrap_err();
    let path = || std::path::PathBuf::from("cloud.json");
    let cloud = || "https://cloud.test".to_owned();
    vec![
        (
            "Unreachable",
            LoginError::Unreachable {
                cloud: CloudOrigin::new(&cloud()).shown(),
                error: transport(),
            },
        ),
        (
            "Unanswered",
            LoginError::Unanswered {
                cloud: CloudOrigin::new(&cloud()).shown(),
                error: transport(),
            },
        ),
        (
            "Unsupported",
            LoginError::Unsupported(CloudOrigin::new(&cloud()).shown()),
        ),
        (
            "Status",
            LoginError::Status {
                status,
                body: String::new(),
            },
        ),
        ("Reply", LoginError::Reply("odd".to_owned())),
        (
            "Store",
            LoginError::Store {
                path: path(),
                source: std::io::Error::other("disk"),
            },
        ),
        (
            "Corrupt",
            LoginError::Corrupt {
                path: path(),
                source: serde_json::from_str::<()>("{").unwrap_err(),
            },
        ),
        (
            "TokenCloudUnreadable",
            LoginError::TokenCloudUnreadable(path()),
        ),
        (
            "OtherCloud",
            LoginError::OtherCloud {
                signed_in: CloudOrigin::new(&cloud()).shown(),
                requested: CloudOrigin::new("https://other.test").shown(),
            },
        ),
        ("SignedOut", LoginError::SignedOut),
        ("Expired", LoginError::Expired),
        ("Denied", LoginError::Denied),
        ("Ended", LoginError::Ended),
        (
            "AwaitingApproval",
            LoginError::AwaitingApproval {
                url: "https://cloud.test/device".to_owned(),
            },
        ),
        ("TokenRefused", LoginError::TokenRefused),
        ("AgentTokenRefused", LoginError::AgentTokenRefused),
        ("TokenMalformed", LoginError::TokenMalformed),
        ("NotMember", LoginError::NotMember("acme".to_owned())),
        ("TokenBound", LoginError::TokenBound),
        (
            "UnknownOrganization",
            LoginError::UnknownOrganization("acme".to_owned()),
        ),
        (
            "UnknownCredential",
            LoginError::UnknownCredential("t1".to_owned()),
        ),
        (
            "AmbiguousCredential",
            LoginError::AmbiguousCredential {
                name: "ci".to_owned(),
                ids: vec!["t1".to_owned(), "t2".to_owned()],
            },
        ),
    ]
}
