//! Shared accepted-row adjudication for route hostnames.

use ployz_core::corrosion::{RouteBindingDocument, StoredRow, read_named_rows};
use ployz_core::ids::ClusterName;
use ployz_core::operation::RouteHostname;

pub(super) fn route_for_hostname(
    cluster_id: &ClusterName,
    rows: impl IntoIterator<Item = StoredRow>,
    hostname: &RouteHostname,
) -> Option<RouteBindingDocument> {
    read_named_rows::<RouteBindingDocument>(cluster_id, rows)
        .accepted
        .into_iter()
        .find(|row| &row.value.hostname == hostname)
        .map(|row| row.value)
}
