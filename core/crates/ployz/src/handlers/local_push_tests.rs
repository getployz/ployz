//! Rung 2: local push fan-out through Machine RPC, with Docker upload simulated.

use std::{fs, os::unix::fs::PermissionsExt, sync::Arc};

use crate::{
    connect::{BoxProxyStream, ConnectError, Connector, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
};

use super::support::*;

struct PushConnector;

#[tonic::async_trait]
impl Connector for PushConnector {
    async fn connect(
        &self,
        connection: &Connection,
    ) -> Result<tonic::transport::Channel, ConnectError> {
        SystemConnector::default().connect(connection).await
    }

    async fn dial_proxy(
        &self,
        _: &Connection,
        _: &str,
        _: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        // The fake Docker upload needs only a successful tunnel preflight.
        Ok(Box::new(tokio::io::duplex(1).0))
    }
}

#[test]
fn local_build_push_keeps_exact_content_after_the_published_tag_moves() {
    if std::env::var_os("PLOYZ_LOCAL_PUSH_RACE_TEST").is_none() {
        let root = std::env::temp_dir().join(format!("ployz-push-race-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let docker = root.join("docker");
        fs::write(
            &docker,
            "#!/bin/sh\nif [ \"$1\" = info ]; then echo native; fi\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
        let mut paths = vec![root.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "handlers::deploy::remote_tests::local_push_tests::local_build_push_keeps_exact_content_after_the_published_tag_moves", "--nocapture"])
            .env("PLOYZ_LOCAL_PUSH_RACE_TEST", "1")
            .env("PATH", std::env::join_paths(paths).unwrap())
            .output()
            .unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for (published, repository) in [
            ("api", "api"),
            ("api:latest", "api"),
            ("team/api:v1", "team/api"),
            ("registry.invalid/api:v1", "registry.invalid/api"),
        ] {
            let exact = format!("sha256:{}", "1".repeat(64));
            let moved = format!("sha256:{}", "2".repeat(64));
            for platform in ["linux/amd64", "linux/arm64"] {
                let source = machine('a', "source");
                let mut destination = machine('b', "destination");
                destination.machine.runtime.architecture = "x86_64".into();
                let builds = Arc::new(BuildFixture::default());
                builds.stores.lock().unwrap().insert(
                    source.machine.id,
                    ployz_core::MachineImages {
                        containerd_store: true,
                        images: vec![
                            ployz_core::ImageSummary {
                                id: exact.clone(),
                                repo_tags: Vec::new(),
                                created: 0,
                                size: 1,
                                containers: 0,
                                platforms: vec![platform.into()],
                            },
                            ployz_core::ImageSummary {
                                id: moved.clone(),
                                repo_tags: vec![published.into()],
                                created: 0,
                                size: 1,
                                containers: 0,
                                platforms: vec!["linux/amd64".into()],
                            },
                        ],
                    },
                );
                let machines = vec![source.clone(), destination.clone()];
                let mut service = DeployService::new(source).with_machines(machines.clone());
                service.builds = Some(builds.clone());
                let (address, server) = listening(service).await;
                let mut client = connect_selected_with(
                    SelectedConnections {
                        source: ConnectionSource::Direct,
                        connections: vec![Connection::tcp(address)],
                    },
                    Arc::new(PushConnector),
                )
                .await
                .unwrap();
                let result = crate::image::push_using_machines(
                    &mut client,
                    crate::image::ImageContent::built(published, &exact),
                    None,
                    &[],
                    &machines,
                    &Default::default(),
                )
                .await
                .unwrap();
                let pulls = builds.pulls.lock().unwrap();
                if platform == "linux/amd64" {
                    assert!(result.all_targets_succeeded(), "{result:?}");
                    assert_eq!(pulls.len(), 1);
                    let (target, pull) = pulls.first().unwrap();
                    assert_eq!(*target, destination.machine.id);
                    assert_eq!(pull.pull.image(), format!("{repository}@{exact}"));
                    assert_eq!(pull.pull.tag(), Some(published));
                    assert_eq!(pull.platform, "linux/amd64");
                } else {
                    assert_eq!(result.successes.len(), 1);
                    assert_eq!(result.failures.len(), 1);
                    assert!(
                        pulls.is_empty(),
                        "the moved tag cannot supply the original build's missing variant"
                    );
                }
                server.abort();
            }
        }
    });
}
