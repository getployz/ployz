//! The last lines of a failed Container, read from Docker for a deploy's failed row.
use ployz_agent_tools::{LineCap, LogExcerpt, LogText, SealedValues};
use ployz_core::{ContainerId, MachineId};

use super::stream::{TAIL_READ, docker_lines};
use crate::connect::Client;

/// The lines read from Docker: more than are kept, so a secret spanning lines is whole
/// when it is redacted, before the cut.
const TAIL_FETCH: i32 = 200;
/// The lines a failed row keeps.
const TAIL_KEEP: u16 = 10;

/// A failed Container's raw last lines, oldest first.
pub(crate) struct Tail {
    frames: Vec<String>,
}

impl Tail {
    pub(crate) fn scrubbed(self, sealed: &SealedValues) -> (LogText, Option<String>) {
        let excerpt = LogExcerpt::scrub(
            self.frames,
            sealed,
            LineCap::new(TAIL_KEEP).unwrap_or_default(),
        );
        let last_line = excerpt.lines().last().cloned();
        (LogText::Shown(excerpt), last_line)
    }
}

/// The raw last lines of failed Container `container` on `machine`, for
/// [`Tail::scrubbed`] once what to scrub them of is known. None when unread in time.
pub(crate) async fn tail_frames(
    client: &Client,
    machine: MachineId,
    container: ContainerId,
) -> Option<Tail> {
    let read = docker_lines(client, machine, container, TAIL_FETCH, None);
    let lines = tokio::time::timeout(TAIL_READ, read).await.ok()?.ok()?;
    Some(Tail {
        frames: lines
            .into_iter()
            .map(|line| String::from_utf8_lossy(&line.text).into_owned())
            .collect(),
    })
}
