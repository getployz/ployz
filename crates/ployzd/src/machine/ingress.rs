//! Reserved Ingress Proxy validation at the Machine trust boundary.

use ployz_core::{ProjectName, QualifiedService, ResolvedServiceSpec};

use super::LocalMachineError;

/// Validate one reserved Ingress Proxy Service immediately before creation.
///
/// # Errors
///
/// Returns when the reserved Service specification is invalid.
pub(crate) fn admit_ingress_service(
    project: &ProjectName,
    spec: &ResolvedServiceSpec,
) -> Result<(), LocalMachineError> {
    if QualifiedService::new(project.clone(), spec.name.clone())
        == QualifiedService::system_ingress()
    {
        ployz_core::validate_ingress_service_spec(spec)?;
    }
    Ok(())
}
