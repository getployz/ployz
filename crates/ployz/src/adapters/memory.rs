//! In-memory adapters for tests and local composition.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::domain::{
    DomainFailure, DomainName, DomainPendingReason, DomainReadyRecord, DomainStatus,
    DomainStatusFailure, DomainStatusPort,
};
use crate::error::{MachineFailure, ServingFailure};
use crate::machine::{MachineId, MachineMembership, MachineMembershipPort, MachineStatus};
use crate::operation::{MutationContext, ScopeId};
use crate::serving::{
    RouteId, ServingCommitObservation, ServingCommitRequest, ServingCommittedSnapshot,
    ServingSnapshotPort,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct InMemoryMachineMembership {
    rows: Rc<RefCell<BTreeMap<(ScopeId, MachineId), MachineMembership>>>,
}

impl MachineMembershipPort for InMemoryMachineMembership {
    async fn observe(
        &self,
        context: &MutationContext,
        machine: &MachineId,
    ) -> Result<MachineStatus, MachineFailure> {
        let scope = context.authority().scope().clone();
        Ok(self
            .rows
            .borrow()
            .get(&(scope, machine.clone()))
            .cloned()
            .map(MachineStatus::Joined)
            .unwrap_or(MachineStatus::Absent))
    }

    async fn join(
        &self,
        context: &MutationContext,
        membership: &MachineMembership,
    ) -> Result<MachineStatus, MachineFailure> {
        let scope = context.authority().scope().clone();
        let mut rows = self.rows.borrow_mut();
        let current = rows
            .entry((scope, membership.machine.clone()))
            .or_insert_with(|| membership.clone())
            .clone();
        Ok(MachineStatus::Joined(current))
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InMemoryDomainStatus {
    rows: Rc<RefCell<DomainStatusRows>>,
}

#[derive(Debug, Default)]
struct DomainStatusRows {
    current_by_domain: BTreeMap<(ScopeId, DomainName), DomainStatus>,
}

impl InMemoryDomainStatus {
    fn record_status(&self, context: &MutationContext, domain: DomainName, status: DomainStatus) {
        let scope = context.authority().scope().clone();
        let mut rows = self.rows.borrow_mut();
        rows.current_by_domain.insert((scope, domain), status);
    }
}

impl DomainStatusPort for InMemoryDomainStatus {
    async fn status(
        &self,
        context: &MutationContext,
        domain: &DomainName,
    ) -> Result<DomainStatus, DomainFailure> {
        let scope = context.authority().scope().clone();
        Ok(self
            .rows
            .borrow()
            .current_by_domain
            .get(&(scope, domain.clone()))
            .cloned()
            .unwrap_or(DomainStatus::Unknown))
    }

    async fn record_pending(
        &self,
        context: &MutationContext,
        domain: &DomainName,
        reason: DomainPendingReason,
    ) -> Result<(), DomainFailure> {
        self.record_status(context, domain.clone(), DomainStatus::Pending(reason));
        Ok(())
    }

    async fn record_ready(
        &self,
        context: &MutationContext,
        ready: DomainReadyRecord,
    ) -> Result<(), DomainFailure> {
        self.record_status(context, ready.domain().clone(), DomainStatus::Ready(ready));
        Ok(())
    }

    async fn record_failed(
        &self,
        context: &MutationContext,
        domain: &DomainName,
        failure: DomainStatusFailure,
    ) -> Result<(), DomainFailure> {
        self.record_status(context, domain.clone(), DomainStatus::Failed(failure));
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InMemoryServingSnapshots {
    rows: Rc<RefCell<ServingSnapshotRows>>,
}

#[derive(Debug, Default)]
struct ServingSnapshotRows {
    current_by_route: BTreeMap<(ScopeId, RouteId), ServingCommitRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServingCommitRow {
    snapshot: ServingCommittedSnapshot,
}

impl ServingSnapshotRows {
    fn commit(
        &mut self,
        context: &MutationContext,
        request: &ServingCommitRequest,
    ) -> Result<(), ServingFailure> {
        let snapshot = ServingCommittedSnapshot::from_request(request)?;
        let row = ServingCommitRow { snapshot };
        self.current_by_route.insert(
            (context.authority().scope().clone(), request.route().clone()),
            row,
        );
        Ok(())
    }
}

impl ServingSnapshotPort for InMemoryServingSnapshots {
    async fn commit_snapshot(
        &self,
        context: &MutationContext,
        request: &ServingCommitRequest,
    ) -> Result<(), ServingFailure> {
        self.rows.borrow_mut().commit(context, request)
    }

    async fn commit_status(
        &self,
        context: &MutationContext,
        request: &ServingCommitRequest,
    ) -> Result<ServingCommitObservation, ServingFailure> {
        let scope = context.authority().scope().clone();
        Ok(
            match self
                .rows
                .borrow()
                .current_by_route
                .get(&(scope, request.route().clone()))
                .cloned()
            {
                Some(current) => ServingCommitObservation::Current(current.snapshot.clone()),
                None => ServingCommitObservation::Missing,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::acme::Hostname;
    use crate::machine::{
        IrohEndpointId, MachineEpoch, MachineNetworkIdentity, OverlayIp, WireGuardPublicKey,
    };
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, OperationId, PrincipalId, ScopeId,
    };
    use crate::serving::{ServingCommitMatch, ServingGeneration, ServingTarget};

    #[tokio::test(flavor = "current_thread")]
    async fn machine_membership_is_scoped_to_authority_context() {
        let rows = InMemoryMachineMembership::default();
        let machine = MachineId::parse("node-a").expect("machine");
        let first_scope = context_with_scope("machine-1", "idem-1", "cluster-a");
        let second_scope = context_with_scope("machine-2", "idem-2", "cluster-b");

        rows.join(&first_scope, &membership("node-a", 1, "fd00::1"))
            .await
            .expect("join first scope");

        assert!(matches!(
            rows.observe(&first_scope, &machine)
                .await
                .expect("first scope"),
            MachineStatus::Joined(joined)
                if joined == membership("node-a", 1, "fd00::1")
        ));
        assert_eq!(
            rows.observe(&second_scope, &machine)
                .await
                .expect("second scope"),
            MachineStatus::Absent
        );

        rows.join(&second_scope, &membership("node-a", 2, "fd00::2"))
            .await
            .expect("join second scope");

        assert!(matches!(
            rows.observe(&second_scope, &machine)
                .await
                .expect("second scope after join"),
            MachineStatus::Joined(joined)
                if joined == membership("node-a", 2, "fd00::2")
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn serving_latest_row_wins_even_for_same_generation() {
        let snapshots = InMemoryServingSnapshots::default();
        let initial_context = context("deploy-1", "idem-1");
        let first = request(7, "gateway-a");
        let replacement = request(7, "gateway-b");

        snapshots
            .commit_snapshot(&initial_context, &first)
            .await
            .expect("initial commit");

        snapshots
            .commit_snapshot(&context("deploy-2", "idem-2"), &replacement)
            .await
            .expect("replacement commit");
        assert_eq!(
            snapshots
                .commit_status(&context("observe-1", "observe-1"), &first)
                .await
                .expect("status")
                .try_confirm_commit(&first),
            Ok(ServingCommitMatch::DifferentCurrent)
        );
        assert_eq!(
            snapshots
                .commit_status(&context("observe-2", "observe-2"), &replacement)
                .await
                .expect("status")
                .try_confirm_commit(&replacement),
            Ok(ServingCommitMatch::Current)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn serving_commit_is_idempotent_for_same_context_and_request() {
        let snapshots = InMemoryServingSnapshots::default();
        let context = context("deploy-1", "idem-1");
        let request = request(7, "gateway-a");

        snapshots
            .commit_snapshot(&context, &request)
            .await
            .expect("initial commit");
        snapshots
            .commit_snapshot(&context, &request)
            .await
            .expect("idempotent replay");

        assert_eq!(
            snapshots
                .commit_status(&context, &request)
                .await
                .expect("status")
                .try_confirm_commit(&request),
            Ok(ServingCommitMatch::Current)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn serving_current_row_moves_after_newer_commit() {
        let snapshots = InMemoryServingSnapshots::default();
        let initial_context = context("deploy-1", "idem-1");
        let first = request(7, "gateway-a");
        let newer = request(8, "gateway-b");

        snapshots
            .commit_snapshot(&initial_context, &first)
            .await
            .expect("initial commit");
        snapshots
            .commit_snapshot(&context("deploy-2", "idem-2"), &newer)
            .await
            .expect("newer commit");

        assert_eq!(
            snapshots
                .commit_status(&context("observe-1", "observe-1"), &first)
                .await
                .expect("status")
                .try_confirm_commit(&first),
            Ok(ServingCommitMatch::DifferentCurrent)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn serving_latest_row_wins_for_older_commit() {
        let snapshots = InMemoryServingSnapshots::default();
        let first = request(7, "gateway-a");
        let stale = request(6, "gateway-a");

        snapshots
            .commit_snapshot(&context("deploy-1", "idem-1"), &first)
            .await
            .expect("initial commit");

        snapshots
            .commit_snapshot(&context("deploy-2", "idem-2"), &stale)
            .await
            .expect("stale generation still becomes latest");
        assert_eq!(
            snapshots
                .commit_status(&context("observe-2", "observe-2"), &stale)
                .await
                .expect("status")
                .try_confirm_commit(&stale),
            Ok(ServingCommitMatch::Current)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn domain_status_is_scoped_to_authority_context() {
        let records = InMemoryDomainStatus::default();
        let domain = DomainName::parse("app.example.com").expect("domain");
        let first_scope = context_with_scope("domain-1", "idem-1", "cluster-a");
        let second_scope = context_with_scope("domain-2", "idem-2", "cluster-b");

        records
            .record_pending(
                &first_scope,
                &domain,
                DomainPendingReason::CertificateIssuance,
            )
            .await
            .expect("pending");
        records
            .record_failed(
                &second_scope,
                &domain,
                DomainStatusFailure::ServingActivationFailed,
            )
            .await
            .expect("failed");

        assert_eq!(
            records
                .status(&first_scope, &domain)
                .await
                .expect("first scope"),
            DomainStatus::Pending(DomainPendingReason::CertificateIssuance)
        );
        assert_eq!(
            records
                .status(&second_scope, &domain)
                .await
                .expect("second scope"),
            DomainStatus::Failed(DomainStatusFailure::ServingActivationFailed)
        );
    }

    fn context(operation: &str, idempotency: &str) -> MutationContext {
        context_with_scope(operation, idempotency, "cluster")
    }

    fn context_with_scope(operation: &str, idempotency: &str, scope: &str) -> MutationContext {
        MutationContext::new(
            OperationId::parse(operation).expect("operation"),
            IdempotencyKey::parse(idempotency).expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse(scope).expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            SystemTime::now() + Duration::from_secs(60),
        )
    }

    fn membership(machine: &str, epoch: u64, overlay_ip: &str) -> MachineMembership {
        MachineMembership::new(
            MachineId::parse(machine).expect("machine"),
            MachineEpoch::new(epoch).expect("epoch"),
            MachineNetworkIdentity::new(
                OverlayIp::parse(overlay_ip).expect("overlay ip"),
                IrohEndpointId::parse(format!("endpoint-{machine}")).expect("endpoint"),
                WireGuardPublicKey::parse(format!("wireguard-{machine}")).expect("wireguard"),
            ),
        )
    }

    fn request(generation: u64, target: &str) -> ServingCommitRequest {
        ServingCommitRequest::replace_current_route(
            RouteId::parse("route-a").expect("route"),
            Hostname::parse("app.example.com").expect("hostname"),
            ServingTarget::parse(target).expect("target"),
            ServingGeneration::new(generation),
        )
    }
}
