//! Shared accepted-row adjudication for route hostnames.

use ployz_core::corrosion::{RouteBindingDocument, StoredRow, read_named_rows};
use ployz_core::ids::{ClusterId, RouteBindingRowId};
use ployz_core::operation::RouteHostname;

pub(super) struct AcceptedRoute {
    pub(super) id: RouteBindingRowId,
    pub(super) document: RouteBindingDocument,
}

pub(super) fn route_for_hostname(
    cluster_id: &ClusterId,
    rows: impl IntoIterator<Item = StoredRow>,
    hostname: &RouteHostname,
) -> Option<AcceptedRoute> {
    let winner = read_named_rows::<RouteBindingDocument>(cluster_id, rows)
        .accepted
        .into_iter()
        .find(|row| &row.value.hostname == hostname)?;
    Some(AcceptedRoute {
        id: RouteBindingRowId::try_new(winner.id.into_string())
            .expect("accepted route-binding id is canonical"),
        document: winner.value,
    })
}
