//! Conversion from validated Corrosion rows into Core lens snapshots.

use ployz_core::corrosion::{
    ClusterDocument, ContainerDocument, MachineDocument, MachineStatusDocument, OperationDocument,
    ServiceDocument, StoredRow, read_named_roster_rows, read_rows,
};
use ployz_core::ids::{ClusterName, MachineName};
use ployz_core::{
    ApiRefusal, ContainerLensRow, LensSnapshot, MachineLensRow, MachineStatusLensRow,
    OperationLensRow, ServiceLensRow,
};

pub(super) fn machines_snapshot(
    expected_cluster: &ClusterName,
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
        let Ok(id) = MachineName::try_new(accepted.source.key) else {
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
    expected_cluster: &ClusterName,
    stored_rows: Vec<StoredRow>,
) -> LensSnapshot {
    let report = read_rows::<ServiceDocument>(expected_cluster, stored_rows);
    let mut rows = Vec::with_capacity(report.accepted.len());
    for accepted in report.accepted {
        rows.push(ServiceLensRow {
            key: accepted.source.key,
            document: accepted.value,
        });
    }
    rows.sort_by(|left, right| left.key.cmp(&right.key));
    LensSnapshot::Services { rows }
}

pub(super) fn containers_snapshot(
    expected_cluster: &ClusterName,
    stored_rows: Vec<StoredRow>,
) -> LensSnapshot {
    let report = read_rows::<ContainerDocument>(expected_cluster, stored_rows);
    let mut rows = Vec::with_capacity(report.accepted.len());
    for accepted in report.accepted {
        rows.push(ContainerLensRow {
            id: accepted.source.key,
            document: accepted.value,
        });
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    LensSnapshot::Containers { rows }
}

pub(super) fn machine_status_snapshot(
    expected_cluster: &ClusterName,
    stored_rows: Vec<StoredRow>,
) -> LensSnapshot {
    let report = read_rows::<MachineStatusDocument>(expected_cluster, stored_rows);
    let mut rows = Vec::with_capacity(report.accepted.len());
    for accepted in report.accepted {
        let Ok(id) = MachineName::try_new(accepted.source.key) else {
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
    expected_cluster: &ClusterName,
    stored_rows: Vec<StoredRow>,
) -> LensSnapshot {
    let report = read_rows::<OperationDocument>(expected_cluster, stored_rows);
    let mut rows = Vec::with_capacity(report.accepted.len());
    for accepted in report.accepted {
        rows.push(OperationLensRow {
            namespace_name: accepted.value.namespace_id.clone(),
            deploy_name: accepted.value.deploy_name.clone(),
            document: accepted.value,
        });
    }
    rows.sort_by(|left, right| {
        (&left.namespace_name, &left.deploy_name).cmp(&(&right.namespace_name, &right.deploy_name))
    });
    LensSnapshot::Operations { rows }
}

#[cfg(test)]
mod tests {
    use ployz_core::LensSnapshot;
    use ployz_core::corrosion::StoredRow;
    use ployz_core::ids::ClusterName;
    use serde_json::json;

    use super::services_snapshot;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const NAMESPACE: &str = "production";
    const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const API_SERVICE: &str = "production/api";
    const WORKER_SERVICE: &str = "production/worker";
    const OPERATION: &str = "release-1";

    fn cluster_id() -> ClusterName {
        ClusterName::try_new(CLUSTER).expect("valid cluster fixture")
    }

    fn service_row(id: &str, name: &str, image: &str) -> StoredRow {
        StoredRow::new(
            id,
            serde_json::to_string(&json!({
                "v": 1,
                "cluster_id": CLUSTER,
                "written_by": { "kind": "peer", "peer_id": PEER },
                "written_at": "2026-08-04T10:00:00Z",
                "namespace_id": NAMESPACE,
                "name": name,
                "image": image,
                "env_fingerprints": {},
                "mode": "replicated",
                "replicas": 1,
                "pinned_machines": [],
                "active_deploy": OPERATION,
                "previous_image": null,
                "deployed_at": "2026-08-04T10:01:00Z"
            }))
            .expect("fixture document JSON"),
        )
    }

    #[test]
    fn services_snapshot_keeps_multiple_named_services_in_one_namespace() {
        let snapshot = services_snapshot(
            &cluster_id(),
            vec![
                service_row(WORKER_SERVICE, "worker", "ghcr.io/acme/worker:latest"),
                service_row(API_SERVICE, "api", "ghcr.io/acme/api:latest"),
            ],
        );
        let LensSnapshot::Services { rows } = snapshot else {
            panic!("expected services snapshot")
        };
        let [api, worker] = rows.as_slice() else {
            panic!("expected two named service rows")
        };

        assert_eq!(api.key, API_SERVICE);
        assert_eq!(worker.key, WORKER_SERVICE);
    }
}
