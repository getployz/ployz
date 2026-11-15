//! The stored shape of a Deployment's `nodes` column, pinned: rolling deploys and
//! older readers decode these rows, so each re-encodes to the bytes it was read from.

use super::TargetNode;

const ID: &str = "6f1c2a9e-4d3b-4f7a-9c1e-0a5b7d8e2f31";
const MACHINE: &str = "0123456789abcdef0123456789abcdef";
const VOLUME: &str = "0a5b7d8e-2f31-4f7a-9c1e-6f1c2a9e4d3b";

fn decode(row: &str) -> TargetNode {
    serde_json::from_str(row).unwrap_or_else(|error| panic!("{row}: {error}"))
}

fn runtimes(node: &TargetNode) -> Vec<String> {
    node.runtimes().iter().map(ToString::to_string).collect()
}

/// Each row a Deployment's nodes are stored as, with whether it retires its Service
/// and the runtime Services it owns rows for.
fn rows() -> Vec<(String, bool, Vec<&'static str>)> {
    vec![
        (
            format!(r#"{{"type":"service","id":"{ID}","name":"web","runtime":"web-1"}}"#),
            false,
            vec!["web-1"],
        ),
        (
            format!(
                r#"{{"type":"service","id":"{ID}","name":"web","runtime":"web-1","removals":["web-0"]}}"#
            ),
            false,
            vec!["web-1", "web-0"],
        ),
        (
            format!(
                r#"{{"type":"service","id":"{ID}","name":"api","runtime":"api-1","removals":["api-1"]}}"#
            ),
            true,
            vec!["api-1"],
        ),
        (
            format!(
                r#"{{"type":"service","id":"{ID}","name":"api","runtime":"api-1","removals":["api-1","api-2"]}}"#
            ),
            true,
            vec!["api-1", "api-2"],
        ),
        (
            format!(
                r#"{{"type":"service","id":"{ID}","name":"api","runtime":"api-2","removals":["api-1","api-2"]}}"#
            ),
            true,
            vec!["api-1", "api-2"],
        ),
        (
            format!(r#"{{"type":"volume","id":"{VOLUME}","name":"data"}}"#),
            false,
            vec![],
        ),
        (
            format!(
                r#"{{"type":"volume","id":"{VOLUME}","name":"data","deletes":[{{"machine_id":"{MACHINE}","name":"data-1"}}]}}"#
            ),
            false,
            vec![],
        ),
        (
            format!(r#"{{"type":"config","id":"{VOLUME}","name":"env"}}"#),
            false,
            vec![],
        ),
    ]
}

#[test]
fn every_stored_node_re_encodes_to_the_bytes_it_was_read_from() {
    for (row, _, _) in rows() {
        assert_eq!(serde_json::to_string(&decode(&row)).unwrap(), row);
    }
}

#[test]
fn a_node_retires_when_it_tears_down_the_runtime_it_names_and_owns_the_rows_of_what_it_touches() {
    for (row, retires, owned) in rows() {
        let node = decode(&row);
        assert_eq!(node.retires(), retires, "{row}");
        assert_eq!(runtimes(&node), owned, "{row}");
    }
}

#[test]
fn nodes_decode_in_a_list_as_the_column_stores_them() {
    let list = format!(
        "[{}]",
        rows()
            .into_iter()
            .map(|(row, ..)| row)
            .collect::<Vec<_>>()
            .join(",")
    );
    let nodes: Vec<TargetNode> = serde_json::from_str(&list).unwrap();
    assert_eq!(serde_json::to_string(&nodes).unwrap(), list);
}

#[test]
fn a_row_tearing_down_its_own_runtime_decodes_as_a_retirement_keeping_that_runtime() {
    let row = |at: usize| rows().into_iter().nth(at).expect("a pinned row").0;
    let name = |text: &str| ployz_core::ServiceName::parse(text).unwrap();
    let TargetNode::Retire {
        runtime, runtimes, ..
    } = decode(&row(4))
    else {
        panic!("a row whose removals hold its runtime retires");
    };
    assert_eq!(runtime, name("api-2"));
    assert_eq!(
        runtimes,
        ployz_core::ServiceRemovals::from_names([name("api-1"), name("api-2")])
    );
    assert!(matches!(decode(&row(1)), TargetNode::Service { .. }));
}
