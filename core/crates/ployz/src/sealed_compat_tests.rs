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

pub(crate) struct Fleet {
    pub(crate) client: Client,
    pub(crate) server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    pub(crate) ensure_calls: Arc<AtomicUsize>,
}

pub(crate) async fn fleet(b_claims: &[&str]) -> Fleet {
    let mut ready = machine('a', "machine-a");
    ready.runtime.capabilities = [MANAGEMENT_SCOPE_CAPABILITY, ENV_SEALED_CAPABILITY]
        .map(String::from)
        .into();
    let mut other = machine('b', "machine-b");
    other.runtime.capabilities = b_claims.iter().copied().map(String::from).collect();
    listing(vec![
        MachineObservation::new(ready, MembershipObservation::Up),
        MachineObservation::new(other, MembershipObservation::Up),
    ])
    .await
}

async fn listing(machines: Vec<MachineObservation>) -> Fleet {
    let id = machine('a', "machine-a").id;
    let ensure_calls = Arc::new(AtomicUsize::new(0));
    let sent = ensure_calls.clone();
    let (client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let machines = machines.clone();
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
    Fleet {
        client,
        server,
        ensure_calls,
    }
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
        let Fleet {
            client,
            server,
            ensure_calls,
        } = fleet(b_claims).await;

        let result = client
            .ensure_unsettled(&target, want(spec(sealed)), None)
            .await;

        assert_eq!(
            ensure_calls.load(Ordering::SeqCst),
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

#[tokio::test]
async fn a_sealed_value_is_refused_when_the_target_shows_no_capability_evidence() {
    let mut ready = machine('a', "machine-a");
    ready.runtime.capabilities = [MANAGEMENT_SCOPE_CAPABILITY, ENV_SEALED_CAPABILITY]
        .map(String::from)
        .into();
    let unlisted = MachineId::parse("c".repeat(32)).unwrap();
    for (machines, target) in [
        (Vec::new(), ready.id),
        (
            vec![MachineObservation::new(
                ready.clone(),
                MembershipObservation::Up,
            )],
            unlisted,
        ),
    ] {
        let Fleet {
            client,
            server,
            ensure_calls,
        } = listing(machines).await;

        let error = client
            .ensure_unsettled(&target, want(spec(true)), None)
            .await
            .unwrap_err();

        assert_eq!(ensure_calls.load(Ordering::SeqCst), 0);
        assert_eq!(error.code, RpcErrorCode::Unsupported);
        assert!(
            error
                .message
                .starts_with(&format!("{target} shows no evidence")),
            "{}",
            error.message
        );
        server.abort();
    }
}

#[test]
fn a_build_reading_a_sealed_value_runs_only_where_the_daemon_opens_them() {
    use ployz_core::BUILD_SEALED_CAPABILITY;
    let mut able = machine('a', "able");
    able.runtime.capabilities = [BUILD_SEALED_CAPABILITY].map(String::from).into();
    let behind = machine('b', "behind");
    let visible = vec![
        MachineObservation::new(behind.clone(), MembershipObservation::Up),
        MachineObservation::new(able.clone(), MembershipObservation::Up),
    ];

    let plain = crate::sealed_compat::builders(&visible, false).unwrap();
    assert_eq!(plain.len(), 2);

    let sealed = crate::sealed_compat::builders(&visible, true).unwrap();
    let names: Vec<_> = sealed
        .iter()
        .map(|observation| observation.machine.name.to_string())
        .collect();
    assert_eq!(names, ["able"]);

    let only_behind = vec![MachineObservation::new(behind, MembershipObservation::Up)];
    let crate::connect::ConnectError::Remote(error) =
        crate::sealed_compat::builders(&only_behind, true).unwrap_err()
    else {
        panic!("expected a remote refusal");
    };
    assert_eq!(error.code, RpcErrorCode::Unsupported);
    assert_eq!(
        error.message,
        "behind cannot open sealed values; upgrade every Server first."
    );
    assert_eq!(
        error
            .details
            .pointer("/rejections/cannot open sealed values"),
        Some(&serde_json::json!(1))
    );
}
