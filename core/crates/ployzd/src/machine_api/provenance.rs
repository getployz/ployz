//! Who a call to the Machine API came from, said once at its edge and read back by name.
//!
//! `OPERATOR_SCOPE_HEADER` and `CLOUD_ROUTED_HEADER` are the marks the edge stamps on a call
//! and the Machine behind it reads. Only the presence of a mark counts, never its value.

use ployz_core::ManagementScope;
use tonic::codegen::http::{HeaderMap, HeaderValue};

/// The scope a call is admitted at, and whether Cloud routed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Provenance {
    scope: ManagementScope,
    cloud_routed: bool,
}

impl Provenance {
    /// The marks `headers` carry. A call with no Operator mark is a Full one.
    pub(super) fn of_headers(headers: &HeaderMap) -> Self {
        Self::reading(|name| headers.contains_key(name))
    }

    /// The marks `request` carries.
    pub(super) fn of<T>(request: &tonic::Request<T>) -> Self {
        Self::reading(|name| request.metadata().contains_key(name))
    }

    fn reading(carries: impl Fn(&str) -> bool) -> Self {
        let scope = if carries(ployz_core::OPERATOR_SCOPE_HEADER) {
            ManagementScope::Operator
        } else {
            ManagementScope::Full
        };
        Self::new(scope, carries(ployz_core::CLOUD_ROUTED_HEADER))
    }

    /// A call at `scope` that Cloud routed when `cloud_routed`.
    pub(super) fn new(scope: ManagementScope, cloud_routed: bool) -> Self {
        Self {
            scope,
            cloud_routed,
        }
    }

    pub(super) fn scope(self) -> ManagementScope {
        self.scope
    }

    pub(super) fn operator(self) -> bool {
        self.scope == ManagementScope::Operator
    }

    pub(super) fn cloud_routed(self) -> bool {
        self.cloud_routed
    }

    /// Sets these marks on a call about to reach the Machine. A mark a caller already sent
    /// stays unless this call is Full, which carries no Operator mark.
    pub(super) fn stamp(self, headers: &mut HeaderMap) {
        if self.cloud_routed {
            headers.insert(
                ployz_core::CLOUD_ROUTED_HEADER,
                HeaderValue::from_static("1"),
            );
        }
        match self.scope {
            ManagementScope::Operator => {
                headers.insert(
                    ployz_core::OPERATOR_SCOPE_HEADER,
                    HeaderValue::from_static("1"),
                );
            }
            ManagementScope::Full => {
                headers.remove(ployz_core::OPERATOR_SCOPE_HEADER);
            }
        }
    }
}
