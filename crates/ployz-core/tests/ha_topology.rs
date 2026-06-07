use ployz_core::ha::{
    CanonicalNodeSet, CanonicalNodeSetError, CoreAvailabilityError, CorePromotionTopology,
    CoreQuorumMode, CoreQuorumStatus, CoreTopology, CoreTopologyError, CoreTopologyMode,
};
use ployz_core::ids::NodeId;

#[test]
fn one_core_reports_single_node_health() {
    let topology = CoreTopology::from_nodes(vec![node_id("core_1")]).expect("single core topology");

    assert_eq!(topology.core_count(), 1);
    assert!(!topology.is_high_availability());
    assert_eq!(
        topology.quorum_status(vec![node_id("core_1")]),
        Ok(CoreQuorumStatus::Available {
            mode: CoreQuorumMode::Single,
            available: 1,
            required: 1,
        })
    );
    assert_eq!(
        topology.quorum_status(Vec::new()),
        Ok(CoreQuorumStatus::Unavailable {
            mode: CoreQuorumMode::Single,
            available: 0,
            required: 1,
        })
    );
}

#[test]
fn two_cores_are_transitional_not_ha() {
    let topology = CorePromotionTopology::from_nodes(vec![node_id("core_1"), node_id("core_2")])
        .expect("two core promotion topology");

    assert_eq!(topology.nodes(), &[node_id("core_1"), node_id("core_2")]);
    assert_eq!(
        topology.quorum_status(vec![node_id("core_1"), node_id("core_2")]),
        Ok(CoreQuorumStatus::Available {
            mode: CoreQuorumMode::PromotingTwo,
            available: 2,
            required: 1,
        })
    );
    assert_eq!(
        topology.quorum_status(Vec::new()),
        Ok(CoreQuorumStatus::Unavailable {
            mode: CoreQuorumMode::PromotingTwo,
            available: 0,
            required: 1,
        })
    );
}

#[test]
fn three_core_ha_survives_one_core_down() {
    let topology = CoreTopology::from_nodes(vec![
        node_id("core_1"),
        node_id("core_2"),
        node_id("core_3"),
    ])
    .expect("three core topology");

    assert_eq!(topology.core_count(), 3);
    assert_eq!(topology.mode(), CoreTopologyMode::HighAvailability);
    assert!(topology.is_high_availability());
    assert_eq!(
        topology.quorum_status(vec![node_id("core_1"), node_id("core_2")]),
        Ok(CoreQuorumStatus::Available {
            mode: CoreQuorumMode::HighAvailability,
            available: 2,
            required: 2,
        })
    );
    assert_eq!(
        topology.quorum_status(vec![node_id("core_1")]),
        Ok(CoreQuorumStatus::Unavailable {
            mode: CoreQuorumMode::HighAvailability,
            available: 1,
            required: 2,
        })
    );
}

#[test]
fn core_topology_has_set_semantics() {
    let topology = CoreTopology::from_nodes(vec![
        node_id("core_3"),
        node_id("core_1"),
        node_id("core_2"),
    ])
    .expect("three core topology");
    let same_topology = CoreTopology::from_nodes(vec![
        node_id("core_1"),
        node_id("core_2"),
        node_id("core_3"),
    ])
    .expect("same three core topology");

    assert_eq!(topology, same_topology);
    assert_eq!(
        topology.nodes(),
        &[node_id("core_1"), node_id("core_2"), node_id("core_3")]
    );
    assert_eq!(
        topology.quorum_status(vec![node_id("core_2"), node_id("core_1")]),
        Ok(CoreQuorumStatus::Available {
            mode: CoreQuorumMode::HighAvailability,
            available: 2,
            required: 2,
        })
    );
}

#[test]
fn canonical_node_set_has_set_semantics() {
    let nodes = CanonicalNodeSet::from_nodes(vec![
        node_id("core_3"),
        node_id("core_1"),
        node_id("core_2"),
    ])
    .expect("canonical node set");

    assert_eq!(
        nodes.nodes(),
        &[node_id("core_1"), node_id("core_2"), node_id("core_3")]
    );
    assert!(nodes.contains_all(&[node_id("core_1"), node_id("core_3")]));
    assert_eq!(
        CanonicalNodeSet::from_nodes(vec![node_id("core_1"), node_id("core_1")]),
        Err(CanonicalNodeSetError::DuplicateNode {
            node_id: node_id("core_1"),
        })
    );
}

#[test]
fn core_topology_has_stable_wire_shape() {
    let topology = CoreTopology::from_nodes(vec![
        node_id("core_3"),
        node_id("core_1"),
        node_id("core_2"),
    ])
    .expect("three core topology");
    let json = serde_json::to_string(&topology).expect("topology serializes");

    assert_eq!(
        json,
        r#"{"mode":"high_availability","nodes":["core_1","core_2","core_3"]}"#
    );
    assert_eq!(
        serde_json::from_str::<CoreTopology>(
            r#"{"mode":"high_availability","nodes":["core_3","core_1","core_2"]}"#
        )
        .expect("topology deserializes"),
        topology
    );
}

#[test]
fn core_availability_rejects_duplicate_and_unknown_available_nodes() {
    let topology = CoreTopology::from_nodes(vec![
        node_id("core_1"),
        node_id("core_2"),
        node_id("core_3"),
    ])
    .expect("three core topology");

    assert_eq!(
        topology.quorum_status(vec![node_id("core_1"), node_id("core_1")]),
        Err(CoreAvailabilityError::DuplicateAvailableNode {
            node_id: node_id("core_1"),
        })
    );
    assert_eq!(
        topology.quorum_status(vec![node_id("edge_1")]),
        Err(CoreAvailabilityError::UnknownCore {
            node_id: node_id("edge_1"),
        })
    );
}

#[test]
fn core_topology_rejects_empty_duplicate_and_too_many_nodes() {
    assert_eq!(
        CoreTopology::from_nodes(Vec::new()),
        Err(CoreTopologyError::Empty)
    );
    assert_eq!(
        CoreTopology::from_nodes(vec![node_id("core_1"), node_id("core_1")]),
        Err(CoreTopologyError::DuplicateNode {
            node_id: node_id("core_1"),
        })
    );
    assert_eq!(
        CoreTopology::from_nodes(vec![node_id("core_1"), node_id("core_2")]),
        Err(CoreTopologyError::TransitionalCoreCount { count: 2 })
    );
    assert_eq!(
        CoreTopology::from_nodes(vec![
            node_id("core_1"),
            node_id("core_2"),
            node_id("core_3"),
            node_id("core_4"),
        ]),
        Err(CoreTopologyError::TooMany { count: 4 })
    );
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}
