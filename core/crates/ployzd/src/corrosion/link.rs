//! This Machine's link to its Cluster: handles to the Corrosion it runs, held by the planes.

use tokio_util::sync::CancellationToken;

use super::{AdminClient, MachineView, ReplicatedStore, RunningCorrosion};
use crate::machine::RecordOwner;

/// The replicated store and admin client of this Machine's Corrosion, with the store
/// answering for the Local Machine record's authority.
#[derive(Clone)]
pub(crate) struct ClusterContext {
    pub(crate) replicated: ReplicatedStore,
    pub(crate) admin: AdminClient,
}

impl ClusterContext {
    /// `replicated` and `admin` as one Machine's planes use them; `owner`'s record decides
    /// which Machine rows the store accepts.
    pub(crate) fn new(replicated: ReplicatedStore, admin: AdminClient, owner: RecordOwner) -> Self {
        Self {
            replicated: replicated.with_authority(owner),
            admin,
        }
    }
}

/// Everything a plane needs from the Cluster. It holds handles only: the running
/// Corrosion stays with the daemon, whose drop stops keeping the container up.
#[derive(Clone)]
pub(crate) struct ClusterLink {
    pub(crate) context: ClusterContext,
    pub(crate) view: MachineView,
}

impl ClusterLink {
    /// The link to the Corrosion `running`, starting the Machines view that stops with
    /// `shutdown`.
    pub(crate) fn start(
        running: &RunningCorrosion,
        owner: RecordOwner,
        shutdown: CancellationToken,
    ) -> Self {
        let context = ClusterContext::new(running.store().clone(), running.admin_client(), owner);
        let view = MachineView::start(context.replicated.clone(), shutdown);
        Self { context, view }
    }

    /// The store and the Machines view read through it, as the publisher and the mesh take them.
    pub(crate) fn observed(&self) -> (ReplicatedStore, MachineView) {
        (self.context.replicated.clone(), self.view.clone())
    }
}
