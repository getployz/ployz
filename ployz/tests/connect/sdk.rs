//! Façade tests for Cloud session connect / about / close.

use std::{path::PathBuf, process::Command, time::Duration};

use ployz::sdk;
use ployz_core::{
    CapabilityName, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, MachineId, PROTOCOL_MAJOR,
    RpcErrorCode,
};
use tokio::time::timeout;

use super::relay::{self, RelaySession};
use super::support::DiscoveryService;

#[tokio::test]
async fn connect_about_returns_contract_and_branches_on_capability_names() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;

    let client = timeout(
        Duration::from_secs(5),
        sdk::connect(&session.url, relay::DIAL, description.machine_id.as_str()),
    )
    .await
    .expect("connect must not hang")
    .unwrap();

    let about = client.about().await.unwrap();
    assert_eq!(about.machine_id, description.machine_id);
    assert!(
        about.supports(DESCRIBE_CONTRACT_CAPABILITY),
        "callers branch on capability names, not daemon_version"
    );
    assert_eq!(about.daemon_version, description.daemon_version);
}

#[tokio::test]
async fn bad_credentials_and_unknown_machines_reject_with_typed_errors() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let machine_id = description.machine_id.as_str();

    let empty = timeout(
        Duration::from_secs(2),
        sdk::connect(&session.url, "", machine_id),
    )
    .await
    .expect("empty Dial Credential must not hang");
    let empty = match empty {
        Ok(_) => panic!("expected empty Dial Credential to fail"),
        Err(error) => error,
    };
    assert_eq!(empty.code, RpcErrorCode::Unauthenticated);

    let bad = timeout(
        Duration::from_secs(2),
        sdk::connect(&session.url, "wrong-secret", machine_id),
    )
    .await
    .expect("bad Dial Credential must not hang");
    let bad = match bad {
        Ok(_) => panic!("expected invalid Dial Credential to fail"),
        Err(error) => error,
    };
    assert_eq!(bad.code, RpcErrorCode::Unauthenticated);

    let unknown = timeout(
        Duration::from_secs(2),
        sdk::connect(&session.url, relay::DIAL, MachineId::random().as_str()),
    )
    .await
    .expect("unknown Machine ID must not hang");
    let unknown = match unknown {
        Ok(_) => panic!("expected unknown Machine ID to fail"),
        Err(error) => error,
    };
    assert_eq!(unknown.code, RpcErrorCode::NotFound);

    let invalid = timeout(
        Duration::from_secs(2),
        sdk::connect(&session.url, relay::DIAL, "not-a-machine-id"),
    )
    .await
    .expect("invalid Machine ID must not hang");
    let invalid = match invalid {
        Ok(_) => panic!("expected invalid Machine ID to fail"),
        Err(error) => error,
    };
    assert_eq!(invalid.code, RpcErrorCode::InvalidArgument);
}

#[tokio::test]
async fn close_drops_the_session_and_repeated_lifecycle_works() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let machine_id = description.machine_id.as_str();

    for _ in 0..3 {
        let client = sdk::connect(&session.url, relay::DIAL, machine_id)
            .await
            .unwrap();
        assert!(
            client
                .about()
                .await
                .unwrap()
                .supports(DESCRIBE_CONTRACT_CAPABILITY)
        );
        client.close().await;
        let closed = match client.about().await {
            Ok(_) => panic!("about() after close must fail"),
            Err(error) => error,
        };
        assert_eq!(closed.code, RpcErrorCode::Unavailable);
        client.close().await;
    }
}

#[tokio::test]
async fn node_smoke_covers_connect_about_error_and_close() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let addon = native_addon();
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ployz-sdk");
    let script = package.join("tests/node_smoke.js");
    let url = session.url.clone();
    let machine_id = description.machine_id.as_str().to_owned();
    let unknown = MachineId::random().as_str().to_owned();

    let output = timeout(
        Duration::from_secs(20),
        tokio::task::spawn_blocking(move || {
            Command::new("node")
                .arg(&script)
                .env("PLOYZ_SDK_ADDON", addon)
                .env("PLOYZ_SDK_PACKAGE", package)
                .env("PLOYZ_RELAY_URL", url)
                .env("PLOYZ_BEARER", relay::DIAL)
                .env("PLOYZ_MACHINE_ID", machine_id)
                .env("PLOYZ_UNKNOWN_MACHINE_ID", unknown)
                .output()
        }),
    )
    .await
    .expect("Node smoke must not hang")
    .expect("Node smoke task joins")
    .expect("Node smoke spawns");

    assert!(
        output.status.success(),
        "Node smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn advertised_description() -> ContractDescription {
    ContractDescription {
        machine_id: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "do-not-branch-on-me".into(),
        capabilities: [CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY)
            .expect("catalogued capability names are valid")]
        .into(),
    }
}

fn native_addon() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.join("..");
    let target = option_env!("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let names = ["libployz_sdk.so", "libployz_sdk.dylib", "ployz_sdk.dll"];
    for name in names {
        let path = target.join(profile).join(name);
        if path.is_file() {
            return path;
        }
    }
    let status = Command::new("cargo")
        .args(["build", "-p", "ployz-sdk", "--locked"])
        .current_dir(&workspace)
        .status()
        .expect("cargo build -p ployz-sdk");
    assert!(status.success(), "cargo build -p ployz-sdk failed");
    for name in names {
        let path = target.join(profile).join(name);
        if path.is_file() {
            return path;
        }
    }
    panic!(
        "ployz-sdk cdylib was not produced under {}",
        target.join(profile).display()
    );
}
