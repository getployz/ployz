use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use ployz_core::{
    Ciphertext, ContainerLocator, ContainerNow, ContainerWant, ENV_SEALED_CAPABILITY,
    EnsuredContainer, EnvValue, MANAGEMENT_SCOPE_CAPABILITY, MachineId, MachineList,
    MachineObservation, MembershipObservation, NewContainer, OpaquePayload, ResolvedServiceSpec,
    ResolvedUpdateConfig, RpcErrorCode, RpcRequestBody, RpcResponse, RunRotation, RunTarget,
    SealedScope, SealedValue, ServiceMode,
};
use tonic::{Request, Response};

use crate::cluster::Client;
use crate::global_fixtures::{machine, requested, service_id};

/// A Cluster of Machine `a`, which can open Sealed values, and Machine `b` with
/// `b_claims`. Counts the `EnsureContainer` calls that reach it.
pub(crate) async fn fleet(
    b_claims: &[&str],
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    Arc<AtomicUsize>,
) {
    let mut ready = machine('a', "machine-a");
    ready.runtime.capabilities = [MANAGEMENT_SCOPE_CAPABILITY, ENV_SEALED_CAPABILITY]
        .map(String::from)
        .into();
    let mut other = machine('b', "machine-b");
    other.runtime.capabilities = b_claims.iter().copied().map(String::from).collect();
    let machines = vec![
        MachineObservation::new(ready.clone(), MembershipObservation::Up),
        MachineObservation::new(other, MembershipObservation::Up),
    ];
    let ensured = Arc::new(AtomicUsize::new(0));
    let sent = ensured.clone();
    let (client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let machines = machines.clone();
            let id = ready.id;
            let sent = sent.clone();
            async move {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "fixture answers only the RPCs a Sealed create sends"
                )]
                let response = match rpc.into_inner().decode_request().unwrap().body {
                    RpcRequestBody::DescribeContract(_) => {
                        RpcResponse::from(ployz_core::ContractDescription {
                            machine_id: id,
                            protocol_major: ployz_core::PROTOCOL_MAJOR,
                            daemon_version: "fixture".into(),
                            capabilities: Default::default(),
                        })
                    }
                    RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                        enrollment: None,
                        machines,
                    }),
                    RpcRequestBody::EnsureContainer(_) => {
                        sent.fetch_add(1, Ordering::SeqCst);
                        RpcResponse::from(EnsuredContainer::Ensured {
                            now: ContainerNow::Absent {
                                of: ContainerLocator::Id(crate::global_fixtures::container_id('c')),
                            },
                            health: None,
                        })
                    }
                    other => panic!("unexpected RPC: {other:?}"),
                };
                Ok(Response::new(response.encode().unwrap()))
            }
        })
        .await;
    (client, server, ensured)
}

pub(crate) fn spec(sealed: bool) -> ResolvedServiceSpec {
    let mut spec = requested(ServiceMode::Global);
    let value = if sealed {
        EnvValue::Sealed(SealedValue {
            sealed: Ciphertext::from_bytes(vec![7; 48]),
            scope: SealedScope::Shared,
            key: "TOKEN".into(),
        })
    } else {
        EnvValue::from("plain")
    };
    spec.container.environment.insert("TOKEN".into(), value);
    spec.to_resolved(service_id('a'), ResolvedUpdateConfig::default())
        .unwrap()
}

fn want(spec: ResolvedServiceSpec) -> ContainerWant {
    ContainerWant::Running {
        container: RunTarget::New(NewContainer {
            r#gen: ployz_core::Gen::FIRST,
            slot: ployz_core::SlotKey {
                namespace: ployz_core::Namespace::parse("app").unwrap(),
                service: spec.name.clone(),
                kind: ployz_core::ContainerKind::ServiceContainer,
                creation_key: "global:test".to_owned().try_into().unwrap(),
            },
            resolved_spec: spec,
            deployment_id: None,
            registry_auth: None,
            image_prepared: false,
            answer: ployz_core::AnswerWhen::Started,
        }),
        rotation: RunRotation::Pending,
    }
}

#[tokio::test]
async fn a_sealed_value_reaches_a_cluster_only_when_every_machine_can_keep_it_sealed() {
    let target = MachineId::parse("a".repeat(32)).unwrap();
    let both = [MANAGEMENT_SCOPE_CAPABILITY, ENV_SEALED_CAPABILITY];
    for (b_claims, sealed, sent) in [
        (&both[..], true, true),
        (&[MANAGEMENT_SCOPE_CAPABILITY][..], true, false),
        (&[ENV_SEALED_CAPABILITY][..], true, false),
        (&[][..], true, false),
        (&[][..], false, true),
    ] {
        let (client, server, ensured) = fleet(b_claims).await;

        let result = client
            .ensure_unsettled(&target, want(spec(sealed)), None)
            .await;

        assert_eq!(
            ensured.load(Ordering::SeqCst),
            usize::from(sent),
            "{b_claims:?}"
        );
        if sent {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code, RpcErrorCode::Unsupported);
            assert!(
                error
                    .message
                    .starts_with("machine-b cannot open Sealed values"),
                "{}",
                error.message
            );
        }
        server.abort();
    }
}
