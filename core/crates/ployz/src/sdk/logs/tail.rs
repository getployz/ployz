//! The last lines of a failed Container, read from its Machine's Log Store for a deploy's failed row.
use ployz_core::{ContainerId, LastRun, LogHistoryRequest, MachineId, MachineTarget, RpcError};
use ployz_store::tools::{LineCap, LogExcerpt, LogText, SealedValues};

use super::stream::{Exit, TAIL_READ, frames, newest_lines};
use crate::connect::Client;

/// The lines read from the Log Store: more than are kept, so a secret spanning lines is
/// whole when it is redacted, before the cut.
const TAIL_FETCH: u16 = 200;
/// The lines a failed row keeps.
const TAIL_KEEP: u16 = 10;

/// A failed Container's raw last lines, oldest first, and its newest stored exit.
pub(crate) struct Tail {
    frames: Vec<String>,
    exit: Option<Exit>,
}

impl Tail {
    pub(crate) fn exit_code(&self) -> Option<i64> {
        self.exit.and_then(|exit| exit.code)
    }

    /// The tail scrubbed of `sealed`, and the run its newest exit ended: that code and
    /// the last line that run printed, not one the run before or after it printed.
    pub(crate) fn scrubbed(self, sealed: &SealedValues) -> (LogText, LastRun) {
        let total = self.frames.len();
        let (from, to) = self.exit.map_or((0, total), |exit| {
            (
                exit.run_from.map_or(0, |after| total.saturating_sub(after)),
                total.saturating_sub(exit.lines_after),
            )
        });
        let run = self
            .frames
            .get(from..to)
            .unwrap_or_default()
            .iter()
            .cloned();
        let last_line = match tail_excerpt(run, sealed) {
            LogText::Shown(excerpt) => excerpt.lines().last().cloned(),
            LogText::Withheld { .. } => None,
        };
        let run = LastRun {
            exit_code: self.exit_code(),
            last_line,
        };
        (tail_excerpt(self.frames, sealed), run)
    }
}

/// The raw last lines of failed Container `container` on `machine`, for
/// [`Tail::scrubbed`] once what to scrub them of is known. None when unread in time.
pub(crate) async fn tail_frames(
    client: &Client,
    machine: MachineId,
    container: ContainerId,
) -> Option<Tail> {
    let target = MachineTarget::from(&machine);
    let read = async {
        let selector = LogHistoryRequest {
            container_id: Some(container),
            ..LogHistoryRequest::default()
        };
        let (newest, exit) = newest_lines(client, &target, selector, TAIL_FETCH).await?;
        Ok::<_, RpcError>(Tail {
            frames: frames(newest.into_iter().map(|(_, text)| text)),
            exit,
        })
    };
    tokio::time::timeout(TAIL_READ, read).await.ok()?.ok()
}

fn tail_excerpt(frames: impl IntoIterator<Item = String>, sealed: &SealedValues) -> LogText {
    LogText::Shown(LogExcerpt::scrub(
        frames,
        sealed,
        LineCap::new(TAIL_KEEP).unwrap_or_default(),
    ))
}
