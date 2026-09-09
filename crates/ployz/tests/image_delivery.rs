//! Rung 2: local push keeps its exact identity when the first Machine serves peers.
use std::{fs, os::unix::fs::PermissionsExt, process::Command, sync::Arc};

use ployz::{
    connect::{BoxProxyStream, ConnectError, Connector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
    image::{ImageContent, push},
};
use ployz_core::{ImageSummary, MachineImages};
use tonic::transport::{Channel, Endpoint};

#[path = "deploy_client/support.rs"]
#[allow(dead_code)]
mod support;
use support::{BuildFixture, DeployService, listening, machine};

struct UnusedImageTransport(Channel);

#[tonic::async_trait]
impl Connector for UnusedImageTransport {
    async fn connect(&self, _: &Connection) -> Result<Channel, ConnectError> {
        Ok(self.0.clone())
    }

    async fn dial_proxy(
        &self,
        _: &Connection,
        _: &str,
        _: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        // Docker is a stand-in: this test checks selection and RPCs, not image bytes.
        Ok(Box::new(tokio::io::duplex(1).0))
    }
}

#[tokio::test]
async fn local_push_selects_peer_variants_by_the_original_digest() {
    const CHILD: &str = "PLOYZ_PEER_DELIVERY_TEST";
    if std::env::var_os(CHILD).is_none() {
        let root = tempfile::tempdir().unwrap();
        let docker = root.path().join("docker");
        fs::write(
            &docker,
            "#!/bin/sh\ncase \"$1 $2\" in\n  'info --format') echo orbstack ;;\n  'image inspect'|'image rm'|'tag '*|'push '*) ;;\n  *) exit 1 ;;\nesac\n",
        )
        .unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
        // Re-exec isolates PATH from concurrently running tests.
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "local_push_selects_peer_variants_by_the_original_digest",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("PATH", root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    let mut source = machine('a', "source");
    let mut peer = machine('b', "peer");
    source.machine.runtime.architecture = "x86_64".into();
    peer.machine.runtime.architecture = "x86_64".into();
    let tag = "registry.invalid/api:latest";
    let digest = format!("sha256:{}", "1".repeat(64));
    let builds = Arc::new(BuildFixture::default());
    let summary = |id: String, tags: Vec<String>, platform: &str| ImageSummary {
        id,
        repo_tags: tags,
        created: 0,
        size: 1,
        containers: 0,
        platforms: vec![platform.into()],
    };
    // The source still holds this Build's AMD64 content, but its tag now names
    // an ARM64 replacement. Selecting by the tag would incorrectly reject the peer.
    builds.stores.lock().unwrap().insert(
        source.machine.id,
        MachineImages {
            containerd_store: true,
            images: vec![
                summary(digest.clone(), vec![], "linux/amd64"),
                summary(
                    format!("sha256:{}", "2".repeat(64)),
                    vec![tag.into()],
                    "linux/arm64",
                ),
            ],
        },
    );
    let mut service =
        DeployService::new(source.clone()).with_machines(vec![source.clone(), peer.clone()]);
    service.builds = Some(builds.clone());
    let (address, server) = listening(service).await;
    let channel = Endpoint::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address)],
        },
        Arc::new(UnusedImageTransport(channel)),
    )
    .await
    .unwrap();
    let result = push(
        &mut client,
        ImageContent::built(tag, &digest),
        None,
        &[source.machine.id.to_string(), peer.machine.id.to_string()],
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(result.failures.is_empty(), "{:?}", result.failures);
    assert_eq!(result.successes.len(), 2);
    let pulls = builds.pulls.lock().unwrap();
    let [(machine_id, pull)] = pulls.as_slice() else {
        panic!("expected one peer delivery, got {pulls:?}");
    };
    assert_eq!(*machine_id, peer.machine.id);
    assert_eq!(pull.platform, "linux/amd64");
    assert_eq!(pull.image, tag);
    server.abort();
}
