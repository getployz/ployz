//! Cloud's observations of the Servers it deploys to: what each holds or runs, read
//! through an entry of the connections it is given, with a failed check reported as
//! evidence rather than refused.

use std::sync::Arc;

use ployz_core::{MachineId, RpcError};
use ployz_store::{ObservationFailure, VolumeObservation};

use super::{Dialed, Session, connect_expecting};
use crate::connect::{ClusterExpectation, SystemConnector};
use crate::context::Connection;

/// Dial `connections`, read through the session, and close it. A check that cannot be made
/// becomes `failed(why)`: that the connection failed says whether no Server could be
/// reached or one answered with an error, naming that one only once its daemon confirmed
/// which Server it is.
///
/// # Errors
/// Returns `invalid_argument` when `connections` is empty.
async fn observe<T>(
    connections: Vec<Connection>,
    expect: Option<ClusterExpectation>,
    failed: impl Fn(ObservationFailure) -> T,
    read: impl AsyncFnOnce(&Session) -> Result<T, RpcError>,
) -> Result<T, RpcError> {
    if connections.is_empty() {
        return Err(RpcError::invalid_argument("connections must not be empty"));
    }
    let session = match super::dial(connections, Arc::new(SystemConnector::default()), expect).await
    {
        Ok(session) => session,
        Err(error) => {
            return Ok(failed(if error.unreached() {
                ObservationFailure::unreachable(error.into())
            } else {
                // The daemon that refused never confirmed which Server it is.
                ObservationFailure::answered(None, error.into())
            }));
        }
    };
    let observed = match read(&session).await {
        Ok(observed) => observed,
        Err(error) => {
            let server = session
                .client()
                .ok()
                .and_then(|client| client.entry_machine());
            failed(ObservationFailure::answered(server, error))
        }
    };
    session.close().await;
    Ok(observed)
}

/// Which of `connections`' Servers hold each Docker Volume in `sought`: the
/// evidence a Deploy that removes deployed Volumes is admitted with. A check that
/// fails names each of `connections`' Servers unanswered, and says whether none
/// could be reached or one answered with an error, naming that one only once its
/// daemon confirmed which Server it is.
///
/// # Errors
/// Returns `invalid_argument` when `connections` is empty.
pub async fn observe_volumes(
    connections: Vec<Connection>,
    sought: Vec<ployz_core::DockerVolumeName>,
) -> Result<VolumeObservation, RpcError> {
    observe_volumes_expecting(connections, sought, None).await
}

/// [`observe_volumes`] through an entry of `expect`'s Cluster only, when given.
///
/// # Errors
/// As [`observe_volumes`].
pub async fn observe_volumes_expecting(
    connections: Vec<Connection>,
    sought: Vec<ployz_core::DockerVolumeName>,
    expect: Option<ClusterExpectation>,
) -> Result<VolumeObservation, RpcError> {
    let servers = named(&connections);
    observe(
        connections,
        expect,
        |failure| VolumeObservation::failed(sought.clone(), servers.clone(), failure),
        async |session| session.observe_volumes(sought.clone()).await,
    )
    .await
}

/// Which Services run on `connections`' Servers, by Namespace: the evidence a full
/// Deploy's prune is admitted with, naming each Service it removes. A check that
/// fails names each of `connections`' Servers unanswered and says why, as
/// [`observe_volumes`] does.
///
/// # Errors
/// Returns `invalid_argument` when `connections` is empty.
pub async fn observe_running(
    connections: Vec<Connection>,
) -> Result<ployz_store::RunningObservation, RpcError> {
    observe_running_expecting(connections, None).await
}

/// [`observe_running`] through an entry of `expect`'s Cluster only, when given.
///
/// # Errors
/// As [`observe_running`].
pub async fn observe_running_expecting(
    connections: Vec<Connection>,
    expect: Option<ClusterExpectation>,
) -> Result<ployz_store::RunningObservation, RpcError> {
    let servers = named(&connections);
    observe(
        connections,
        expect,
        |failure| ployz_store::RunningObservation {
            services: Vec::new(),
            unanswered: servers.clone(),
            failure: Some(failure),
            dropped: Vec::new(),
        },
        async |session| session.observe_running().await,
    )
    .await
}

/// The Servers `connections` name, which a failed check reports unanswered.
fn named(connections: &[Connection]) -> Vec<MachineId> {
    connections
        .iter()
        .filter_map(Connection::machine_id)
        .copied()
        .collect()
}

/// Every copy of every Volume on `connections`' Servers, by role: what Cloud's sweep
/// finds orphaned mirror slots with. With `expect`, only through an entry of its
/// Cluster, naming the Machines it left.
///
/// # Errors
/// Returns the connection's error when no Server can be reached, or listing the
/// Machines fails.
pub async fn observe_copies(
    connections: Vec<Connection>,
    expect: Option<ClusterExpectation>,
) -> Result<Dialed<super::CopyObservation>, RpcError> {
    let connector = Arc::new(SystemConnector::default());
    let session = connect_expecting(connections, connector, expect).await?;
    let foreign = session.foreign();
    let observed = session.observe_copies().await;
    session.close().await;
    Ok(Dialed {
        outcome: observed?,
        foreign,
    })
}
