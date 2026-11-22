//! The Cluster's name, read and set through its daemons.

use ployz_core::{
    ClusterName, ClusterNamed, Hint, READ_CLUSTER_NAME_CAPABILITY, ReadClusterNameRequest,
    RpcErrorCode, SET_CLUSTER_NAME_CAPABILITY, SetClusterNameRequest, op,
};
use serde_json::json;

use super::{Call, Client};
use crate::failure::Failure;

pub(crate) const VALID_NAME: &str =
    "Use up to 63 lowercase letters, digits and hyphens. Start and end with a letter or digit.";

pub(crate) fn parse(name: &str) -> Result<ClusterName, Failure> {
    ClusterName::parse(name).map_err(|_| {
        Failure::detailed(
            RpcErrorCode::InvalidArgument,
            VALID_NAME,
            json!({ "name": name }),
        )
    })
}

pub(crate) async fn set(client: &Client, name: ClusterName) -> Result<ClusterNamed, Failure> {
    require(client, SET_CLUSTER_NAME_CAPABILITY).await?;
    Ok(client
        .call_shared::<op::SetClusterName>(SetClusterNameRequest { name }, Call::once())
        .await
        .map_err(crate::connect::rpc_error)?)
}

pub(crate) async fn read(client: &Client) -> Result<ClusterNamed, Failure> {
    require(client, READ_CLUSTER_NAME_CAPABILITY).await?;
    Ok(client
        .call_shared::<op::ReadClusterName>(ReadClusterNameRequest {}, Call::once())
        .await
        .map_err(crate::connect::rpc_error)?)
}

async fn require(client: &Client, capability: &str) -> Result<(), Failure> {
    if client
        .supports(None, capability)
        .await
        .map_err(crate::connect::rpc_error)?
    {
        return Ok(());
    }
    Err(Failure::detailed(
        RpcErrorCode::Unsupported,
        "This Server's Ployz release can't name a Cluster.",
        json!({ "capability": capability }),
    )
    .hint(Hint::Next("ployz server upgrade".into())))
}
