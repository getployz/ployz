//! What every act shares: resolving a Server by name, running the act, and
//! waiting on its run.

use serde_json::{Value, json};

use super::cloud::{Operation, OperationsAnswer, ServerReport, ServerState, ServersAnswer};
use super::machine::{Cx, Stop};
use super::refusal::{Call, Refusal};
use super::step::{
    Action, Ask, CloudQuery, DashboardPage, Fix, OperationsQuery, RequestKey, ServerName, Subject,
};
use crate::error::did_you_mean;
use ployz_core::RpcErrorCode;

/// The ask for every Server.
pub(crate) const fn servers() -> Ask {
    Ask::Cloud(CloudQuery::Servers { only: None })
}

/// `server_show` for one Server, or every one.
pub(crate) fn server_show(server: Option<&ServerName>) -> Call {
    let input = match server {
        Some(server) => json!({ "server": server.as_str() }),
        None => json!({}),
    };
    Call::new("server_show", input)
}

/// The one Server `name` names. With `answering`, a Server that did not answer
/// is refused before anything is asked of it.
pub(crate) fn resolve<'a>(
    answer: &'a ServersAnswer,
    name: &ServerName,
    field: &str,
    answering: bool,
) -> Result<&'a ServerReport, Stop> {
    let names = || answer.servers.iter().map(|server| server.name.as_str());
    let mut matches = answer.servers.iter().filter(|server| server.name == *name);
    let Some(found) = matches.next() else {
        return Err(Refusal::not_found("No server by that name")
            .with_did_you_mean(did_you_mean(name.as_str(), names()))
            .with_valid(names())
            .with_next(server_show(None))
            .into());
    };
    if matches.next().is_some() {
        return Err(Refusal::new(
            RpcErrorCode::Ambiguous,
            "More than one server has that name; rename one in the dashboard",
        )
        .with_valid(names())
        .with_next(server_show(None))
        .with_fix(&Fix::OpenDashboard {
            page: DashboardPage::Servers,
        })
        .into());
    }
    let silent = answer.seen.unanswered.contains(name) || found.state == ServerState::Offline;
    if answering && silent {
        let mut refusal = Refusal::new(
            RpcErrorCode::Unavailable,
            "That server did not answer, so nothing was asked of it",
        );
        refusal.field = Some(field.to_owned());
        return Err(refusal
            .with_next(server_show(Some(name)))
            .with_fix(&Fix::ConnectServers)
            .into());
    }
    Ok(found)
}

/// One act and where its result says it ran.
pub(crate) struct Run {
    pub(crate) at: String,
    pub(crate) action: Action,
    pub(crate) subject: Subject,
    /// The read that shows the subject now.
    pub(crate) read: Call,
}

/// Run the call's act, then wait on its run within `wait_seconds`.
pub(crate) fn operate(
    cx: &mut Cx<'_>,
    run: Run,
    key: Option<RequestKey>,
    wait_seconds: u32,
) -> Result<Value, Stop> {
    let mut operation = cx.act(run.action, key, run.read.clone())?;
    let mut attempt = 0;
    while !operation.state.ended() {
        let ask = Ask::Cloud(CloudQuery::Operations(OperationsQuery {
            subject: run.subject.clone(),
            attempt: Some(operation.attempt),
            limit: 1,
        }));
        let Some(answers) = cx.poll(wait_seconds, attempt, vec![ask])? else {
            break;
        };
        let page = cx.cloud::<OperationsAnswer>(answers.into_iter().next())?;
        if let Some(latest) = page.operations.into_iter().next() {
            operation = latest;
        }
        attempt += 1;
    }
    Ok(json!({
        "at": run.at,
        "run": operation.run,
        "state": operation.state,
        "reason": operation.reason,
        "servers": operation.servers,
        "kept": operation.kept,
        "unanswered": operation.unanswered,
        "next": run.read,
    }))
}

/// An operation as a read lists it: what was asked and how it ended.
pub(crate) fn latest(operation: &Operation) -> Value {
    json!({
        "run": operation.run,
        "state": operation.state,
        "requested_at": operation.requested_at,
        "ended_at": operation.ended_at,
        "reason": operation.reason,
    })
}

/// The latest operation on `subject`.
pub(crate) fn operations(subject: Subject, limit: u8) -> Ask {
    Ask::Cloud(CloudQuery::Operations(OperationsQuery {
        subject,
        attempt: None,
        limit,
    }))
}
