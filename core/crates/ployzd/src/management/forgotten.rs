//! The forgotten question on [`FORGOTTEN_ALPN`]: a Machine dials a member with its own
//! Management Identity and reads from the close code whether the Cluster forgot it
//! (#2927). No stream opens and no Management Client is needed, so a Server removed
//! while it was unreachable can still ask. The asker proves only that it holds the
//! identity it asks about, and the answer gives away nothing but one bit about that
//! identity, so answering an unknown key is safe.

use std::sync::Arc;

use async_trait::async_trait;
use iroh::{
    Endpoint, EndpointAddr, PublicKey, RelayUrl,
    endpoint::{Connection, ConnectionError, VarInt},
};
use ployz_core::{FORGOTTEN_ALPN, ForgottenAnswer, ManagementIdentity};

use crate::corrosion::ReplicatedStore;

/// Who this Machine consults for the answer: its view of the Cluster.
#[async_trait]
pub trait Roster: Send + Sync {
    /// None when the roster cannot be read right now; the asker hears NoAnswer.
    async fn forgotten(&self, identity: ManagementIdentity) -> Option<ForgottenAnswer>;
}

#[async_trait]
impl Roster for ReplicatedStore {
    async fn forgotten(&self, identity: ManagementIdentity) -> Option<ForgottenAnswer> {
        ReplicatedStore::forgotten(self, identity).await.ok()
    }
}

/// Answer one connection on [`FORGOTTEN_ALPN`] from its transport key and close it.
pub(super) async fn answer(connection: Connection, roster: Option<&Arc<dyn Roster>>) {
    let identity = ManagementIdentity::from_bytes(*connection.remote_id().as_bytes());
    let answer = match roster {
        Some(roster) => roster
            .forgotten(identity)
            .await
            .unwrap_or(ForgottenAnswer::NoAnswer),
        None => ForgottenAnswer::NoAnswer,
    };
    connection.close(VarInt::from_u32(answer.close_code()), b"forgotten answered");
}

/// Ask `member` whether the Cluster forgot the identity `endpoint` dials with. None
/// when the member cannot be reached, runs a daemon without this ALPN, or closes with
/// a code that is not an answer. The caller bounds how long this may take.
pub async fn ask(
    endpoint: &Endpoint,
    member: ManagementIdentity,
    relay: &RelayUrl,
) -> Option<ForgottenAnswer> {
    let member = PublicKey::from_bytes(member.as_bytes()).ok()?;
    let address = EndpointAddr::new(member).with_relay_url(relay.clone());
    let connection = endpoint.connect(address, FORGOTTEN_ALPN).await.ok()?;
    let ConnectionError::ApplicationClosed(close) = connection.closed().await else {
        return None;
    };
    ForgottenAnswer::from_close_code(u32::try_from(close.error_code.into_inner()).ok()?)
}
