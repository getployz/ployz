use ployz_core::ha::{CoreAvailability, CoreAvailabilityError, CoreTopology, CoreTopologyError};
use ployz_core::ids::NodeId;

#[test]
fn one_core_reports_single_node_availability() {
    let topology = CoreTopology::single(node_id("core_1"));

    assert_eq!(topology.core_count(), 1);
    assert_eq!(topology.nodes(), &[node_id("core_1")]);
    assert_eq!(
        topology.availability(vec![node_id("core_1")]),
        Ok(CoreAvailability::Available)
    );
    assert_eq!(
        topology.availability(Vec::new()),
        Ok(CoreAvailability::Unavailable)
    );
}

#[test]
fn core_availability_rejects_duplicate_and_unknown_available_nodes() {
    let topology = CoreTopology::single(node_id("core_1"));

    assert_eq!(
        topology.availability(vec![node_id("core_1"), node_id("core_1")]),
        Err(CoreAvailabilityError::DuplicateAvailableNode {
            node_id: node_id("core_1"),
        })
    );
    assert_eq!(
        topology.availability(vec![node_id("edge_1")]),
        Err(CoreAvailabilityError::UnknownCore {
            node_id: node_id("edge_1"),
        })
    );
}

#[test]
fn core_topology_wire_shape_is_single_core_only() {
    let topology = CoreTopology::single(node_id("core_1"));
    let json = serde_json::to_string(&topology).expect("topology serializes");

    assert_eq!(json, r#"{"mode":"single","node":"core_1"}"#);
    assert_eq!(
        serde_json::from_str::<CoreTopology>(r#"{"mode":"single","node":"core_1"}"#)
            .expect("topology deserializes"),
        topology
    );
}

#[test]
fn core_topology_rejects_stale_high_availability_wire_shape() {
    assert!(
        serde_json::from_str::<CoreTopology>(
            r#"{"mode":"high_availability","nodes":["core_1","core_2","core_3"]}"#
        )
        .is_err()
    );
}

#[test]
fn core_topology_rejects_empty_duplicate_and_multi_core_nodes() {
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
        Err(CoreTopologyError::UnsupportedCoreCount { count: 2 })
    );
    assert_eq!(
        CoreTopology::from_nodes(vec![
            node_id("core_1"),
            node_id("core_2"),
            node_id("core_3"),
        ]),
        Err(CoreTopologyError::UnsupportedCoreCount { count: 3 })
    );
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}
