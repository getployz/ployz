//! How an agent reaches the control plane. In `all` mode the link is a function call into the
//! plane; in `agent` mode it is HTTP/1.1 JSON over the platform's private network.

pub mod http;
pub(crate) mod wire;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::control::plane::ControlPlane;
use crate::domain::{Job, Observation, Region, UnixMs};

/// How long a pull may wait for work before the plane answers empty.
pub const PULL_WAIT: Duration = Duration::from_secs(10);

pub trait ControlLink: Send + Sync + 'static {
    fn pull(&self, want: u32) -> impl Future<Output = Result<Pulled, LinkError>> + Send;
    fn report(&self, batch: &ResultBatch) -> impl Future<Output = Result<(), LinkError>> + Send;
}

#[derive(Debug)]
pub struct Pulled {
    pub jobs: Vec<Job>,
    /// The plane's clock when it answered, so agents can start probes on its schedule.
    pub server_now: UnixMs,
}

impl Pulled {
    #[must_use]
    pub fn empty() -> Pulled {
        Pulled {
            jobs: Vec::new(),
            server_now: UnixMs::now(),
        }
    }
}

#[derive(Debug)]
pub struct ResultBatch {
    pub id: Uuid,
    pub observations: Vec<Observation>,
}

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The plane could not be reached or did not answer properly. Retry with the same batch.
    #[error("the control plane is unreachable: {0}")]
    Broken(String),
    /// The plane answered and refused. Retrying the same batch would get the same answer.
    #[error("the control plane rejected the batch: {0}")]
    Rejected(Rejected),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Rejected {
    #[error("it has not loaded its checks yet")]
    NotReady,
    #[error("it answered HTTP {0}")]
    Status(u16),
}

/// The in-process link `all` mode uses: one region, no token, no serialization.
pub struct LocalLink {
    plane: Arc<ControlPlane>,
    region: Region,
}

impl LocalLink {
    #[must_use]
    pub fn new(plane: Arc<ControlPlane>, region: Region) -> LocalLink {
        LocalLink { plane, region }
    }
}

impl ControlLink for LocalLink {
    async fn pull(&self, want: u32) -> Result<Pulled, LinkError> {
        Ok(self.plane.pull(&self.region, want, PULL_WAIT).await)
    }

    async fn report(&self, batch: &ResultBatch) -> Result<(), LinkError> {
        let copy = ResultBatch {
            id: batch.id,
            observations: batch.observations.clone(),
        };
        self.plane
            .report(&self.region, copy)
            .await
            .map_err(LinkError::Rejected)
    }
}
