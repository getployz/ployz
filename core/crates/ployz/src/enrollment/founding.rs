//! What founding a Cluster does once its first Server is ready, for `server add --standalone`
//! and Cloud enrollment alike. Each caller words a failure for the step that finishes it.

use ployz_core::{ClusterName, ClusterNamed, Hint, Machine};

use crate::connect::Client;
use crate::failure::Failure;
use crate::ingress::{Followed, NotFollowed};

/// Name the new Cluster, when a name was asked for. The Server is founded either way, so a
/// failure says so and names the command, built by `rerun`, that names it.
pub(crate) async fn name_cluster(
    ready: &Client,
    name: Option<ClusterName>,
    rerun: impl Fn(&[&str]) -> String,
) -> Option<Result<ClusterNamed, Failure>> {
    let name = name?;
    let next = rerun(&["cluster", "name", name.as_str()]);
    Some(
        crate::cluster::name::set(ready, name)
            .await
            .map_err(|error| {
                error
                    .context("Server initialized; the Cluster has no name yet.")
                    .hint(Hint::Next(next))
            }),
    )
}

/// Close the new Cluster's Doors when `closed`, then start `machine`'s Ingress Proxy when it
/// takes web traffic: `None` when it doesn't.
pub(crate) async fn open_ingress(
    ready: &mut Client,
    machine: &Machine,
    closed: bool,
) -> Result<Option<Vec<Followed>>, NotFollowed> {
    if closed {
        crate::ingress::declare_founding_closed(ready).await?;
    }
    if !machine.accepts_ingress {
        return Ok(None);
    }
    if closed {
        crate::ingress::found_closed(ready).await
    } else {
        crate::ingress::follow_roles(ready).await
    }
    .map(Some)
}
