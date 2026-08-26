//! Tests for the Machine RPC boundary.

use super::{MachineService, local_error, store_error};
use crate::corrosion::{AdminClient, fake_cluster};
use crate::machine::{LocalMachineError, LocalMachineStore, StoreError};
use ployz_core::{
    AdvertisedEndpoint, ContainerKind, GetIngressProxyConfigRequest, IngressProxyBackend,
    IngressProxyConfig, MachineName, MachineRpc, ProjectName, RpcErrorCode, RpcResponseBody,
    RuntimeWatchRequest, op,
};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use tonic::{Code, Request};

#[test]
fn non_participating_update_is_a_typed_conflict() {
    assert_eq!(
        store_error(StoreError::NotParticipating).code,
        RpcErrorCode::Conflict
    );
}

#[test]
fn allocator_not_quiet_is_retryable_unavailable() {
    let RpcResponseBody::Error(error) = local_error(LocalMachineError::AllocatorNotQuiet)
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .body
    else {
        panic!("expected error payload");
    };
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "Allocator is not quiet");
}

#[test]
fn not_allocator_does_not_allocate() {
    let RpcResponseBody::Error(error) = local_error(LocalMachineError::NotAllocator)
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .body
    else {
        panic!("expected error payload");
    };
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "this Machine is not the Allocator");
}

#[tokio::test]
async fn zentinel_service_is_bootstrapped_before_host_network_is_returned() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-zentinel-network-{}",
        ployz_core::MachineId::random()
    ));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .initialize(
            MachineName::parse("edge").unwrap(),
            crate::machine::FoundingCluster {
                network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: IngressProxyBackend::Zentinel,
            },
            None,
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            None,
            None,
        )
        .unwrap();
    let store = Arc::new(Mutex::new(store));
    let (replicated, server) =
        fake_cluster::store_with_ingress_proxy_backend_value("zentinel").await;
    let service = MachineService::with_cluster(
        store,
        watch::channel(false).0,
        Some((replicated, AdminClient::new("/no/such/admin.sock"))),
    );
    let spec = IngressProxyBackend::Zentinel
        .requested_service_spec("example.test/zentinel".into(), Vec::new(), None)
        .unwrap()
        .to_resolved(
            ployz_core::ServiceId::parse("a".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig {
                order: ployz_core::UpdateOrder::StopFirst,
                monitor_millis: None,
            },
        );

    assert!(matches!(
        service
            .local
            .prepare_service_runtime(ContainerKind::PreDeployHook, &ProjectName::system(), &spec,)
            .await
            .unwrap(),
        crate::docker::NetworkAttachment::Bridge
    ));
    let config_file = crate::ingress::zentinel::config_path(&data_dir);
    assert!(!config_file.exists());

    assert!(matches!(
        service
            .local
            .prepare_service_runtime(
                ContainerKind::ServiceContainer,
                &ProjectName::system(),
                &spec
            )
            .await
            .unwrap(),
        crate::docker::NetworkAttachment::Host
    ));
    let config = std::fs::read_to_string(&config_file).unwrap();
    assert!(config.contains("schema-version \"1.0\""));
    assert!(config.contains(crate::ingress::zentinel::ADMIN_ADDRESS));
    std::fs::write(&config_file, "authoritative\n").unwrap();
    service
        .local
        .prepare_service_runtime(
            ContainerKind::ServiceContainer,
            &ProjectName::system(),
            &spec,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&config_file).unwrap(),
        "authoritative\n"
    );
    std::fs::remove_dir_all(data_dir.join("ingress")).unwrap();
    std::fs::write(data_dir.join("ingress"), "not a directory").unwrap();
    assert!(matches!(
        service
            .local
            .prepare_service_runtime(
                ContainerKind::ServiceContainer,
                &ProjectName::system(),
                &spec,
            )
            .await,
        Err(crate::machine::LocalMachineError::IngressRuntime(_))
    ));
    server.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn envoy_service_is_bootstrapped_before_bridge_network_is_returned() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-envoy-network-{}",
        ployz_core::MachineId::random()
    ));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .initialize(
            MachineName::parse("edge").unwrap(),
            crate::machine::FoundingCluster {
                network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: IngressProxyBackend::Envoy,
            },
            None,
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            None,
            None,
        )
        .unwrap();
    let store = Arc::new(Mutex::new(store));
    let (replicated, server) = fake_cluster::store_with_ingress_proxy_backend_value("envoy").await;
    let service = MachineService::with_cluster(
        store,
        watch::channel(false).0,
        Some((replicated, AdminClient::new("/no/such/admin.sock"))),
    );
    let spec = IngressProxyBackend::Envoy
        .requested_service_spec("example.test/envoy".into(), Vec::new(), None)
        .unwrap()
        .to_resolved(
            ployz_core::ServiceId::parse("a".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig::default(),
        );

    assert!(matches!(
        service
            .local
            .prepare_service_runtime(ContainerKind::PreDeployHook, &ProjectName::system(), &spec,)
            .await
            .unwrap(),
        crate::docker::NetworkAttachment::Bridge
    ));
    let config_file = crate::ingress::envoy::config_path(&data_dir);
    assert!(!config_file.exists());

    assert!(matches!(
        service
            .local
            .prepare_service_runtime(
                ContainerKind::ServiceContainer,
                &ProjectName::system(),
                &spec
            )
            .await
            .unwrap(),
        crate::docker::NetworkAttachment::Bridge
    ));
    let config = std::fs::read_to_string(&config_file).unwrap();
    assert_eq!(config, crate::ingress::envoy::BOOTSTRAP);
    assert!(!config.contains("admin:"));
    assert!(config.contains("path_config_source:"));
    let envoy_dir = config_file.parent().unwrap();
    assert!(
        std::fs::read_to_string(envoy_dir.join("rds.yaml"))
            .unwrap()
            .contains("/.ployz-verify")
    );
    assert!(envoy_dir.join("lds.yaml").exists());
    assert!(envoy_dir.join("cds.yaml").exists());
    assert!(envoy_dir.join("sds.yaml").exists());
    std::fs::write(&config_file, "authoritative\n").unwrap();
    service
        .local
        .prepare_service_runtime(
            ContainerKind::ServiceContainer,
            &ProjectName::system(),
            &spec,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&config_file).unwrap(),
        "authoritative\n"
    );
    server.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn ingress_config_rpc_returns_only_the_selected_backend_file() {
    for (backend, expected) in [
        (
            IngressProxyBackend::Caddy,
            IngressProxyConfig::Caddy("caddy exact\n".into()),
        ),
        (
            IngressProxyBackend::Zentinel,
            IngressProxyConfig::Zentinel("zentinel exact\n".into()),
        ),
        (
            IngressProxyBackend::Envoy,
            IngressProxyConfig::Envoy(
                "envoy-lds\n\n---\nenvoy-rds\n\n---\nenvoy-cds\n\n---\nenvoy-sds\n".into(),
            ),
        ),
    ] {
        let data_dir = std::env::temp_dir().join(format!(
            "ployzd-ingress-config-{}",
            ployz_core::MachineId::random()
        ));
        let mut local = LocalMachineStore::open(&data_dir).unwrap();
        local
            .initialize(
                MachineName::parse("edge").unwrap(),
                crate::machine::FoundingCluster {
                    network: "10.210.0.0/16".parse().unwrap(),
                    ingress_proxy_backend: backend,
                },
                None,
                vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
                None,
                None,
            )
            .unwrap();
        let (replicated, server) =
            fake_cluster::store_with_ingress_proxy_backend_value(backend.as_str()).await;
        let caddy = crate::ingress::caddy::config_path(&data_dir);
        let zentinel = crate::ingress::zentinel::config_path(&data_dir);
        let envoy = crate::ingress::envoy::config_path(&data_dir);
        std::fs::create_dir_all(caddy.parent().unwrap()).unwrap();
        std::fs::create_dir_all(zentinel.parent().unwrap()).unwrap();
        std::fs::create_dir_all(envoy.parent().unwrap()).unwrap();
        std::fs::write(&caddy, "caddy exact\n").unwrap();
        std::fs::write(&zentinel, "zentinel exact\n").unwrap();
        std::fs::write(envoy.parent().unwrap().join("lds.yaml"), "envoy-lds\n").unwrap();
        std::fs::write(envoy.parent().unwrap().join("rds.yaml"), "envoy-rds\n").unwrap();
        std::fs::write(envoy.parent().unwrap().join("cds.yaml"), "envoy-cds\n").unwrap();
        std::fs::write(envoy.parent().unwrap().join("sds.yaml"), "envoy-sds\n").unwrap();
        let service = MachineService::with_cluster(
            Arc::new(Mutex::new(local)),
            watch::channel(false).0,
            Some((replicated, AdminClient::new("/no/such/admin.sock"))),
        )
        .with_ingress_data_dir(data_dir.clone());

        let response = service
            .get_ingress_proxy_config(Request::new(
                op::GetIngressProxyConfig::into_request(GetIngressProxyConfigRequest {})
                    .encode()
                    .unwrap(),
            ))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::GetIngressProxyConfig>()
            .unwrap();

        assert_eq!(response, expected);
        server.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }
}

#[tokio::test]
async fn runtime_watch_without_a_cluster_store_is_unavailable() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-runtime-watch-{}",
        ployz_core::MachineId::random()
    ));
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
    let (restart, _) = watch::channel(false);
    let service = MachineService::new(store, restart);
    let error = service
        .runtime_watch(Request::new(
            op::RuntimeWatch::into_request(RuntimeWatchRequest {})
                .encode()
                .unwrap(),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unavailable);
    let _ = std::fs::remove_dir_all(data_dir);
}
