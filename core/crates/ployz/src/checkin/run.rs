//! The loop a waiting host runs until Cloud enrolls, rejects or forgets it.

use chrono::Utc;

use super::paths::Paths;
use super::plan::{Next, REFUSED_ONCE, pass_from};
use super::probe::Probes;
use super::token_file::{self, Held, replaced_token};
use super::{CheckinClient, CheckinKey, Host, StopReason, client, enroll, unit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckinEnd {
    Stopped(StopReason),
    /// The token file changed as the loop stopped. The loop stays enabled to restart with it.
    TokenChanged,
    Enrolled,
}

pub(crate) async fn run<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
) -> CheckinEnd {
    let mut said = Said::default();
    let mut probes = Probes::load(paths.probe(), host.binder());
    loop {
        match pass_from(host, client, key, paths, Utc::now(), false, &mut probes).await {
            Next::Wait { after } => {
                said.fresh(None);
                tokio::time::sleep(after).await;
            }
            // Cloud holds a full token back pass after pass, so the hold is logged once.
            Next::Held { after, why, .. } => {
                let line = format!("{why} Trying again in {}s.", after.as_secs());
                if let Some(line) = said.fresh(Some(line)) {
                    crate::ui::note(line);
                }
                tokio::time::sleep(after).await;
            }
            Next::Retry { after, why } => {
                said.fresh(None);
                crate::ui::note(format!("{why} Trying again in {}s.", after.as_secs()));
                tokio::time::sleep(after).await;
            }
            Next::RefusedOnce { after } => {
                said.fresh(None);
                crate::ui::note(format!(
                    "{REFUSED_ONCE}. Trying again in {}s.",
                    after.as_secs()
                ));
                tokio::time::sleep(after).await;
            }
            Next::Stop(reason, message) => {
                // A token answer is about the token that was sent, not one written since. A
                // rejection is about the host, whatever token it holds.
                if reason != StopReason::Rejected
                    && let Some(token) = replaced_token(client, &paths.token_file).await
                {
                    crate::ui::note(format!(
                        "{}. Checking in with the new Fleet Token.",
                        reason.title()
                    ));
                    client.use_token(token);
                    continue;
                }
                crate::ui::note(format!("{}. {message}", reason.title()));
                return stop(host, client, paths, reason).await;
            }
            Next::ClockOff(why) | Next::NotCloud(why) => {
                said.fresh(None);
                crate::ui::note(format!(
                    "{why}. Trying again in {}s.",
                    client::BACKOFF.as_secs()
                ));
                tokio::time::sleep(client::BACKOFF).await;
            }
            Next::Enrolled => return enroll::finish(host, paths).await,
            Next::Enrolling => unreachable!("only an install pass leaves enrolling to the loop"),
        }
    }
}

/// The last line the loop logged, so a line repeated pass after pass is logged once.
#[derive(Default)]
pub(super) struct Said(Option<String>);

impl Said {
    /// The line, unless the loop just logged it. None says nothing, so any next line is fresh.
    pub(super) fn fresh(&mut self, line: Option<String>) -> Option<String> {
        if line == self.0 {
            return None;
        }
        self.0.clone_from(&line);
        line
    }
}

/// Removes the token file only while it still holds the token Cloud answered, so a token written
/// since survives, and the loop stays enabled to restart with it. A rejection is about the host,
/// so it ends the loop whatever the file holds.
pub(crate) async fn stop<H: Host>(
    host: &H,
    client: &CheckinClient,
    paths: &Paths,
    reason: StopReason,
) -> CheckinEnd {
    let held = token_file::retire(client, &paths.token_file);
    if held == Held::Replaced && reason != StopReason::Rejected {
        crate::ui::note(format!(
            "{} changed. The check-in loop restarts with it.",
            paths.token_file.display()
        ));
        return CheckinEnd::TokenChanged;
    }
    if let Err(why) = host.disable_loop().await {
        crate::ui::note(format!("Could not disable {}: {why}", unit::CHECKIN));
    }
    CheckinEnd::Stopped(reason)
}
