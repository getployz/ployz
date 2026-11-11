//! The one way the Client sends a unary RPC.
//!
//! A call site says what may be sent again ([`Replay`]) through the constructor it picks:
//! [`Call::read`], [`Call::keyed`] or [`Call::once`]. Reads and keyed mutations go out again
//! after a failure a resend can fix; a call made once never does. The engine never mints a
//! key: a keyed call is sent again only because its request already carries the key the
//! Server dedupes it by.

use std::time::Duration;

use ployz_core::{MachineTarget, OpaquePayload, Retry, Rpc, RpcResponse};
use tokio::time::Instant;

use tonic::{codec::ProstCodec, codegen::http::uri::PathAndQuery};

use super::{Client, status_error};
use crate::{
    connect::{
        ConnectError, TransportError, UNARY_RETRY_DELAYS, keep_lost_call, keep_target_reply,
        target_request,
    },
    setup_retry::{self, Expected},
};

/// What a call site lets the engine send again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Replay {
    /// A read: nothing changes, so every resend is safe.
    Read,
    /// A mutation whose request carries the key the Server dedupes it by.
    Keyed,
    /// A mutation no key protects: a lost reply is reported, never repeated.
    Once,
}

/// Where a call waits for a Machine that is restarting, in the Machine's own words.
#[derive(Clone, Copy, Debug)]
pub struct Wait<'a> {
    /// How long the call keeps trying.
    pub(crate) wait: Duration,
    /// The Server to name in the connectivity notice, when the call knows it.
    pub(crate) server: Option<&'a str>,
    /// An outage the caller expects, announced in place of the connectivity notice.
    pub(crate) expected: Option<Expected>,
}

impl Wait<'_> {
    /// Keep trying for `wait`, with the connectivity notice.
    #[must_use]
    pub const fn of(wait: Duration) -> Self {
        Self {
            wait,
            server: None,
            expected: None,
        }
    }

    /// [`Self::of`] the time setup gives a Machine to come back.
    #[must_use]
    pub const fn standard() -> Self {
        Self::of(setup_retry::WAIT)
    }
}

/// When a single attempt gives up, and the error it gives up with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Deadline {
    /// The target Machine did not answer within the time.
    Target(Duration),
    /// A readiness check's read did not answer within its five seconds.
    SetupRead,
    /// A live Machine's reply to a mutation did not come within the time. The error does not
    /// claim the target stayed silent.
    MutationReply(Duration),
}

impl Deadline {
    const SETUP_READ: Duration = Duration::from_secs(5);
}

/// The constructors of a call: [`Call::read`], [`Call::keyed`] and [`Call::once`].
#[derive(Clone, Copy, Debug)]
pub struct Call;

impl Call {
    /// A read, sent again after a failure a resend can fix.
    #[must_use]
    pub const fn read() -> Repeat<'static> {
        Repeat::new(Replay::Read)
    }

    /// A mutation whose request carries its key, sent again after a failure a resend can fix.
    #[must_use]
    pub const fn keyed() -> Repeat<'static> {
        Repeat::new(Replay::Keyed)
    }

    /// A mutation no key protects, sent once.
    #[must_use]
    pub const fn once() -> Once<'static> {
        Once::new()
    }
}

/// How one call is sent: what it may repeat, where it goes and how long it keeps trying.
/// Each kind of call is its own type, so a call made once cannot be given the pace of a
/// retrying one, and a caller that never waits for a Machine to restart never carries the
/// wait loop.
pub trait Pacing: sealed::Sealed + Sized {
    /// What this call lets the engine send again.
    fn replay(&self) -> Replay;

    #[doc(hidden)]
    fn send(
        self,
        client: &mut Client,
        path: &'static str,
        payload: OpaquePayload,
    ) -> impl Future<Output = Result<RpcResponse, ConnectError>>;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Repeat<'_> {}
    impl Sealed for super::Budgeted<'_> {}
    impl Sealed for super::Waiting<'_> {}
    impl Sealed for super::Once<'_> {}
}

/// A call that goes out again: [`Call::read`] or [`Call::keyed`]. Sent as it is, it takes
/// the entry's default retries; a budget or a wait changes how long it keeps trying.
#[derive(Clone, Copy, Debug)]
pub struct Repeat<'a> {
    replay: Replay,
    target: Option<&'a MachineTarget>,
}

impl Repeat<'static> {
    const fn new(replay: Replay) -> Self {
        Self {
            replay,
            target: None,
        }
    }
}

impl<'a> Repeat<'a> {
    /// Send to `target`, through the entry.
    #[must_use]
    pub const fn to<'b>(self, target: &'b MachineTarget) -> Repeat<'b> {
        self.toward(Some(target))
    }

    /// [`Self::to`], when the call may be for the entry itself.
    #[must_use]
    pub const fn toward<'b>(self, target: Option<&'b MachineTarget>) -> Repeat<'b> {
        Repeat {
            replay: self.replay,
            target,
        }
    }

    /// Stop trying after one budget across every attempt, so a vanished Machine costs it once.
    #[must_use]
    pub const fn budget(self, budget: Duration) -> Budgeted<'a> {
        Budgeted {
            replay: self.replay,
            target: self.target,
            budget,
        }
    }

    /// Keep trying, a second apart, while the Machine restarts or the Cluster forms.
    #[must_use]
    pub const fn waiting(self, wait: Wait<'a>) -> Waiting<'a> {
        Waiting {
            replay: self.replay,
            target: self.target,
            wait,
        }
    }
}

impl Pacing for Repeat<'_> {
    fn replay(&self) -> Replay {
        self.replay
    }

    async fn send(
        self,
        client: &mut Client,
        path: &'static str,
        payload: OpaquePayload,
    ) -> Result<RpcResponse, ConnectError> {
        client
            .repeat(path, payload, self.target, Limit::Unbounded)
            .await
    }
}

/// A [`Repeat`] with one budget across every attempt: [`Repeat::budget`].
#[derive(Clone, Copy, Debug)]
pub struct Budgeted<'a> {
    replay: Replay,
    target: Option<&'a MachineTarget>,
    budget: Duration,
}

impl Pacing for Budgeted<'_> {
    fn replay(&self) -> Replay {
        self.replay
    }

    async fn send(
        self,
        client: &mut Client,
        path: &'static str,
        payload: OpaquePayload,
    ) -> Result<RpcResponse, ConnectError> {
        let limit = Limit::of(Some(Deadline::Target(self.budget)));
        client.repeat(path, payload, self.target, limit).await
    }
}

/// A [`Repeat`] that waits for a restarting Machine: [`Repeat::waiting`].
#[derive(Clone, Copy, Debug)]
pub struct Waiting<'a> {
    replay: Replay,
    target: Option<&'a MachineTarget>,
    wait: Wait<'a>,
}

impl Pacing for Waiting<'_> {
    fn replay(&self) -> Replay {
        self.replay
    }

    async fn send(
        self,
        client: &mut Client,
        path: &'static str,
        payload: OpaquePayload,
    ) -> Result<RpcResponse, ConnectError> {
        client.wait_for(path, payload, self.target, self.wait).await
    }
}

/// A call made once: [`Call::once`].
#[derive(Clone, Copy, Debug)]
pub struct Once<'a> {
    target: Option<&'a MachineTarget>,
    deadline: Option<Deadline>,
}

impl<'a> Once<'a> {
    const fn new() -> Once<'static> {
        Once {
            target: None,
            deadline: None,
        }
    }

    /// Send to `target`, through the entry.
    #[must_use]
    pub const fn to<'b>(self, target: &'b MachineTarget) -> Once<'b> {
        self.toward(Some(target))
    }

    /// [`Self::to`], when the call may be for the entry itself.
    #[must_use]
    pub const fn toward<'b>(self, target: Option<&'b MachineTarget>) -> Once<'b> {
        Once {
            target,
            deadline: self.deadline,
        }
    }

    /// Give up when `deadline` passes.
    #[must_use]
    pub const fn within(self, deadline: Deadline) -> Self {
        Self {
            target: self.target,
            deadline: Some(deadline),
        }
    }
}

impl Pacing for Once<'_> {
    fn replay(&self) -> Replay {
        Replay::Once
    }

    async fn send(
        self,
        client: &mut Client,
        path: &'static str,
        payload: OpaquePayload,
    ) -> Result<RpcResponse, ConnectError> {
        client
            .attempt_once(path, payload, self.target, Limit::of(self.deadline))
            .await
    }
}

impl Client {
    /// Issue one unary RPC, as `call` says. The response type is derived from the RPC, so a
    /// request cannot be paired with the wrong response.
    ///
    /// A call made once sends once. A read or keyed mutation redials the same connection and
    /// sends again, up to three more times: transient transport drops (`Attempt`, tonic
    /// `Unavailable` and `DeadlineExceeded`) retry, and so does a failure the entry's router
    /// reported, on the channel it came back on. Domain `Remote` errors and other gRPC status
    /// codes are not retried.
    ///
    /// # Errors
    ///
    /// Returns an encoding, transport, or remote RPC error when the call does not succeed
    /// within what `call` allows.
    pub async fn call<T: Rpc>(
        &mut self,
        request: T::Request,
        call: impl Pacing,
    ) -> Result<T::Response, ConnectError> {
        let payload = T::into_request(request).encode()?;
        call.send(self, T::PATH, payload)
            .await?
            .decode::<T>()
            .map_err(ConnectError::Codec)
    }

    /// A call made once, on a client shared by reference, as a Deploy's is. A client that is
    /// shared is never redialed between attempts, so only a call made once can go through it.
    ///
    /// # Errors
    ///
    /// As [`Self::call`].
    pub async fn call_shared<T: Rpc>(
        &self,
        request: T::Request,
        once: Once<'_>,
    ) -> Result<T::Response, ConnectError> {
        let payload = T::into_request(request).encode()?;
        self.attempt_once(T::PATH, payload, once.target, Limit::of(once.deadline))
            .await?
            .decode::<T>()
            .map_err(ConnectError::Codec)
    }

    /// One unary RPC from an already-shaped request body, sent once. The path is the one the
    /// body's Op names.
    pub(crate) async fn call_raw(
        &self,
        request: &ployz_core::RpcRequest,
        path: &'static str,
        once: Once<'_>,
    ) -> Result<ployz_core::RpcResponse, ConnectError> {
        let payload = request.encode().map_err(ConnectError::Codec)?;
        let send = async {
            let mut grpc = self.grpc().await?;
            Ok(grpc
                .unary(
                    target_request(payload, once.target),
                    PathAndQuery::from_static(path),
                    ProstCodec::<OpaquePayload, OpaquePayload>::default(),
                )
                .await
                .map_err(|status| status_error(status, path, once.target))?
                .into_inner()
                .decode_response()?)
        };
        Limit::of(once.deadline).apply(send).await
    }

    /// Send, and after a failure a resend can fix, send again at [`UNARY_RETRY_DELAYS`]. One
    /// budget spans every attempt when there is one: fast failures retry inside it, and a
    /// vanished Machine costs the budget once instead of once per attempt.
    async fn repeat(
        &mut self,
        path: &'static str,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
        limit: Limit,
    ) -> Result<RpcResponse, ConnectError> {
        let mut delays = UNARY_RETRY_DELAYS.iter().copied();
        let mut redial = false;
        // A reply on an earlier attempt means the target was not silent throughout.
        let mut reply: Option<ConnectError> = None;
        // An earlier attempt that went out and lost its answer may have been done.
        let mut lost: Option<ConnectError> = None;
        loop {
            let error = match self
                .attempt(path, payload.clone(), target, redial, limit)
                .await
            {
                Ok(response) => return Ok(response),
                Err(error) => error,
            };
            // The entry's router made this failure, so the channel to the entry works.
            let entry_answered = error.routed_by_entry();
            let error = keep_target_reply(&mut reply, error);
            if error.retry() != Retry::Now {
                return Err(keep_lost_call(lost, error));
            }
            let Some(delay) = delays.next() else {
                return Err(keep_lost_call(lost, error));
            };
            if let Limit::Until(deadline, _) = limit
                && Instant::now() + delay >= deadline
            {
                return Err(keep_lost_call(lost, error));
            }
            tokio::time::sleep(delay).await;
            redial = !entry_answered;
            if error.target_replied() {
                // The target answered after the loss: that answer is the news now.
                lost = None;
                reply = Some(error);
            } else if error.unreplied() {
                lost = Some(error);
            }
        }
    }

    /// One attempt, after the redial it asks for. Only a deadline spent waiting on a confirmed
    /// entry is evidence the target is silent; one spent reconnecting to the entry says nothing
    /// about it. An entry that just reported the target silent on this channel counts as
    /// confirmed, as one a redial checked does.
    async fn attempt(
        &mut self,
        path: &'static str,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
        redial: bool,
        limit: Limit,
    ) -> Result<RpcResponse, ConnectError> {
        if redial {
            match limit {
                Limit::Unbounded => self.redial().await?,
                Limit::Until(deadline, _) => {
                    match tokio::time::timeout_at(deadline, self.redial()).await {
                        Ok(confirmed) => confirmed?,
                        Err(_) => {
                            return Err(tonic::Status::deadline_exceeded(
                                "entry Machine reconnect timed out",
                            )
                            .into());
                        }
                    }
                }
            }
        }
        self.attempt_once(path, payload, target, limit).await
    }

    async fn attempt_once(
        &self,
        path: &'static str,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
        limit: Limit,
    ) -> Result<RpcResponse, ConnectError> {
        limit.apply(self.send_once(path, payload, target)).await
    }

    /// Send again a second apart until the [`Wait`] runs out. An attempt that outlives five
    /// seconds fails as a setup read does, redial included.
    async fn wait_for(
        &mut self,
        path: &'static str,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
        wait: Wait<'_>,
    ) -> Result<RpcResponse, ConnectError> {
        let progress = match (wait.server, target) {
            (Some(server), _) => format!("Could not reach Server {server}"),
            (None, Some(target)) => format!(
                "Could not reach Server {} via {}",
                target.as_str().escape_debug(),
                self.connection()
            ),
            (None, None) => format!("Could not reach {}", self.connection()),
        };
        let mut redial = false;
        setup_retry::run_expecting(
            self,
            &crate::ui::WaitNotices,
            &progress,
            wait.expected,
            wait.wait,
            ConnectError::is_setup_retryable,
            async |client| {
                let reconnect = redial;
                redial = true;
                tokio::time::timeout(
                    Deadline::SETUP_READ,
                    client.attempt(path, payload.clone(), target, reconnect, Limit::Unbounded),
                )
                .await
                .unwrap_or_else(|_| Err(Words::SetupRead.expired()))
            },
        )
        .await
        .map_err(|error| match error {
            setup_retry::Error::Permanent(error) => error,
            exhausted => ConnectError::Exhausted(Box::new(exhausted)),
        })
    }
}

/// What a deadline on the target's answer reports, here and in [`Deadline::Target`].
fn target_silent() -> ConnectError {
    ConnectError::Rpc(TransportError::budget_expired(
        "target Machine RPC timed out",
    ))
}

/// How long and in what words a call's attempts run out.
#[derive(Clone, Copy)]
enum Limit {
    /// No deadline: an attempt runs until the transport ends it.
    Unbounded,
    /// One instant across every attempt, redials included.
    Until(Instant, Words),
}

/// What an attempt that runs past its deadline says.
#[derive(Clone, Copy)]
enum Words {
    Target,
    SetupRead,
    MutationReply,
}

impl Limit {
    fn of(deadline: Option<Deadline>) -> Self {
        let Some(deadline) = deadline else {
            return Self::Unbounded;
        };
        let (after, words) = match deadline {
            Deadline::Target(after) => (after, Words::Target),
            Deadline::SetupRead => (Deadline::SETUP_READ, Words::SetupRead),
            Deadline::MutationReply(after) => (after, Words::MutationReply),
        };
        Self::Until(Instant::now() + after, words)
    }

    async fn apply<V>(
        self,
        attempt: impl Future<Output = Result<V, ConnectError>>,
    ) -> Result<V, ConnectError> {
        match self {
            Self::Unbounded => attempt.await,
            Self::Until(deadline, words) => tokio::time::timeout_at(deadline, attempt)
                .await
                .unwrap_or_else(|_| Err(words.expired())),
        }
    }
}

impl Words {
    fn expired(self) -> ConnectError {
        match self {
            Self::Target => target_silent(),
            Self::SetupRead => {
                tonic::Status::deadline_exceeded("Machine setup read timed out").into()
            }
            Self::MutationReply => {
                tonic::Status::deadline_exceeded("Machine setup mutation reply timed out").into()
            }
        }
    }
}

/// A setup call's error as the wait loop reported it, for a caller that tells a wait that
/// ran out from an error no retry fixes.
pub(crate) fn setup_outcome(error: ConnectError) -> setup_retry::Error<ConnectError> {
    let ConnectError::Exhausted(exhausted) = error else {
        return setup_retry::Error::Permanent(error);
    };
    match exhausted.downcast::<setup_retry::Error<ConnectError>>() {
        Ok(exhausted) => *exhausted,
        Err(other) => setup_retry::Error::Permanent(ConnectError::Exhausted(other)),
    }
}
