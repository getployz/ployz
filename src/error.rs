use crate::control::reconcile::MeshError;
use crate::dataplane::traits::PortError;
use crate::domain::identity::IdentityError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PloyzError {
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    Mesh(#[from] MeshError),
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
