//! The Container steps this Machine owns: Freeze's docker stop and the docker start of a
//! Thaw or a handed Start. Each runs in a task keyed by its Volume and lease record, so a
//! caller that gives up drops nothing. A replay with the same record waits on the task, and
//! Docker runs once per record.

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{
    Cycle, FenceDecision, Lease, LeaseRecord, Pos, RpcError, Switch, SwitchError, SwitchReply,
};
use tokio::sync::watch;

use super::{
    VolumeError, VolumeStorage,
    lease::{fence, internal, refusal},
    storage::HeldMutation,
};

type Answer = Result<SwitchReply, RpcError>;

/// How long a caller waits for a step's answer before hearing that the step still works.
/// It bounds only the answer; the step runs until Docker returns.
const ANSWER_WITHIN: Duration = Duration::from_secs(3);

#[derive(Clone, Default)]
pub(super) struct ContainerSteps(Arc<Mutex<HashMap<String, Step>>>);

/// One Volume's step: the task working on it now, and what Docker answered last.
#[derive(Default)]
struct Step {
    running: Option<Running>,
    docker: Option<Ran>,
}

struct Running {
    at: LeaseRecord,
    answer: watch::Receiver<Option<Answer>>,
}

impl Running {
    fn working(&self) -> bool {
        self.answer.borrow().is_none() && self.answer.has_changed().is_ok()
    }
}

struct Ran {
    at: (Lease, Pos),
    outcome: Result<String, String>,
}

impl ContainerSteps {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Step>> {
        self.0.lock().expect("container steps are never poisoned")
    }
}

fn busy(name: &str, working: &str) -> RpcError {
    SwitchError::Busy.rpc_error(format!("Volume {name} is still {working} its Container"))
}

/// Refuses `switch` while a step of `name` runs: a step it fences out is refused as the
/// fence decides, and any other waits for Docker.
fn while_running(running: &Running, name: &str, switch: &Switch, working: &str) -> RpcError {
    let now = chrono::Utc::now().timestamp();
    let decision = fence(Some(running.at), switch, now);
    refusal(decision, switch, Some(running.at), now).unwrap_or_else(|| busy(name, working))
}

impl VolumeStorage {
    /// Runs `step` in a task this Machine owns, or attaches to the running one when `switch`
    /// replays its record. Answers within [`ANSWER_WITHIN`], with Busy while Docker works.
    pub(super) async fn container_step(
        &self,
        name: &str,
        switch: &Switch,
        working: &'static str,
        step: impl Future<Output = Answer> + Send + 'static,
    ) -> Answer {
        let mut answer = {
            let mut steps = self.steps.lock();
            let entry = steps.entry(name.to_owned()).or_default();
            match entry.running.as_ref().filter(|running| running.working()) {
                Some(running) => {
                    if fence(Some(running.at), switch, chrono::Utc::now().timestamp())
                        != FenceDecision::Replay
                    {
                        return Err(while_running(running, name, switch, working));
                    }
                    running.answer.clone()
                }
                None => {
                    let (sender, answer) = watch::channel(None);
                    entry.running = Some(Running {
                        at: LeaseRecord {
                            lease: switch.lease,
                            pos: switch.pos,
                            cycle: Cycle::Open,
                        },
                        answer: answer.clone(),
                    });
                    tokio::spawn(async move {
                        sender.send_replace(Some(step.await));
                    });
                    answer
                }
            }
        };
        match tokio::time::timeout(ANSWER_WITHIN, answer.wait_for(Option::is_some)).await {
            Ok(Ok(answer)) => answer.clone().expect("the step's answer was waited for"),
            Ok(Err(_)) => Err(internal(
                format!("the Container step of Volume {name} ended without an answer").into(),
            )),
            Err(_) => Err(busy(name, working)),
        }
    }

    /// Refuses a verb that would race the running step of `name`.
    pub(super) fn refuse_while_running(
        &self,
        name: &str,
        switch: &Switch,
        working: &str,
    ) -> Result<(), RpcError> {
        match self
            .steps
            .lock()
            .get(name)
            .and_then(|step| step.running.as_ref())
            .filter(|running| running.working())
        {
            Some(running) => Err(while_running(running, name, switch, working)),
            None => Ok(()),
        }
    }

    /// Runs Docker for the step `at` unless it already ran for that record, and then answers
    /// what Docker answered that time.
    pub(super) async fn docker_once(
        &self,
        held: &HeldMutation,
        name: &str,
        at: LeaseRecord,
        arguments: &[&str],
    ) -> super::Result<String> {
        let at = (at.lease, at.pos);
        let ran = self
            .steps
            .lock()
            .get(name)
            .and_then(|step| step.docker.as_ref())
            .filter(|ran| ran.at == at)
            .map(|ran| ran.outcome.clone());
        let outcome = match ran {
            Some(outcome) => outcome,
            None => {
                let outcome = self
                    .docker(held, arguments)
                    .await
                    .map_err(|error| error.to_string());
                self.steps.lock().entry(name.to_owned()).or_default().docker = Some(Ran {
                    at,
                    outcome: outcome.clone(),
                });
                outcome
            }
        };
        outcome.map_err(VolumeError::from)
    }
}
