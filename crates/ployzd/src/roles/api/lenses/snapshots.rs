//! Conversion from validated Corrosion rows into Core lens snapshots.

use ployz_core::corrosion::{
    ClusterDocument, ContainerDocument, MachineDocument, MachineStatusDocument, OperationDocument,
    ServiceDocument, StoredRow, read_named_roster_rows, read_named_rows, read_rows,
};
use ployz_core::ids::{ClusterId, ContainerId, MachineRowId, OperationRowId, ServiceRowId};
use ployz_core::{
    ApiRefusal, ContainerLensRow, LensSnapshot, MachineLensRow, MachineStatusLensRow,
    OperationLensRow, ServiceLensRow,
};

pub(super) fn machines_snapshot(
    expected_cluster: &ClusterId,
    cluster_rows: Vec<StoredRow>,
    machine_rows: Vec<StoredRow>,
) -> Result<LensSnapshot, ApiRefusal> {
    let cluster_report = read_rows::<ClusterDocument>(expected_cluster, cluster_rows);
    let cluster = match cluster_report.accepted.as_slice() {
        [accepted]
            if accepted.source.key == expected_cluster.as_str()
                && accepted.value.cluster_id == *expected_cluster =>
        {
            accepted.value.clone()
        }
        [] if cluster_report.skipped.is_empty() => return Err(ApiRefusal::MissingCluster),
        _ => return Err(ApiRefusal::InvalidCluster),
    };

    let report = read_named_roster_rows::<MachineDocument>(&cluster, machine_rows);
    let mut rows = Vec::with_capacity(report.accepted.len());
    for accepted in report.accepted {
        let Ok(id) = MachineRowId::try_new(accepted.id.as_str().to_owned()) else {
            continue;
        };
        rows.push(MachineLensRow {
            id,
            document: accepted.value,
        });
    }
    rows.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));

    Ok(LensSnapshot::Machines {
        cluster: Box::new(cluster),
        rows,
    })
}

pub(super) fn services_snapshot(
    expected_cluster: &ClusterId,
    stored_rows: Vec<StoredRow>,
) -> LensSnapshot {
    let report = read_named_rows::<ServiceDocument>(expected_cluster, stored_rows);
    let mut rows = Vec::with_capacity(report.accepted.len());
    for accepted in report.accepted {
        let Ok(id) = ServiceRowId::try_new(accepted.id.as_str().to_owned()) else {
            continue;
        };
        rows.push(ServiceLensRow {
            id,
            document: accepted.value,
        });
    }
    rows.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    LensSnapshot::Services { rows }
}

pub(super) fn containers_snapshot(
    expected_cluster: &ClusterId,
    stored_rows: Vec<StoredRow>,
) -> LensSnapshot {
    let report = read_rows::<ContainerDocument>(expected_cluster, stored_rows);
    let mut rows = Vec::with_capacity(report.accepted.len());
    for accepted in report.accepted {
        let Ok(id) = ContainerId::try_new(accepted.source.key) else {
            continue;
        };
        rows.push(ContainerLensRow {
            id,
            document: accepted.value,
        });
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    LensSnapshot::Containers { rows }
}

pub(super) fn machine_status_snapshot(
    expected_cluster: &ClusterId,
    stored_rows: Vec<StoredRow>,
) -> LensSnapshot {
    let report = read_rows::<MachineStatusDocument>(expected_cluster, stored_rows);
    let mut rows = Vec::with_capacity(report.accepted.len());
    for accepted in report.accepted {
        let Ok(id) = MachineRowId::try_new(accepted.source.key) else {
            continue;
        };
        let Ok(row) = MachineStatusLensRow::try_new(id, accepted.value) else {
            continue;
        };
        rows.push(row);
    }
    rows.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    LensSnapshot::MachineStatus { rows }
}

pub(super) fn operations_snapshot(
    expected_cluster: &ClusterId,
    stored_rows: Vec<StoredRow>,
) -> LensSnapshot {
    let report = read_rows::<OperationDocument>(expected_cluster, stored_rows);
    let mut rows = Vec::with_capacity(report.accepted.len());
    for accepted in report.accepted {
        let Ok(id) = OperationRowId::try_new(accepted.source.key) else {
            continue;
        };
        rows.push(OperationLensRow {
            id,
            document: accepted.value,
        });
    }
    rows.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    LensSnapshot::Operations { rows }
}

#[cfg(test)]
mod tests {
    use ployz_core::LensSnapshot;
    use ployz_core::corrosion::StoredRow;
    use ployz_core::ids::ClusterId;
    use serde_json::json;

    use super::services_snapshot;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const NAMESPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const LOWER_SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const HIGHER_SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const OPERATION: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB1";

    fn cluster_id() -> ClusterId {
        ClusterId::try_new(CLUSTER).expect("valid cluster fixture")
    }

    fn service_row(id: &str, image: &str) -> StoredRow {
        StoredRow::new(
            id,
            serde_json::to_string(&json!({
                "v": 1,
                "cluster_id": CLUSTER,
                "written_by": { "kind": "peer", "peer_id": PEER },
                "written_at": "2026-08-04T10:00:00Z",
                "namespace_id": NAMESPACE,
                "name": "api",
                "image": image,
                "env_fingerprints": {},
                "mode": "replicated",
                "replicas": 1,
                "pinned_machines": [],
                "active_deploy": OPERATION,
                "previous_image": null,
                "deployed_at": "2026-08-04T10:01:00Z",
                "operation_id": OPERATION
            }))
            .expect("fixture document JSON"),
        )
    }

    #[test]
    fn services_snapshot_adjudicates_duplicate_names_by_lowest_ulid() {
        let snapshot = services_snapshot(
            &cluster_id(),
            vec![
                service_row(HIGHER_SERVICE, "ghcr.io/acme/api:higher"),
                service_row(LOWER_SERVICE, "ghcr.io/acme/api:lower"),
            ],
        );
        let LensSnapshot::Services { rows } = snapshot else {
            panic!("expected services snapshot")
        };
        let [row] = rows.as_slice() else {
            panic!("expected exactly one canonical service row")
        };

        assert_eq!(row.id.as_str(), LOWER_SERVICE);
        assert_eq!(row.document.image.as_str(), "ghcr.io/acme/api:lower");
    }
}
