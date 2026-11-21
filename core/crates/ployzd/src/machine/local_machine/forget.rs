//! Forget: a person's signed version marked to forget raises its Service's floor here, so
//! no approval made before it admits a Container of that Service again. Running Containers
//! keep running. The route only raises floors, so a resend is harmless.

use ployz_core::{
    ForgetOutcome, ForgetVersionsRequest, VersionsForgotten,
    authority::{AuthorityRefusal, Purpose, Timestamp, version},
};
use ployz_decide::service_floor::Raise;

use super::{Error, LocalMachine, admission};

impl LocalMachine {
    /// Raise the floor of each forget's Service that opens here, and answer each one.
    ///
    /// # Errors
    /// Returns [`Error::ForgetRefused`] with the first refusal when no forget opens,
    /// [`Error::NotParticipating`], and [`Error::ApprovalRecord`] when the floors cannot
    /// be kept.
    pub(crate) async fn forget_versions(
        &self,
        request: ForgetVersionsRequest,
    ) -> Result<VersionsForgotten, Error> {
        let record = self.record();
        record.require_participating()?;
        let cluster = self.cluster_key(record.phase());
        let opened = version::verify_forget(&request.versions, &record.trusted_keys(), cluster);
        let forgets: Vec<_> = opened
            .iter()
            .filter_map(|forget| forget.clone().ok())
            .collect();
        let Some(cluster) = cluster.filter(|_| !forgets.is_empty()) else {
            let refusal = opened.into_iter().find_map(Result::err);
            return Err(Error::ForgetRefused(refusal.unwrap_or(
                AuthorityRefusal::Unsigned {
                    purpose: Purpose::Workflow,
                },
            )));
        };
        let mut raised = self
            .approvals
            .raise_floors(cluster, &forgets, Timestamp::now())?
            .into_iter();
        let outcomes = opened
            .into_iter()
            .map(|forget| {
                let (forget, raise) = match forget {
                    Ok(forget) => (forget, raised.next().expect("one answer per forget")),
                    Err(refusal) => return refused(&refusal),
                };
                let service = forget.service;
                tracing::info!(%service, "forget: {raise:?}");
                match raise {
                    Ok(Raise::Raised(floor)) => ForgetOutcome::Raised { service, floor },
                    Ok(Raise::Held(floor)) => ForgetOutcome::Held { service, floor },
                    Err(ahead) => refused(&admission::floor_refusal(ahead)),
                }
            })
            .collect();
        Ok(VersionsForgotten { outcomes })
    }
}

fn refused(refusal: &AuthorityRefusal) -> ForgetOutcome {
    ForgetOutcome::Refused {
        reason: refusal_text(refusal),
    }
}

/// What a refused forget says to the person who asked for it.
pub(super) fn refusal_text(refusal: &AuthorityRefusal) -> String {
    if matches!(
        refusal,
        AuthorityRefusal::UnknownKey { .. } | AuthorityRefusal::UnknownPasskey
    ) {
        return "This server doesn't trust the device that signed this forget.".to_owned();
    }
    admission::refusal_text(refusal)
}
