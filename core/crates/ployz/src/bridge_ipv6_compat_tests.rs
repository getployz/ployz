use std::sync::atomic::Ordering;

use ployz_core::{
    BRIDGE_IPV6_CAPABILITY, MachineId, MachineObservation, MachineTarget, MembershipObservation,
    RpcErrorCode, RpcRequestBody,
};
use serde_json::json;

use crate::global_fixtures::machine;
use crate::sealed_compat::tests::{Fleet, listing, spec, want};

#[tokio::test]
async fn a_spec_that_takes_ipv6_reaches_only_a_machine_that_advertises_it() {
    let target = MachineId::parse("a".repeat(32)).unwrap();
    for (claims, ipv6, sent) in [
        (&[BRIDGE_IPV6_CAPABILITY][..], true, true),
        (&[][..], true, false),
        (&[][..], false, true),
    ] {
        let mut a = machine('a', "machine-a");
        a.runtime.capabilities = claims.iter().copied().map(String::from).collect();
        let Fleet {
            client,
            server,
            ensure_calls,
        } = listing(vec![MachineObservation::new(a, MembershipObservation::Up)]).await;
        let mut spec = spec(false);
        spec.container.ipv6 = ipv6;

        let result = client.ensure_unsettled(&target, want(spec), None).await;

        assert_eq!(ensure_calls.load(Ordering::SeqCst), usize::from(sent));
        if sent {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code, RpcErrorCode::Unsupported);
            assert!(
                error
                    .message
                    .starts_with("machine-a runs a ployzd that can't give a Service IPv6"),
                "{}",
                error.message
            );
        }
        server.abort();
    }
}

#[tokio::test]
async fn a_spec_that_takes_ipv6_is_refused_when_the_target_is_not_listed() {
    let Fleet {
        client,
        server,
        ensure_calls,
    } = listing(Vec::new()).await;
    let target = MachineId::parse("a".repeat(32)).unwrap();
    let mut spec = spec(false);
    spec.container.ipv6 = true;

    let error = client
        .ensure_unsettled(&target, want(spec), None)
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

#[tokio::test]
async fn a_volume_move_creates_a_copy_that_takes_ipv6_only_where_it_is_advertised() {
    let target = MachineTarget::from(&MachineId::parse("a".repeat(32)).unwrap());
    for (claims, ipv6, admitted) in [
        (&[BRIDGE_IPV6_CAPABILITY][..], true, true),
        (&[][..], true, false),
        (&[][..], false, true),
    ] {
        let mut a = machine('a', "machine-a");
        a.runtime.capabilities = claims.iter().copied().map(String::from).collect();
        let Fleet { client, server, .. } =
            listing(vec![MachineObservation::new(a, MembershipObservation::Up)]).await;
        let mut spec = spec(false);
        spec.container.ipv6 = ipv6;
        let body = RpcRequestBody::SetVolumeCopy(
            serde_json::from_value(json!({
                "name": "app_data", "expect": {"kind": "writer", "epoch": 1},
                "want": {"kind": "writer", "at": "7", "epoch": 2, "start": {
                    "kind": "create", "namespace": "app", "resolved_spec": spec,
                    "creation_key": "move-1", "gen": 1, "rotation": "pending"}}
            }))
            .unwrap(),
        );

        let result = super::admit_switch(&client, &target, &body).await;

        if admitted {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code, RpcErrorCode::Unsupported);
            assert_eq!(
                error.details,
                json!({ "capability": BRIDGE_IPV6_CAPABILITY })
            );
        }
        server.abort();
    }
}
