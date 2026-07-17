use std::future::Future;
use std::pin::Pin;

use futures_util::{Stream, StreamExt};

pub(super) enum MachineCallOrLog<R, T> {
    Call(R),
    Log(Option<T>),
    LogsClosed,
}

pub(super) async fn next_machine_call_or_log<F, S>(
    mut call: Pin<&mut F>,
    logs: &mut S,
    logs_open: bool,
) -> MachineCallOrLog<F::Output, S::Item>
where
    F: Future + ?Sized,
    S: Stream + Unpin,
{
    tokio::select! {
        result = &mut call => MachineCallOrLog::Call(result),
        message = logs.next(), if logs_open => MachineCallOrLog::Log(message),
        else => MachineCallOrLog::LogsClosed,
    }
}

pub(super) enum LogBeforeDeadline<T> {
    Deadline,
    Log(Option<T>),
    LogsClosed,
}

pub(super) async fn next_log_before_deadline<S>(
    mut deadline: Pin<&mut tokio::time::Sleep>,
    logs: &mut S,
    logs_open: bool,
) -> LogBeforeDeadline<S::Item>
where
    S: Stream + Unpin,
{
    tokio::select! {
        biased;
        () = &mut deadline => LogBeforeDeadline::Deadline,
        message = logs.next(), if logs_open => LogBeforeDeadline::Log(message),
        else => LogBeforeDeadline::LogsClosed,
    }
}
