use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use ployz_core::{
    ContainerDetails, ContainerId, ContainerLimitsUpdated, ContainerNow, ContainerObservation,
    ContainerPlacementUpdated, ContainerWant, DockerVolumeId, EnsuredContainer, HealthVerdict,
    ImagePulled, IngressDoorsSet, MachineId, PrivateAccessSet, QualifiedService,
    ResolvedServiceSpec, Retry, RoutesRead, RpcError, RpcErrorCode, SealedLogin,
    SetIngressDoorsRequest, SetPrivateAccessRequest, SlotKey,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{CopyLifecycle, DeployOperations, Listed, ProxyPlacement, RedialFailure, Session};

pub(in crate::deploy) const RESTART_WAIT: Duration = Duration::from_secs(60);
pub(in crate::deploy) const RESTART_POLL: Duration = Duration::from_millis(250);

// Wait out a target daemon restart on the same Machine. A new Container's
// creation key makes a resent Running find the Container the lost reply made.
// An entry daemon restart closes the session every call goes out on, so the
// same connection is dialed again. Do not walk to another connection; that is
// a different command-entry problem.
pub(crate) struct RestartTolerant<'a, C> {
    inner: &'a C,
    cancellation: &'a CancellationToken,
    /// How many times the entry was dialed again. Calls that met the same closed
    /// session dial it once between them.
    sessions: tokio::sync::Mutex<u64>,
}

impl<'a, C: Session> RestartTolerant<'a, C> {
    pub(crate) fn new(inner: &'a C, cancellation: &'a CancellationToken) -> Self {
        Self {
            inner,
            cancellation,
            sessions: tokio::sync::Mutex::new(0),
        }
    }

    async fn session(&self) -> u64 {
        *self.sessions.lock().await
    }

    /// Dial the entry again, unless another call already has since `seen`.
    async fn redial_since(&self, seen: u64) -> Option<Result<(), RedialFailure>> {
        let mut sessions = self.sessions.lock().await;
        if *sessions != seen {
            return Some(Ok(()));
        }
        let answer = self.inner.redial().await;
        if matches!(answer, Some(Ok(()))) {
            *sessions += 1;
        }
        answer
    }
}

impl<C: Session> RestartTolerant<'_, C> {
    /// `op`'s answer, sent again while the Machine restarts.
    async fn waited<T, Fut>(&self, op: impl FnMut() -> Fut) -> Result<T, RpcError>
    where
        Fut: Future<Output = Result<T, RpcError>>,
    {
        let seen = &AtomicU64::new(self.session().await);
        wait_out_restart(self.cancellation, op, move || async move {
            let answer = self.redial_since(seen.load(Ordering::Relaxed)).await;
            seen.store(self.session().await, Ordering::Relaxed);
            answer
        })
        .await
    }
}

/// Whether a creation's sends may still make a Container on their Machine: one went out
/// and lost its answer, or was cut off while out. Once set it stays set, since a later
/// answer speaks only for its own send.
#[derive(Default)]
pub(crate) struct Outstanding {
    out: AtomicBool,
    lost: AtomicBool,
}

impl Outstanding {
    pub(crate) fn any(&self) -> bool {
        self.out.load(Ordering::Relaxed) || self.lost.load(Ordering::Relaxed)
    }
}

impl<C: CopyLifecycle + Session> RestartTolerant<'_, C> {
    /// [`CopyLifecycle::ensure_new`], sent again while the Machine restarts, noting in
    /// `outstanding` every send that may still act.
    pub(crate) async fn ensure_new_noting(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        outstanding: &Outstanding,
    ) -> Result<EnsuredContainer, RpcError> {
        self.waited(|| async {
            outstanding.out.store(true, Ordering::Relaxed);
            let answer = self.inner.ensure_new(machine_id, want.clone()).await;
            outstanding.out.store(false, Ordering::Relaxed);
            if let Err(error) = &answer
                && (error.reply_lost() || error.target_unanswered())
            {
                outstanding.lost.store(true, Ordering::Relaxed);
            }
            answer
        })
        .await
    }
}

impl<C> Session for RestartTolerant<'_, C> {}

// Each method names its restart policy. `wait` resends while the Machine is
// restarting, so its daemon side must converge on a resend. `once` sends one
// time and names why a resend is unsafe. A row without a policy does not parse.
macro_rules! restart_policy {
    ($($trait:ident { $($policy:ident $(($reason:literal))? fn $name:ident(&self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)* })*) => {
        $(
            impl<C: $trait> $trait for RestartTolerant<'_, C> {
                $(
                    async fn $name(&self $(, $arg: $ty)*) -> $ret {
                        restart_policy!(@send $policy $(($reason))? self $name $($arg)*)
                    }
                )*
            }
        )*
    };
    (@send wait $self:ident $name:ident $($arg:ident)*) => {{
        $self.waited(|| $self.inner.$name($(Clone::clone(&$arg)),*)).await
    }};
    (@send once($reason:literal) $self:ident $name:ident $($arg:ident)*) => {
        $self.inner.$name($($arg),*).await
    };
}

restart_policy! {
    CopyLifecycle {
        wait fn service_containers(&self, service: &QualifiedService) -> Result<Vec<ContainerObservation>, RpcError>;
        wait fn listed_copies(&self, service: &QualifiedService) -> Result<Listed, RpcError>;
        wait fn target_copies(&self, machine_id: &MachineId, service: &QualifiedService) -> Result<Vec<ContainerObservation>, RpcError>;
        once("a failed read leaves the gen to the live answers") fn replicated_containers(&self) -> Result<Vec<ContainerObservation>, RpcError>;
        wait fn ensure_container(&self, machine_id: &MachineId, want: ContainerWant) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError>;
        wait fn ensure_unsettled(&self, machine_id: &MachineId, want: ContainerWant, timeout: Option<Duration>) -> Result<EnsuredContainer, RpcError>;
        wait fn ensure_new(&self, machine_id: &MachineId, want: ContainerWant) -> Result<EnsuredContainer, RpcError>;
        wait fn inspect_container(&self, machine_id: &MachineId, container_id: &ContainerId) -> Result<ContainerDetails, RpcError>;
        wait fn read_routes(&self, machine_id: &MachineId, service: &QualifiedService) -> Result<RoutesRead, RpcError>;
        wait fn read_entry_routes(&self, service: &QualifiedService) -> Result<RoutesRead, RpcError>;
    }
    DeployOperations {
        wait fn prepare_volumes(&self, machine_id: &MachineId, specs: &[ployz_core::ServiceStorageSpec]) -> Result<ployz_core::PreparedVolumes, RpcError>;
        wait fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError>;
        wait fn update_container_limits(&self, machine_id: &MachineId, container_id: &ContainerId, spec: &ResolvedServiceSpec) -> Result<ContainerLimitsUpdated, RpcError>;
        wait fn update_container_placement(&self, machine_id: &MachineId, container_id: &ContainerId, spec: &ResolvedServiceSpec) -> Result<ContainerPlacementUpdated, RpcError>;
        wait fn runs_global(&self, machine_id: &MachineId, slot: &SlotKey, spec: &ResolvedServiceSpec) -> Result<bool, RpcError>;
        wait fn ensure_image(&self, machine_id: &MachineId, spec: &ResolvedServiceSpec, auth: Option<SealedLogin>) -> Result<ImagePulled, RpcError>;
    }
    ProxyPlacement {
        wait fn set_ingress_doors(&self, machine_id: &MachineId, request: SetIngressDoorsRequest) -> Result<IngressDoorsSet, RpcError>;
        wait fn set_private_access(&self, machine_id: &MachineId, request: SetPrivateAccessRequest) -> Result<PrivateAccessSet, RpcError>;
    }
}

pub(in crate::deploy) async fn wait_out_restart<T, Fut, Redial>(
    cancellation: &CancellationToken,
    mut op: impl FnMut() -> Fut,
    mut redial: impl FnMut() -> Redial,
) -> Result<T, RpcError>
where
    Fut: Future<Output = Result<T, RpcError>>,
    Redial: Future<Output = Option<Result<(), RedialFailure>>>,
{
    let deadline = Instant::now() + RESTART_WAIT;
    // What earlier attempts showed of the Machine. A real answer clears the other two, and
    // a lost reply and a silence each clear the other. A Machine that answered was not
    // silent throughout: its answer stands in for a later silence, as `Client::repeat` keeps it.
    let mut answer: Option<RpcError> = None;
    // A send whose answer was lost: the Machine may have done it.
    let mut lost: Option<RpcError> = None;
    // A Machine that didn't answer: stronger news than an earlier lost reply, as
    // `keep_lost_call` holds. That fold and `keep_target_reply` are this one's twin for
    // `Client::repeat`; they differ only in a refused redial, where this wait keeps saying
    // what the lost send may have done (`still_lost`) and `keep_lost_call` does not.
    let mut silent: Option<RpcError> = None;
    loop {
        let error = match op().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        // An ended client's refusal of its entry never reached the Machine and says nothing
        // of it: it ends the wait in its own words, still saying what earlier sends may have
        // done. Other unreached failures, as a refused dial, go through the bookkeeping below
        // as before.
        if error.never_reached() && error.code == RpcErrorCode::Unauthenticated {
            return Err(still_lost(error, lost.as_ref().or(silent.as_ref())));
        }
        let closed = crate::connect::on_closed_session(&error);
        // A send that never reached the Machine, or met a closed session, says nothing of
        // it, so it is never kept.
        let unreached = error.never_reached() || closed;
        if error.reply_lost() {
            silent = None;
            lost = Some(error.clone());
        } else if error.target_unanswered() && !unreached {
            lost = None;
            silent = Some(error.clone());
        } else if !unreached {
            lost = None;
            silent = None;
            answer = Some(error.clone());
        }
        let reported = lost
            .clone()
            .or_else(|| answer.clone())
            .or_else(|| silent.clone())
            .unwrap_or_else(|| error.clone());
        let retry = if unreached { &error } else { &reported }.code.retry();
        let error = reported;
        let again = retry != Retry::Never
            && Instant::now() < deadline
            && !cancellation.is_cancelled()
            // Nothing sent on a closed session reaches a daemon again.
            && (!closed
                || redialed(&mut redial, deadline, cancellation)
                    .await
                    .map_err(|refusal| still_lost(refusal, lost.as_ref().or(silent.as_ref())))?);
        if !again {
            return Err(error);
        }
        tokio::select! {
            () = cancellation.cancelled() => return Err(error),
            () = tokio::time::sleep(RESTART_POLL) => {}
        }
    }
}

/// `refusal`, still saying the Machine may have done the `earlier` send whose reply was
/// lost, or that it left unanswered.
fn still_lost(refusal: RpcError, earlier: Option<&RpcError>) -> RpcError {
    let machine =
        earlier.and_then(|earlier| earlier.details.get("machine_id")?.as_str()?.parse().ok());
    match machine {
        Some(machine) => refusal.reply_lost_from(&machine),
        None => refusal,
    }
}

/// Dial the entry again until it answers, `deadline` passes or the Deploy is
/// cancelled. False then, or when the client can't dial again; the refusal when the
/// entry refuses the dial.
async fn redialed<Redial>(
    redial: &mut impl FnMut() -> Redial,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> Result<bool, RpcError>
where
    Redial: Future<Output = Option<Result<(), RedialFailure>>>,
{
    loop {
        let answer = tokio::select! {
            () = cancellation.cancelled() => return Ok(false),
            answer = tokio::time::timeout_at(deadline, redial()) => answer,
        };
        match answer {
            Ok(Some(Ok(()))) => return Ok(true),
            Ok(Some(Err(RedialFailure::Refused(error)))) => return Err(error),
            Ok(Some(Err(RedialFailure::Failed))) => {}
            Ok(None) | Err(_) => return Ok(false),
        }
        tokio::select! {
            () = cancellation.cancelled() => return Ok(false),
            () = tokio::time::sleep_until(deadline) => return Ok(false),
            () = tokio::time::sleep(RESTART_POLL) => {}
        }
    }
}
