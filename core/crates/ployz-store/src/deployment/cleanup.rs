//! Cleanup transitions share the Deployment row. Only Pending can renew or finish.

use super::{CleanupStatus, DeploymentId, LEASE};
use crate::error;
use crate::storage::{Backend, Param, Row, Storage, Tx};
use ployz_core::RpcError;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Cleanup {
    NotStarted,
    Pending(i64),
    Unconfirmed,
    Done,
}

impl Cleanup {
    pub(super) const fn status(self) -> Option<CleanupStatus> {
        match self {
            Self::Pending(_) => Some(CleanupStatus::Pending),
            Self::Unconfirmed => Some(CleanupStatus::Unconfirmed),
            Self::NotStarted | Self::Done => None,
        }
    }

    pub(super) fn decode(row: &Row, status: usize, deadline: usize) -> Result<Self, RpcError> {
        match row.optional_text(status)? {
            None => Ok(Self::NotStarted),
            Some("pending") => Ok(Self::Pending(row.int(deadline)?)),
            Some("unconfirmed") => Ok(Self::Unconfirmed),
            Some("done") => Ok(Self::Done),
            Some(_) => Err(error::corrupt("Deployment cleanup status")),
        }
    }
}

pub(super) fn pending_in_time(tx: &dyn Tx) -> String {
    let clock = tx.backend().wall_clock();
    format!("cleanup_status = 'pending' AND cleanup_deadline >= {clock}")
}

pub(super) fn renewed(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    start: bool,
) -> Result<Option<Cleanup>, RpcError> {
    let pending = pending_in_time(tx);
    let seconds = tx.backend().wall_seconds();
    let initial = if start {
        "cleanup_status IS NULL OR "
    } else {
        ""
    };
    tx.query(
        &format!(
            "UPDATE config_deployment SET cleanup_status = 'pending', cleanup_deadline = {seconds} + {LEASE}
             WHERE id = ?1 AND ({initial}({pending}))
             RETURNING cleanup_status, cleanup_deadline"
        ),
        &[id.as_str().into()],
    )?
    .first()
    .map(|row| Cleanup::decode(row, 0, 1))
    .transpose()
}

pub(super) fn finished(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Cleanup, RpcError> {
    let pending = pending_in_time(tx);
    let changed = tx.execute(
        &format!(
            "UPDATE config_deployment SET cleanup_status = 'done', cleanup_deadline = NULL
             WHERE id = ?1 AND {pending}"
        ),
        &[id.as_str().into()],
    )?;
    if changed != 0 {
        return Ok(Cleanup::Done);
    }
    let cleanup = expire_and_read(tx, id)?;
    if matches!(cleanup, Cleanup::Pending(_)) {
        return Err(RpcError::unavailable(
            "The Store could not confirm that cleanup ended",
        ));
    }
    Ok(cleanup)
}

pub(super) fn expire(tx: &mut dyn Tx, id: &DeploymentId) -> Result<(), RpcError> {
    let clock = tx.backend().wall_clock();
    tx.execute(
        &format!(
            "UPDATE config_deployment SET cleanup_status = 'unconfirmed'
             WHERE id = ?1 AND cleanup_status = 'pending' AND cleanup_deadline < {clock}"
        ),
        &[id.as_str().into()],
    )?;
    Ok(())
}

pub(super) fn expire_and_read(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Cleanup, RpcError> {
    expire(tx, id)?;
    read_state(tx, id)
}

fn read_state(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Cleanup, RpcError> {
    let rows = tx.query(
        "SELECT cleanup_status, cleanup_deadline FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    Cleanup::decode(rows.first().ok_or_else(|| super::missing(id))?, 0, 1)
}

/// Keep normal reads in their read-only snapshot. When one observes expired cleanup,
/// release that snapshot before conditionally persisting the transition and reading again.
pub(crate) fn read<T>(
    storage: &Storage,
    work: impl Fn(&mut dyn Tx) -> Result<T, RpcError>,
) -> Result<T, RpcError> {
    loop {
        let mut expired = std::collections::BTreeSet::new();
        let result = storage.read(|tx| {
            work(&mut Observe {
                tx,
                expired: &mut expired,
            })
        })?;
        if expired.is_empty() {
            return Ok(result);
        }
        storage.write(|tx| {
            for id in expired {
                expire(tx, &id)?;
            }
            Ok(())
        })?;
    }
}

struct Observe<'a> {
    tx: &'a mut dyn Tx,
    expired: &'a mut std::collections::BTreeSet<DeploymentId>,
}

impl Tx for Observe<'_> {
    fn backend(&self) -> Backend {
        self.tx.backend()
    }

    fn observe_expired_cleanup(&mut self, id: &DeploymentId) {
        self.expired.insert(id.clone());
    }

    fn execute(&mut self, sql: &str, params: &[Param<'_>]) -> Result<usize, RpcError> {
        self.tx.execute(sql, params)
    }

    fn query(&mut self, sql: &str, params: &[Param<'_>]) -> Result<Vec<Row>, RpcError> {
        self.tx.query(sql, params)
    }

    fn batch(&mut self, sql: &str) -> Result<(), RpcError> {
        self.tx.batch(sql)
    }

    fn wait_for_locks(&mut self, at_most: Option<std::time::Duration>) -> Result<(), RpcError> {
        self.tx.wait_for_locks(at_most)
    }
}
