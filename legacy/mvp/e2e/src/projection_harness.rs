use std::path::Path;
use std::sync::Arc;

use mvp_bus::{BusSession, IslandId};
use mvp_projection::{FactSource, ProjectionActorHandle, SqliteProjectionStore};

use crate::bus_syntax::fact_pattern;

pub(crate) fn projection_actor(
    source: Arc<dyn FactSource>,
    session: BusSession,
    root: &Path,
) -> Result<ProjectionActorHandle, String> {
    Ok(ProjectionActorHandle::spawn(
        source,
        IslandId::new("prod"),
        session,
        fact_pattern("/facts/>")?,
        SqliteProjectionStore::new(root.join("projections.sqlite")),
        root.join("gateway.snapshot"),
        root.join("dns.snapshot"),
    ))
}
