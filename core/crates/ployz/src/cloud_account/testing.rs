//! What the Cloud tests share: a fake Cloud, a stored sign-in and a token credential.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
};

use super::credential::{Credential, TokenFrom};
use crate::cloud_login::{CredentialStore, Secret};

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
            let (status, json) = reply(route, &String::from_utf8(body).unwrap());
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
