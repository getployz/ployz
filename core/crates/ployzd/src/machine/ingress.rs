//! Reserved Ingress Proxy validation at the Machine trust boundary.

use ployz_core::{Namespace, QualifiedService, ResolvedServiceSpec};

use super::LocalMachineError;

/// Validate one reserved Ingress Proxy Service immediately before creation.
///
/// # Errors
///
/// Returns when the reserved Service specification is invalid.
pub(crate) fn admit_ingress_service(
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
) -> Result<(), LocalMachineError> {
    if QualifiedService::new(namespace.clone(), spec.name.clone())
        == QualifiedService::system_ingress()
    {
        ployz_core::validate_ingress_service_spec(spec)?;
    }
    Ok(())
}
