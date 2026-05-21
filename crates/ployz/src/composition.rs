//! Composition layer that wires Ployz product ports to adapters.
//!
//! Feature modules should depend on Ployz-owned traits. This module is allowed
//! to assemble concrete adapters and pass them into product orchestration.

use crate::acme::CertificatePort;
use crate::deploy::{AuthorityPort, OperationPort};
use crate::projection::ProjectionPort;
use crate::runtime::RuntimePort;
use crate::serving::ServingPort;

pub struct FirstProofPorts<A, C, O, P, R, S, View> {
    pub authority: A,
    pub certificates: C,
    pub operations: O,
    pub projections: P,
    pub runtime: R,
    pub serving: S,
    _view: std::marker::PhantomData<View>,
}

impl<A, C, O, P, R, S, View> FirstProofPorts<A, C, O, P, R, S, View>
where
    A: AuthorityPort,
    C: CertificatePort,
    O: OperationPort,
    P: ProjectionPort<View>,
    R: RuntimePort,
    S: ServingPort,
{
    #[must_use]
    pub fn new(
        authority: A,
        certificates: C,
        operations: O,
        projections: P,
        runtime: R,
        serving: S,
    ) -> Self {
        Self {
            authority,
            certificates,
            operations,
            projections,
            runtime,
            serving,
            _view: std::marker::PhantomData,
        }
    }
}
