//! The display names `UPTIME_REGIONS` gives its regions, written to `uptime_region` for the
//! dashboard. Probe rows keep the code, so a rename relabels every probe already stored.

use std::time::Duration;

use tokio_postgres::types::{ToSql, Type};

use super::pg::{Pg, PgError};
use crate::config::RegionConfig;

const RETRY_MAX: Duration = Duration::from_secs(60);

/// Sets each configured region's name, null for a bare code. Rows for regions no longer
/// configured stay, so retired regions keep their label on the history they left.
const UPSERT: &str = "INSERT INTO uptime_region (region, name) \
     SELECT * FROM unnest($1::text[], $2::text[]) \
     ON CONFLICT (region) DO UPDATE SET name = excluded.name \
     WHERE uptime_region.name IS DISTINCT FROM excluded.name";

/// # Errors
/// As [`Pg::tx`].
pub async fn write(pg: &Pg, regions: &[RegionConfig]) -> Result<(), PgError> {
    let codes: Vec<&str> = regions.iter().map(|r| r.region.as_str()).collect();
    let names: Vec<Option<&str>> = regions
        .iter()
        .map(|r| r.name.as_ref().map(|n| n.as_str()))
        .collect();
    pg.tx(async |tx| {
        let params: [(&(dyn ToSql + Sync), Type); 2] =
            [(&codes, Type::TEXT_ARRAY), (&names, Type::TEXT_ARRAY)];
        tx.query_typed(UPSERT, &params).await?;
        Ok(())
    })
    .await
}

/// [`write`] until it lands. Names are labels only, so probing never waits on them, and a schema
/// that is behind is retried until the migration lands.
pub async fn write_until_done(pg: Pg, regions: Vec<RegionConfig>) {
    let mut wait = Duration::from_secs(1);
    loop {
        match write(&pg, &regions).await {
            Ok(()) => return,
            Err(PgError::Fatal(e)) => {
                tracing::error!(error = %e, "region names not written");
                return;
            }
            Err(e) => tracing::warn!(error = %e, "region names not written yet; retrying"),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(RETRY_MAX);
    }
}

#[cfg(test)]
#[path = "region_names_tests.rs"]
mod tests;
