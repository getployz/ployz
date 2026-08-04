//! Conversion from validated Corrosion rows into Core lens snapshots.

use ployz_core::corrosion::{
    ClusterDocument, ContainerDocument, MachineDocument, MachineStatusDocument, OperationDocument,
    ServiceDocument, StoredRow, read_named_roster_rows, read_rows,
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

    Ok(LensSnapshot::Machines { cluster, rows })
}

pub(super) fn services_snapshot(
    expected_cluster: &ClusterId,
    stored_rows: Vec<StoredRow>,
) -> LensSnapshot {
    let report = read_rows::<ServiceDocument>(expected_cluster, stored_rows);
    let mut rows = Vec::with_capacity(report.accepted.len());
    for accepted in report.accepted {
        let Ok(id) = ServiceRowId::try_new(accepted.source.key) else {
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
