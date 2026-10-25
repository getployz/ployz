//! Secrets in a Standalone Cluster: with no Cloud, the CLI seals to the Sealing Key its
//! Servers hold, and reseals what a CLI before 0.3.5 sealed with the Store's own key.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ployz_core::config::{
    ClientSealedValue, EncryptedSecretValue, SavedEnvironmentIntent, SavedVariableIntent,
    SavedVariableValue, SecretCiphertext,
};
use ployz_core::{Keypair, SealedIdentity, Sealing};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::digest::{SHA256, digest};
use tokio::io::AsyncWriteExt as _;

use super::*;

/// The in-process Store's key file secret, as an earlier CLI made it.
const STORE_SECRET: &[u8] = b"standalone-store-secret";

/// `ployz --json ARGS` with `stdin`, reaching the Server at `address`, with its own home
/// and the in-process Store in `store`: whether it succeeded, and what it printed.
async fn ployz(
    store: &Path,
    address: std::net::SocketAddr,
    args: &[&str],
    stdin: &str,
) -> (bool, Value) {
    let home = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .current_dir(home.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env("PLOYZ_CONFIG", home.path().join("config.yaml"))
        .env("PLOYZ_STORE", format!("sqlite:{}", store.display()))
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV")
        .arg("--json")
        .arg("--connect")
        .arg(format!("tcp://{address}"))
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(stdin.as_bytes()).await.unwrap();
    drop(input);
    let output = child.wait_with_output().await.unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let printed = serde_json::from_str(&stdout)
        .or_else(|_| serde_json::from_str(&stderr))
        .unwrap_or_else(|_| json!({ "stdout": stdout, "stderr": stderr }));
    (output.status.success(), printed)
}

/// A Server that holds `key`'s public half, or none when `key` is None.
async fn server(
    key: Option<&Keypair>,
) -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let service = DiscoveryService::new(test_description());
    let mut frame = empty_watch_frame();
    let mut one = machine('a', "one");
    one.machine.runtime.sealing = key.map(|key| Sealing::Held {
        public_key: key.public(),
    });
    frame.machines.push(one);
    service.emit_watch_frame_on_open(frame);
    serve_discovery(service).await
}

/// Project `shop` with Service `web`, in a Store whose key file holds [`STORE_SECRET`].
async fn shop(store: &Path, address: std::net::SocketAddr) {
    std::fs::write(store.with_extension("db.key"), STORE_SECRET).unwrap();
    for args in [
        &["project", "new", "shop"][..],
        &["service", "add", "web", "--image", "nginx:1"],
    ] {
        let (ok, printed) = ployz(store, address, args, "").await;
        assert!(ok, "{printed}");
    }
}

fn working(store: &Path) -> (String, SavedEnvironmentIntent) {
    let db = rusqlite::Connection::open(store).unwrap();
    let (id, working): (String, String) = db
        .query_row("SELECT id, working FROM config_environment", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    (id, serde_json::from_str(&working).unwrap())
}

/// Plant `web.env.KEY` sealed with the Store's own key, as a CLI before 0.3.5 set it.
fn plant_legacy(store: &Path, key: &str, plaintext: &str) {
    let cipher = digest(&SHA256, STORE_SECRET);
    let cipher = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, cipher.as_ref()).unwrap());
    let iv = [7; 12];
    let mut sealed = plaintext.as_bytes().to_vec();
    let tag = cipher
        .seal_in_place_separate_tag(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut sealed)
        .unwrap();
    let legacy = EncryptedSecretValue {
        version: 1,
        iv: STANDARD.encode(iv),
        tag: STANDARD.encode(tag.as_ref()),
        ciphertext: STANDARD.encode(sealed),
    };
    let (id, mut intent) = working(store);
    intent
        .services
        .first_mut()
        .unwrap()
        .variables
        .push(SavedVariableIntent {
            id: uuid::Uuid::new_v4().to_string(),
            key: key.into(),
            description: None,
            exported: false,
            value_fingerprint: "v1:legacy".into(),
            value: SavedVariableValue::Secret {
                encrypted_value: Some(SecretCiphertext::Legacy(legacy)),
            },
        });
    let db = rusqlite::Connection::open(store).unwrap();
    db.execute(
        "UPDATE config_environment SET working = ?1 WHERE id = ?2",
        (serde_json::to_string(&intent).unwrap(), id),
    )
    .unwrap();
}

/// What `web.env.KEY` in Working State opens to with `key`; None when it is not
/// sealed for the Cluster or `key` doesn't open it.
fn opened(store: &Path, key: &Keypair, variable: &str) -> Option<String> {
    let (id, intent) = working(store);
    let web = intent.services.first().unwrap();
    let value = web.variables.iter().find(|found| found.key == variable)?;
    let SavedVariableValue::Secret {
        encrypted_value: Some(SecretCiphertext::ClientSealed(sealed)),
    } = &value.value
    else {
        return None;
    };
    let sealed: &ClientSealedValue = sealed;
    let identity = SealedIdentity {
        organization: "local".into(),
        environment: uuid::Uuid::parse_str(&id).unwrap(),
        service: uuid::Uuid::parse_str(&web.id).unwrap(),
    };
    key.open(&identity, sealed)
}

#[tokio::test]
async fn a_standalone_cli_seals_a_secret_to_the_key_its_servers_hold() {
    let key = Keypair::generate();
    let (address, server) = server(Some(&key)).await;
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    shop(&store, address).await;
    let args = ["set", "web.env.TOKEN", "--secret", "--project", "shop"];
    let (ok, printed) = ployz(&store, address, &args, "standalone-s3cr3t\n").await;
    assert!(ok, "{printed}");
    assert!(
        !printed.to_string().contains("standalone-s3cr3t"),
        "{printed}"
    );
    assert_eq!(
        opened(&store, &key, "TOKEN").as_deref(),
        Some("standalone-s3cr3t")
    );
    server.abort();
}

#[tokio::test]
async fn a_standalone_cli_refuses_a_secret_while_no_server_holds_a_key() {
    let (address, server) = server(None).await;
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    shop(&store, address).await;
    let args = ["set", "web.env.TOKEN", "--secret", "--project", "shop"];
    let (ok, printed) = ployz(&store, address, &args, "standalone-s3cr3t\n").await;
    assert!(!ok, "{printed}");
    assert_eq!(
        printed.pointer("/error/message"),
        Some(&json!("Sealed values need a server.")),
        "{printed}"
    );
    let (_, intent) = working(&store);
    assert!(
        intent.services.first().unwrap().variables.is_empty(),
        "{intent:?}"
    );
    server.abort();
}

/// A Cluster upgraded from 0.3.4 deploys the secrets its earlier CLI set: the Deploy
/// reseals them for the Cluster first, keeping each value.
#[tokio::test]
async fn a_standalone_deploy_reseals_secrets_an_earlier_cli_set() {
    let key = Keypair::generate();
    let (address, server) = server(Some(&key)).await;
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    shop(&store, address).await;
    plant_legacy(&store, "TOKEN", "from-0.3.4");
    let (_, printed) = ployz(
        &store,
        address,
        &["deploy", "--yes", "--project", "shop"],
        "",
    )
    .await;
    assert!(
        !printed.to_string().contains("reseal"),
        "the Deploy waited on a reseal: {printed}"
    );
    assert_eq!(opened(&store, &key, "TOKEN").as_deref(), Some("from-0.3.4"));
    server.abort();
}
