//! Ask each of several Machines to set one thing, a few at a time, and keep every
//! Machine's answer in the order the Machines came.

use std::future::Future;

use futures_util::{StreamExt, stream};
use ployz_core::RpcError;
use tokio_util::sync::CancellationToken;

/// What one Machine's daemon answered, or why it wasn't asked to the end.
#[derive(Debug)]
pub(crate) enum Answer<T> {
    Set(Box<T>),
    Refused(RpcError),
    Cancelled,
}

/// Ask each of `machines`, `at_once` at a time. `refuse` names a Machine that can't be
/// asked, which answers [`Answer::Refused`] without a call, even once `cancel` has fired.
/// An answer still pending when `cancel` fires is [`Answer::Cancelled`]. The answers come
/// back in `machines`' order.
///
/// Each future owns its Machine, since one borrowing from the slice leaves the caller's
/// future Send for one lifetime only, and a spawned Deploy runner needs all.
pub(crate) async fn set_each<M, T, Fut>(
    machines: &[M],
    at_once: usize,
    cancel: &CancellationToken,
    refuse: impl Fn(&M) -> Option<RpcError>,
    ask: impl Fn(M) -> Fut,
) -> Vec<Answer<T>>
where
    M: Clone,
    Fut: Future<Output = Result<T, RpcError>>,
{
    let (refuse, ask) = (&refuse, &ask);
    stream::iter(machines.to_vec())
        .map(|machine| async move {
            if let Some(refusal) = refuse(&machine) {
                return Answer::Refused(refusal);
            }
            tokio::select! {
                answer = ask(machine) => match answer {
                    Ok(set) => Answer::Set(Box::new(set)),
                    Err(error) => Answer::Refused(error),
                },
                () = cancel.cancelled() => Answer::Cancelled,
            }
        })
        .buffered(at_once)
        .collect()
        .await
}

#[cfg(test)]
#[path = "set_each_tests.rs"]
mod tests;
