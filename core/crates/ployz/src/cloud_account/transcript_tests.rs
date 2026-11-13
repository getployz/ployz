//! Every Cloud entry point answered by a canned Cloud, over the statuses and bodies
//! Cloud, a proxy or a broken deploy can send, for each credential: what the caller
//! gets, how it prints, which code and next step it exits with, and whether the reply's
//! manifest revision was kept. The transcript's digest is pinned, so a change to how
//! requests are sent or answers are read shows up as a different transcript.

use std::net::TcpListener;

use reqwest::Method;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::cloud_login::{CredentialStore, LoginError, Secret};
use crate::failure::Failure;

const STATUSES: [u16; 7] = [200, 401, 403, 404, 409, 413, 500];

/// The statuses and bodies the pinned digest covers; `PLOYZ_CLOUD_TRANSCRIPT_FULL` runs
/// them all, which takes minutes.
const PINNED_STATUSES: [u16; 5] = [200, 401, 403, 404, 500];
const PINNED_BODIES: [&str; 5] = ["object", "html", "refusal", "public-forbidden", "rich"];

const RICH: &str = r#"{"ok":true,"id":"run_1","organizations":[{"id":"o1","slug":"acme","name":"Acme","current":true}],"via":"agent","tokens":[],"devices":[],"revoking":[],"connections":[],"unreachable":[],"organization":{"id":"o1","slug":"acme","name":"Acme"},"removed":{"id":"t1","kind":"token"},"servers":{"confirmed":[],"unconfirmed":[]},"token":{"id":"t1","name":"ci","organization":"acme","expires_at":"2027-01-01","secret":"ployz_s","scope":"Full"}}"#;
const FORGET: &str =
    r#"{"organization":"acme","servers":[],"volumes":[],"cancelled":[],"removed":true}"#;
const REMOVAL: &str =
    r#"{"organization":"acme","removed":true,"servers":{"confirmed":[],"unconfirmed":[]}}"#;

const BODIES: [(&str, &str); 14] = [
    ("object", r#"{"ok":true}"#),
    ("empty", ""),
    ("text", "upstream down"),
    (
        "html",
        "<!DOCTYPE html><html><head><title>502 Bad Gateway</title></head></html>",
    ),
    (
        "refusal",
        r#"{"error":{"code":"conflict","message":"m","details":{"next":"ployz login"},"field":"f","expected":"e","writes":false}}"#,
    ),
    (
        "refusal-bare",
        r#"{"error":{"code":"not_found","message":"nf","details":null}}"#,
    ),
    (
        "public-not-found",
        r#"{"_tag":"PublicError","code":"NOT_FOUND","message":"gone"}"#,
    ),
    (
        "public-conflict",
        r#"{"_tag":"PublicError","code":"CONFLICT","message":"taken"}"#,
    ),
    (
        "public-forbidden",
        r#"{"_tag":"PublicError","code":"FORBIDDEN","message":"no"}"#,
    ),
    (
        "public-other",
        r#"{"_tag":"PublicError","code":"WAT","message":"w"}"#,
    ),
    ("rich", RICH),
    ("forget", FORGET),
    ("removal", REMOVAL),
    ("not-modified-shape", r#"{"state":"finished"}"#),
];

/// A Cloud on loopback that answers every request with one status and body and the
/// manifest revision `r<port>-<n>` for its n-th request, and closes the connection. With
/// no `answer` it takes the request and hangs up.
async fn canned(answer: Option<(u16, &'static str)>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let n = served.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::spawn(async move {
                let (read, mut write) = stream.split();
                let mut reader = tokio::io::BufReader::new(read);
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).await.unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                    if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:")
                    {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body).await;
                if let Some((status, body)) = answer {
                    let _ = write
                        .write_all(
                            format!(
                                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\nx-ployz-manifest: r{port}-{n}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                }
                let _ = write.shutdown().await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn refused_port() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

#[derive(Clone, Copy, Debug)]
enum Entry {
    CallGet,
    CallPost,
    Refusable,
    RefusableOperation,
    ConfigRead,
    ConfigWrite,
    ConfigIfRoutedRead,
    ConfigIfRoutedWrite,
    ServerAccess,
    Upload,
    KeyedReply,
    StartRun,
    FollowRun,
    Credentials,
    Organizations,
    ActingAs,
    UseOrganization,
    NewOrganization,
    RemoveToken,
    NewToken,
    CheckForget,
    ForgetServers,
    RemoveOrganization,
}

const ENTRIES: [Entry; 23] = [
    Entry::CallGet,
    Entry::CallPost,
    Entry::Refusable,
    Entry::RefusableOperation,
    Entry::ConfigRead,
    Entry::ConfigWrite,
    Entry::ConfigIfRoutedRead,
    Entry::ConfigIfRoutedWrite,
    Entry::ServerAccess,
    Entry::Upload,
    Entry::KeyedReply,
    Entry::StartRun,
    Entry::FollowRun,
    Entry::Credentials,
    Entry::Organizations,
    Entry::ActingAs,
    Entry::UseOrganization,
    Entry::NewOrganization,
    Entry::RemoveToken,
    Entry::NewToken,
    Entry::CheckForget,
    Entry::ForgetServers,
    Entry::RemoveOrganization,
];

fn exits(error: Failure) -> String {
    format!("exit {:?}", error.report())
}

fn login(error: LoginError) -> String {
    let described = format!(
        "{error:?} | {error} | code {:?}",
        crate::failure::login_code(&error)
    );
    format!("{described} | {}", exits(Failure::from(error)))
}

fn stopped(error: StoreCallError) -> String {
    match error {
        StoreCallError::Cloud(error) => format!("cloud: {}", login(error)),
        StoreCallError::Refused(refused) => format!(
            "refused: {refused:?} | {}",
            exits(Failure::from(StoreCallError::Refused(refused.clone())))
        ),
        StoreCallError::Stopped(failure) => format!("stopped: {}", exits(failure)),
    }
}

fn shown<T: std::fmt::Debug>(result: Result<T, StoreCallError>) -> String {
    match result {
        Ok(value) => format!("ok {value:?}"),
        Err(error) => stopped(error),
    }
}

fn shown_login<T: std::fmt::Debug>(result: Result<T, LoginError>) -> String {
    match result {
        Ok(value) => format!("ok {value:?}"),
        Err(error) => format!("login: {}", login(error)),
    }
}

async fn run(entry: Entry, credential: &Credential, store: &CredentialStore) -> String {
    let body = serde_json::json!({ "a": 1 });
    match entry {
        Entry::CallGet => shown_login(
            call::<serde_json::Value>(credential, Method::GET, "organizations", None).await,
        ),
        Entry::CallPost => shown_login(
            call::<serde_json::Value>(credential, Method::POST, "tokens", Some(&body)).await,
        ),
        Entry::Refusable => shown(
            refusable::<serde_json::Value>(credential, Method::GET, "github/tree", None).await,
        ),
        Entry::RefusableOperation => shown(
            refusable_operation::<serde_json::Value>(
                credential,
                Method::GET,
                "namespaces/app/clean",
                None,
            )
            .await,
        ),
        Entry::ConfigRead => shown(
            config_store::<serde_json::Value>(credential, Via::Cli, "read", &body, None).await,
        ),
        Entry::ConfigWrite => shown(
            config_store::<serde_json::Value>(credential, Via::Agent, "write", &body, Some("ap1"))
                .await,
        ),
        Entry::ConfigIfRoutedRead => shown(
            config_store_if_routed::<serde_json::Value>(credential, Via::Cli, "read", &body).await,
        ),
        Entry::ConfigIfRoutedWrite => shown(
            config_store_if_routed::<serde_json::Value>(credential, Via::Agent, "write", &body)
                .await,
        ),
        Entry::ServerAccess => shown(server_access(credential).await),
        Entry::Upload => shown(upload(credential, "d1", b"source".to_vec()).await),
        Entry::KeyedReply => {
            let (result, revision) = keyed_reply::<serde_json::Value>(
                credential,
                Via::Agent,
                Method::POST,
                "servers/x/forget",
                &body,
                Some("ap1"),
                Some("key1"),
            )
            .await;
            format!("{} | revision {revision:?}", shown(result))
        }
        Entry::StartRun => {
            shown(start_run(credential, Method::DELETE, "servers/x", &body, Some("ap1")).await)
        }
        Entry::FollowRun => shown(
            follow_run::<Settled<serde_json::Value>>(
                credential,
                "server-drains/d1",
                "Draining",
                Some(tokio::time::Instant::now()),
            )
            .await
            .map(|settled| settled.map(|settled| format!("{settled:?}"))),
        ),
        Entry::Credentials => shown_login(credentials(credential).await),
        Entry::Organizations => shown_login(organizations(credential).await),
        Entry::ActingAs => shown_login(
            acting_as(credential)
                .await
                .map(|acting| (acting.organization, acting.agent)),
        ),
        Entry::UseOrganization => {
            shown_login(use_organization(store, credential, Target::Slug("beta")).await)
        }
        Entry::NewOrganization => shown_login(
            new_organization(store, credential, "Acme", Some("acme"))
                .await
                .map(|made| made.slug),
        ),
        Entry::RemoveToken => shown_login(remove_token(credential, "t1").await),
        Entry::NewToken => shown_login(
            new_token(credential, "ci", TokenKind::Cli, &TokenPreset::Read, 30)
                .await
                .map(|made| (made.id, made.secret, made.scope)),
        ),
        Entry::CheckForget => shown(check_forget_servers(credential).await),
        Entry::ForgetServers => shown(forget_servers(credential, "acme", Some("ap1")).await),
        Entry::RemoveOrganization => {
            shown(remove_organization(credential, "o1", Some("ap1")).await)
        }
    }
}

fn normalized(text: String, cloud: &str, port: &str) -> String {
    text.replace(cloud, "CLOUD").replace(port, "PORT")
}

async fn credentials_for(
    cloud: &str,
    dir: &std::path::Path,
) -> (CredentialStore, Vec<(&'static str, Credential)>) {
    let store = CredentialStore::beside(&dir.join("config.yaml"));
    let stored = serde_json::json!({
        "state": "signed_in",
        "cloud": cloud,
        "token": "session-token",
        "account": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
        "organization": { "id": "o1", "slug": "acme" },
    });
    std::fs::write(dir.join("cloud.json"), stored.to_string()).unwrap();
    let device = credential(&store, None, None).await.unwrap();
    let from = |from| Credential::Token {
        cloud: cloud.to_owned(),
        token: Secret::new("ployz_secret".to_owned()),
        from,
    };
    (
        store,
        vec![
            ("device", device),
            ("token", from(TokenFrom::Env)),
            ("agent-token", from(TokenFrom::McpLogin)),
        ],
    )
}

async fn transcript() -> String {
    let mut lines = Vec::new();
    let full = std::env::var_os("PLOYZ_CLOUD_TRANSCRIPT_FULL").is_some();
    for status in STATUSES {
        for (label, body) in BODIES {
            if !full && !(PINNED_STATUSES.contains(&status) && PINNED_BODIES.contains(&label)) {
                continue;
            }
            let cloud = canned(Some((status, body))).await;
            let port = cloud.rsplit(':').next().unwrap().to_owned();
            let dir = tempfile::tempdir().unwrap();
            let (store, credentials) = credentials_for(&cloud, dir.path()).await;
            for entry in ENTRIES {
                for (kind, credential) in &credentials {
                    if !full && *kind == "agent-token" && status != 401 {
                        continue;
                    }
                    let before = revision_seen(&cloud);
                    let answer = run(entry, credential, &store).await;
                    let noticed = revision_seen(&cloud) != before;
                    lines.push(normalized(
                        format!(
                            "{entry:?} | {kind} | {status} {label} => {answer} | noticed {noticed}"
                        ),
                        &cloud,
                        &port,
                    ));
                }
            }
        }
    }
    for label in ["refused", "hangs-up"] {
        let cloud = match label {
            "refused" => refused_port(),
            _ => canned(None).await,
        };
        let port = cloud.rsplit(':').next().unwrap().to_owned();
        let dir = tempfile::tempdir().unwrap();
        let (store, credentials) = credentials_for(&cloud, dir.path()).await;
        for entry in ENTRIES {
            for (kind, credential) in credentials.iter().take(2) {
                let answer = run(entry, credential, &store).await;
                lines.push(normalized(
                    format!("{entry:?} | {kind} | {label} => {answer}"),
                    &cloud,
                    &port,
                ));
            }
        }
    }
    lines.join("\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn cloud_answers_read_the_same_through_every_entry_point() {
    let transcript = transcript().await;
    if let Ok(path) = std::env::var("PLOYZ_CLOUD_TRANSCRIPT") {
        std::fs::write(path, &transcript).unwrap();
    }
    let digest = hex::encode(Sha256::digest(transcript.as_bytes()));
    if std::env::var_os("PLOYZ_CLOUD_TRANSCRIPT_FULL").is_some() {
        return;
    }
    assert_eq!(
        (transcript.lines().count(), digest.as_str()),
        (
            1357,
            "a3cddbae196f155ee8ac1edf2e89ea30de521c18516b32c31df5002b04d3360e"
        ),
        "the transcript changed"
    );
}
