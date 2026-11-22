//! The terminal side of Global catch-up after a join: progress, warnings and the
//! follow-up failure.

use ployz_core::{Hint, Machine, QualifiedService};

use crate::connect::Client;
use crate::failure::Failure;
use crate::global_catch_up::{CatchUpError, CatchUpFact, catch_up_globals};
use crate::global_slot::ServerNames;

pub(crate) fn joined_catch_up_error(
    error: CatchUpError,
    server: &Machine,
    command: impl Fn(&[&str]) -> String,
) -> Failure {
    let mut message = if error.cause.is_interrupted() {
        "Server joined; Global catch-up was interrupted. It remains a Cluster member."
    } else {
        "Server joined, but Global catch-up is incomplete; it remains a Cluster member."
    }
    .to_owned();
    for identity in &error.unresolved {
        if *identity != QualifiedService::system_ingress() && !identity.is_private_access_router() {
            message.push_str(&format!(
                " To finish, redeploy Namespace Service `{identity}`."
            ));
        }
    }
    let mut failure = error.cause.context(message);
    for identity in error.unresolved {
        let hint = if identity == QualifiedService::system_ingress() {
            Hint::Retry(command(&[
                "server",
                "set",
                &server.id.to_string(),
                "--accepts-ingress=true",
            ]))
        } else if identity.is_private_access_router() {
            Hint::Retry(command(&["private-access", "on", "--auth-key-stdin"]))
        } else {
            Hint::Inspect(command(&[
                "logs",
                &identity.to_string(),
                "--machine",
                &server.id.to_string(),
            ]))
        };
        failure = failure.hint(hint);
    }
    failure
}

/// Follow target-only catch-up with the shared progress block. Returns the Unarrived
/// Servers it reported, whether or not catch-up then succeeded.
pub(crate) async fn follow_globals(
    client: &mut Client,
    assigned: &Machine,
    command: impl Fn(&[&str]) -> String,
) -> (Vec<serde_json::Value>, Result<(), CatchUpError>) {
    use crate::ui::progress::{Disposition, Frame, Progress, Row, Run, State, Subject, Timing};
    let signal = match crate::cancellation::interrupted() {
        Ok(signal) => signal,
        Err(error) => return (Vec::new(), Err(CatchUpError::new(error.into(), Vec::new()))),
    };
    let mut frame = Frame {
        run: Run::CatchUp(assigned.id),
        title: format!("Starting Globals on {}", assigned.name),
        rows: Vec::new(),
        notices: Vec::new(),
        held_back: ployz_core::RegionWait::default(),
    };
    let mut progress = Progress::start(frame.clone());
    let mut not_arrived = ServerNames::default();
    let result = catch_up_globals(client, assigned, &signal, |fact| {
        let (identity, state) = match fact {
            CatchUpFact::NotArrived(names) => {
                not_arrived = names;
                return;
            }
            CatchUpFact::Identified(services) => {
                frame.rows = services
                    .into_iter()
                    .map(|service| Row {
                        subject: Subject::ServiceOnServer {
                            service,
                            machine: assigned.id,
                            server: assigned.name.to_string(),
                        },
                        state: State::Pending,
                        detail: None,
                        timing: Timing::Unavailable,
                    })
                    .collect();
                progress.update(frame.clone());
                return;
            }
            CatchUpFact::Creating(service) => (
                service,
                State::Running(ployz_store::RowPhase::CreatingContainer),
            ),
            CatchUpFact::Excluded(service) => (service, State::Excluded),
            CatchUpFact::Running(service) => (service, State::ObservedRunning),
            CatchUpFact::Failed(service) => (service, State::Failed),
        };
        if let Some(row) = frame.rows.iter_mut().find(|row| {
            matches!(&row.subject, Subject::ServiceOnServer { service, .. } if service == &identity)
        }) {
            if matches!(row.timing, Timing::Unavailable) {
                row.timing = Timing::Started(std::time::SystemTime::now());
            }
            if !matches!(state, State::Running(_) | State::Pending)
                && let Timing::Started(at) = row.timing
            {
                row.timing = Timing::Finished(at.elapsed().unwrap_or_default());
            }
            row.state = state;
            row.detail = None;
        }
        progress.update(frame.clone());
    })
    .await;
    if result.is_err() {
        for row in &mut frame.rows {
            if matches!(row.state, State::Pending | State::Running(_)) {
                row.state = State::NotAttempted;
            }
        }
    }
    progress.finish(
        frame,
        if signal.is_cancelled() {
            Disposition::LocalInterrupted
        } else {
            Disposition::Settled
        },
    );
    let mut unarrived = Vec::new();
    // A Server enrolled moments ago looks the same as one that never will join,
    // so forgetting it is offered only with its condition.
    let list = command(&["server", "ls"]);
    for (id, name) in not_arrived {
        let next = command(&["server", "forget", id.as_str(), "--confirm", "dead"]);
        crate::ui::warn(format!(
            "Server {name} may still be joining, so Global catch-up did not wait on it; if `{}` \
             keeps showing it down with no version, remove it with `{}`.",
            crate::ui::pasteable(&list),
            crate::ui::pasteable(&next)
        ));
        unarrived.push(serde_json::json!({ "id": id, "name": name, "next": next }));
    }
    if !unarrived.is_empty() {
        crate::ui::hint(&Hint::Inspect(list));
    }
    (unarrived, result)
}

/// Add the Unarrived Servers catch-up skipped to a join result, when there are any.
pub(crate) fn with_unarrived(
    mut result: serde_json::Value,
    unarrived: &[serde_json::Value],
) -> serde_json::Value {
    if !unarrived.is_empty()
        && let Some(fields) = result.as_object_mut()
    {
        fields.insert("unarrived".into(), unarrived.into());
    }
    result
}
