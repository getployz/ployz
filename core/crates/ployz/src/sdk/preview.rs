//! Closing previews from Cloud's side. A preview the Store closes is closed on its home
//! Server, which destroys the data the preview made there. A Server that still holds open
//! a preview the Store reports closed or never knew, such as one whose Close never reached
//! it, is told to close it the next time Cloud sweeps.

use std::sync::Arc;
use std::time::Duration;

use ployz_core::{
    CLOSE_PREVIEW_CAPABILITY, Claim, ClosePreviewRequest, DockerVolumeId, LIST_PREVIEWS_CAPABILITY,
    ListPreviewsRequest, MachineId, MachineObservation, MachineTarget, Namespace,
    PREVIEW_HOME_CAPABILITY, PreviewClosed, PreviewNamespace, RpcError, VolumeRemoval,
    VolumeRemovalOutcome, op,
};
use ployz_store::{Actor, ConfigStore, OpenPreviewsQuery};
use serde::Serialize;
use ts_rs::TS;

use super::{Dialed, Session, connect_expecting};
use crate::cluster::{Call, Deadline};
use crate::connect::{Client, SystemConnector};
use crate::context::Connection;

/// How long a Close may take to answer: it removes the preview's Containers and destroys
/// its Volumes one by one.
const CLOSE_REPLY_TIMEOUT: Duration = Duration::from_secs(300);

/// What a sweep of the Cluster's previews did.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, TS)]
pub struct PreviewsSwept {
    /// Previews a Server held open that the Store doesn't, now closed there.
    pub closed: Vec<PreviewOn>,
    /// Those whose Close failed: the next sweep tries again.
    pub failed: Vec<PreviewFailed>,
    /// Servers that didn't say which previews they hold.
    #[ts(as = "Vec<String>")]
    pub unanswered: Vec<MachineId>,
    /// Cloud couldn't check that the Servers are the Organization's Cluster, so it asked
    /// none of them and closed nothing.
    pub unverified: bool,
}

/// The Servers that can hold a preview.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, TS)]
pub struct PreviewHomes {
    pub homes: Vec<MachineId>,
}

/// One preview on one Server.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct PreviewOn {
    #[ts(as = "String")]
    pub machine: MachineId,
    #[ts(as = "String")]
    pub namespace: Namespace,
}

/// A Close that failed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct PreviewFailed {
    #[serde(flatten)]
    pub preview: PreviewOn,
    pub error: String,
}

/// Close, on each Server of the Cluster `connections` reach, every preview it holds open
/// that the Store doesn't report open for `who`'s Organization, and finish every Close
/// one began. Each Server's list is read before the Store, and a preview's Namespace is
/// never used again, so a preview the Store opens meanwhile is never one this closes.
/// A Close destroys data, so it goes only to a Cluster whose Sealing key the dial matched
/// to `expect`: without one, or past an entry that shows no key, it closes nothing.
///
/// # Errors
/// Returns the connection's error when no Server can be reached, or a storage error.
pub async fn close_unheld_previews(
    store: Arc<ConfigStore>,
    who: Actor,
    connections: Vec<Connection>,
    expect: Option<crate::connect::ClusterExpectation>,
) -> Result<Dialed<PreviewsSwept>, RpcError> {
    let unverified = PreviewsSwept {
        unverified: true,
        ..PreviewsSwept::default()
    };
    let Some(expect) = expect else {
        return Ok(Dialed::undialed(unverified));
    };
    let connector = Arc::new(SystemConnector::default());
    let session = connect_expecting(connections, connector, Some(expect)).await?;
    let foreign = session.foreign();
    if !session.cluster().is_some_and(|check| check.verified) {
        session.close().await;
        return Ok(Dialed {
            outcome: unverified,
            foreign,
        });
    }
    let swept = async {
        let mut client = session.client()?;
        let machines = client.machines().await.map_err(crate::connect::rpc_error)?;
        sweep(&client, &machines, |namespaces| {
            let store = Arc::clone(&store);
            let who = who.clone();
            async move {
                let query = OpenPreviewsQuery { namespaces };
                super::store_call(&store, move |store| store.read(&who, &query))
                    .await
                    .map(|view| view.open)
            }
        })
        .await
    }
    .await;
    session.close().await;
    Ok(Dialed {
        outcome: swept?,
        foreign,
    })
}

/// The Servers of the Cluster `connections` reach that can hold a preview: each takes
/// Services, isn't leaving, and advertises a preview's home and its Close. A Server that
/// is down now still counts, so a preview isn't moved for an outage. With `expect`, only
/// through an entry of its Cluster.
///
/// # Errors
/// Returns the connection's error when no Server can be reached, or listing them fails.
pub async fn observe_preview_homes(
    connections: Vec<Connection>,
    expect: Option<crate::connect::ClusterExpectation>,
) -> Result<Dialed<PreviewHomes>, RpcError> {
    let connector = Arc::new(SystemConnector::default());
    let session = connect_expecting(connections, connector, expect).await?;
    let foreign = session.foreign();
    let listed = async {
        let mut client = session.client()?;
        client.machines().await.map_err(crate::connect::rpc_error)
    }
    .await;
    session.close().await;
    Ok(Dialed {
        outcome: PreviewHomes {
            homes: homes(&listed?),
        },
        foreign,
    })
}

/// The keys the Server `connections` reach first trusts, as its own record lists them:
/// what a preview rule's signature checks against before the Store keeps it. With
/// `expect`, only through an entry of its Cluster.
///
/// # Errors
/// Returns the connection's error when no Server can be reached, or Inspect's.
pub async fn observe_signers(
    connections: Vec<Connection>,
    expect: Option<crate::connect::ClusterExpectation>,
) -> Result<Dialed<ployz_store::Signers>, RpcError> {
    let connector = Arc::new(SystemConnector::default());
    let session = connect_expecting(connections, connector, expect).await?;
    let foreign = session.foreign();
    let details = session.inspect().await;
    session.close().await;
    let details = details?;
    Ok(Dialed {
        outcome: ployz_store::Signers {
            devices: details.devices,
            passkeys: details.passkeys,
            grants: details.grants,
        },
        foreign,
    })
}

fn homes(machines: &[MachineObservation]) -> Vec<MachineId> {
    machines
        .iter()
        .map(|observed| &observed.machine)
        .filter(|machine| {
            machine.accepts_services
                && machine.takes_part()
                && [PREVIEW_HOME_CAPABILITY, CLOSE_PREVIEW_CAPABILITY]
                    .iter()
                    .all(|capability| machine.runtime.claim(capability) == Claim::Advertises)
        })
        .map(|machine| machine.id)
        .collect()
}

async fn sweep<F, Fut>(
    client: &Client,
    machines: &[MachineObservation],
    open: F,
) -> Result<PreviewsSwept, RpcError>
where
    F: FnOnce(Vec<Namespace>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Namespace>, RpcError>>,
{
    let mut swept = PreviewsSwept::default();
    let mut held = Vec::new();
    let mut closing = Vec::new();
    for machine in machines {
        let id = &machine.machine.id;
        if !machine.invites_rpc()
            || !machine
                .machine
                .runtime
                .claim(LIST_PREVIEWS_CAPABILITY)
                .may_ask()
        {
            continue;
        }
        let listed = client
            .call_shared::<op::ListPreviews>(
                ListPreviewsRequest {},
                Call::once()
                    .to(&MachineTarget::from(id))
                    .within(Deadline::Target(crate::connect::target_read_timeout())),
            )
            .await;
        let Ok(listed) = listed else {
            swept.unanswered.push(*id);
            continue;
        };
        let on = |namespace| PreviewOn {
            machine: *id,
            namespace,
        };
        held.extend(listed.open.into_iter().map(on));
        closing.extend(listed.closing.into_iter().map(on));
    }
    // A Close that began never ends any other way, whatever the Store holds.
    let mut closes = closing;
    if !held.is_empty() {
        let open = open(held.iter().map(|on| on.namespace.clone()).collect()).await?;
        closes.extend(held.into_iter().filter(|on| !open.contains(&on.namespace)));
    }
    for on in closes {
        match close_on(client, &on.machine, &on.namespace).await {
            Ok(_) => swept.closed.push(on),
            Err(error) => swept.failed.push(PreviewFailed {
                preview: on,
                error: error.message,
            }),
        }
    }
    Ok(swept)
}

async fn close_on(
    client: &Client,
    machine: &MachineId,
    namespace: &Namespace,
) -> Result<PreviewClosed, RpcError> {
    client
        .call_shared::<op::ClosePreview>(
            ClosePreviewRequest {
                namespace: namespace.clone(),
            },
            Call::once()
                .to(&MachineTarget::from(machine))
                .within(Deadline::MutationReply(CLOSE_REPLY_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)
}

impl Session {
    /// Close preview `namespace` on its home Server, reporting each of `deletes` there:
    /// removed, unless the Close failed or kept it. A home the Cluster doesn't list as
    /// answering is reported omitted, so the removal stays to be retried. Any of
    /// `deletes` on another Server is removed as usual.
    pub(crate) async fn close_preview(
        &self,
        namespace: &Namespace,
        deletes: Vec<DockerVolumeId>,
    ) -> Vec<VolumeRemoval> {
        let Some(preview) = PreviewNamespace::of(namespace) else {
            return super::store_runner::remove_volumes(self, deletes).await;
        };
        let (home, elsewhere): (Vec<_>, Vec<_>) = deletes
            .into_iter()
            .partition(|id| preview.homed_on(&id.machine_id));
        let outcome = self.closed_on_home(namespace, &preview).await;
        let mut removed = removals(home, |name| match &outcome {
            None => VolumeRemovalOutcome::Omitted,
            Some(Ok(closed)) if closed.kept.contains(name) => VolumeRemovalOutcome::Failed {
                error: RpcError::conflict(
                    "No Ployz Pool holds this Volume, so the preview's Close kept it",
                ),
            },
            Some(Ok(_)) => VolumeRemovalOutcome::Removed,
            Some(Err(error)) => VolumeRemovalOutcome::Failed {
                error: error.clone(),
            },
        });
        removed.extend(super::store_runner::remove_volumes(self, elsewhere).await);
        removed
    }

    async fn closed_on_home(
        &self,
        namespace: &Namespace,
        preview: &PreviewNamespace,
    ) -> Option<Result<PreviewClosed, RpcError>> {
        let mut client = self.client().ok()?;
        let machines =
            self.until_closed(async { client.machines().await.map_err(crate::connect::rpc_error) });
        let machines = machines.await.ok()?;
        let home = machines.iter().find(|machine| {
            preview.homed_on(&machine.machine.id)
                && machine.invites_rpc()
                && machine
                    .machine
                    .runtime
                    .claim(CLOSE_PREVIEW_CAPABILITY)
                    .may_ask()
        })?;
        let id = home.machine.id;
        Some(self.until_closed(close_on(&client, &id, namespace)).await)
    }
}

fn removals(
    deletes: Vec<DockerVolumeId>,
    outcome: impl Fn(&ployz_core::DockerVolumeName) -> VolumeRemovalOutcome,
) -> Vec<VolumeRemoval> {
    deletes
        .into_iter()
        .map(|id| VolumeRemoval {
            outcome: outcome(&id.name),
            id,
        })
        .collect()
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
