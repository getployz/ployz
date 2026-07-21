use std::time::{Duration, Instant};

use ployz_core::network::{DEFAULT_WIREGUARD_LISTEN_PORT, DataplaneProjectionMember};
use ployz_core::security::NatsPrincipal;
use ployz_e2e::dind::{self, DindMachine};
use ployz_nats::subjects::INTENT_CHANGED;
use ployz_test_support::ids::machine_id;

use super::{
    CONNECT_TIMEOUT, CoreContext, OVERLAY_FIRST_CONTACT_BUDGET, WireGuardPairBlock,
    add_and_join_edge, connect_core_client, finish, init_core_cluster, read_intent,
    set_wireguard_pair_block, wait_for_machine_observations, wait_for_ready_dataplane,
    with_evidence,
};

#[tokio::test]
async fn group_wireguard_reconciliation_preserves_runtime_evidence() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 1).await;
    with_evidence(&core.cluster, async {
        let [edge] = core.cluster.edges() else {
            panic!("WireGuard reconciliation requires exactly one edge")
        };
        add_and_join_edge(&core, edge).await;
        for machine in ["core_1", "edge_2"] {
            wait_for_machine_observations(&core, &machine_id(machine)).await;
        }

        let controller = connect_core_client(
            &core,
            NatsPrincipal::Controller,
            &core.material.controller_seed,
        )
        .await
        .expect("connect controller for WireGuard reconciliation");
        let intent = read_intent(&controller, CONNECT_TIMEOUT)
            .await
            .expect("read WireGuard reconciliation intent");
        wait_for_ready_dataplane(&core, &intent.dataplane_projection).await;
        let Some(peer) = intent
            .dataplane_projection
            .declared_members()
            .iter()
            .find(|member| member.machine_id == machine_id("edge_2"))
        else {
            panic!("dataplane projection omitted edge_2")
        };

        assert_unchanged_reconciliation_preserves_handshake(
            &core,
            &controller,
            core.cluster.core(),
            edge,
            peer,
        )
        .await;
    })
    .await;

    finish(core).await;
}

async fn assert_unchanged_reconciliation_preserves_handshake(
    core: &CoreContext,
    controller: &async_nats::Client,
    source: &DindMachine,
    edge: &DindMachine,
    peer: &DataplaneProjectionMember,
) {
    let peer_subnet = peer.endpoint_subnet.as_string();
    let peer_address = peer.endpoint_subnet.host_address().to_string();
    let public_key = peer.wireguard_public_key.as_str();
    let primed = core
        .exec_on(
            source,
            &[
                "ping",
                "-I",
                "ployz-wg0",
                "-c",
                "1",
                "-W",
                "2",
                &peer_address,
            ],
        )
        .await;
    assert!(
        primed.success(),
        "prime WireGuard handshake failed: {primed:?}"
    );
    set_wireguard_pair_block(core, source, edge, WireGuardPairBlock::Install).await;
    let baseline = wait_for_stable_handshake(core, source, public_key).await;

    let deleted = core
        .exec_on(
            source,
            &["ip", "route", "del", &peer_subnet, "dev", "ployz-wg0"],
        )
        .await;
    assert!(
        deleted.success(),
        "delete route sentinel failed: {deleted:?}"
    );
    publish_intent_invalidation(controller).await;
    wait_for_route(core, source, &peer_subnet, true).await;
    assert_frozen_handshake(core, source, public_key, baseline).await;

    let Some(drifted_port) = DEFAULT_WIREGUARD_LISTEN_PORT.checked_add(1) else {
        panic!("default WireGuard port has no successor")
    };
    let drifted_port = drifted_port.to_string();
    let drifted = core
        .exec_on(
            source,
            &["wg", "set", "ployz-wg0", "listen-port", &drifted_port],
        )
        .await;
    assert!(
        drifted.success(),
        "drift WireGuard listen port failed: {drifted:?}"
    );
    publish_intent_invalidation(controller).await;
    wait_for_listen_port(core, source, DEFAULT_WIREGUARD_LISTEN_PORT).await;
    assert_frozen_handshake(core, source, public_key, baseline).await;

    let rogue_subnet = "192.0.2.255/32";
    let drifted = core
        .exec_on(
            source,
            &[
                "wg",
                "set",
                "ployz-wg0",
                "peer",
                public_key,
                "allowed-ips",
                rogue_subnet,
            ],
        )
        .await;
    assert!(drifted.success(), "drift allowed IP failed: {drifted:?}");
    add_rogue_route(core, source, rogue_subnet).await;
    publish_intent_invalidation(controller).await;
    wait_for_allowed_ip(core, source, public_key, &peer_subnet).await;
    wait_for_route(core, source, rogue_subnet, false).await;

    add_rogue_route(core, source, rogue_subnet).await;
    publish_intent_invalidation(controller).await;
    wait_for_route(core, source, rogue_subnet, false).await;
    assert_frozen_handshake(core, source, public_key, baseline).await;

    set_wireguard_pair_block(core, source, edge, WireGuardPairBlock::Remove).await;
}

async fn add_rogue_route(core: &CoreContext, machine: &DindMachine, subnet: &str) {
    let outcome = core
        .exec_on(
            machine,
            &[
                "ip",
                "route",
                "add",
                subnet,
                "dev",
                "ployz-wg0",
                "proto",
                "boot",
                "scope",
                "link",
            ],
        )
        .await;
    assert!(outcome.success(), "add rogue route failed: {outcome:?}");
}

async fn publish_intent_invalidation(controller: &async_nats::Client) {
    controller
        .publish(INTENT_CHANGED, Vec::new().into())
        .await
        .expect("intent invalidation publishes");
    controller
        .flush()
        .await
        .expect("intent invalidation flushes");
}

async fn wait_for_stable_handshake(
    core: &CoreContext,
    machine: &DindMachine,
    public_key: &str,
) -> u64 {
    let deadline = Instant::now() + OVERLAY_FIRST_CONTACT_BUDGET;
    loop {
        let first = latest_handshake(core, machine, public_key).await;
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        let second = latest_handshake(core, machine, public_key).await;
        if first > 0 && first == second {
            return first;
        }
        assert!(
            Instant::now() < deadline,
            "WireGuard handshake did not freeze: {first} then {second}"
        );
    }
}

async fn assert_frozen_handshake(
    core: &CoreContext,
    machine: &DindMachine,
    public_key: &str,
    expected: u64,
) {
    assert_eq!(
        latest_handshake(core, machine, public_key).await,
        expected,
        "unchanged reconciliation reset the WireGuard handshake epoch"
    );
}

async fn latest_handshake(core: &CoreContext, machine: &DindMachine, public_key: &str) -> u64 {
    let outcome = core
        .exec_on(machine, &["wg", "show", "ployz-wg0", "latest-handshakes"])
        .await;
    assert!(
        outcome.success(),
        "read latest handshakes failed: {outcome:?}"
    );
    let epoch = outcome.stdout.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        match (fields.next(), fields.next(), fields.next()) {
            (Some(key), Some(epoch), None) if key == public_key => epoch.parse().ok(),
            _ => None,
        }
    });
    let Some(epoch) = epoch else {
        panic!("latest handshakes omitted peer {public_key}: {outcome:?}")
    };
    epoch
}

async fn wait_for_listen_port(core: &CoreContext, machine: &DindMachine, expected: u16) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let outcome = core
            .exec_on(machine, &["wg", "show", "ployz-wg0", "listen-port"])
            .await;
        assert!(outcome.success(), "inspect listen port failed: {outcome:?}");
        if outcome.stdout.trim().parse::<u16>() == Ok(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "listen port did not converge to {expected}: {outcome:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_route(core: &CoreContext, machine: &DindMachine, subnet: &str, present: bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let outcome = core
            .exec_on(
                machine,
                &["ip", "route", "show", "exact", subnet, "dev", "ployz-wg0"],
            )
            .await;
        assert!(outcome.success(), "inspect route failed: {outcome:?}");
        if outcome.stdout.trim().is_empty() != present {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "route {subnet} did not converge: {outcome:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_allowed_ip(
    core: &CoreContext,
    machine: &DindMachine,
    public_key: &str,
    subnet: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let outcome = core
            .exec_on(machine, &["wg", "show", "ployz-wg0", "allowed-ips"])
            .await;
        assert!(outcome.success(), "inspect allowed IPs failed: {outcome:?}");
        if outcome.stdout.lines().any(|line| {
            let mut fields = line.split_whitespace();
            fields.next() == Some(public_key)
                && fields.next() == Some(subnet)
                && fields.next().is_none()
        }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "allowed IP did not converge: {outcome:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
