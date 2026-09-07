//! CLI paint model for one Deploy. Human copy lives here, not on the wire types.

use std::fmt::Write as _;
use std::io::IsTerminal;

use crossterm::style::Stylize as _;
use ployz_core::{
    ContainerId, ContainerRuntimeObservation, DependencyHealthFailure, DeployOperation,
    DeployOutcome, ExecutionError, FailedOperation, HealthFailure, HealthObservation, HookFailure,
    MachineAction, MachineName, OperationPhase, OperationRow, OperationStatus,
    ReplacementCompensation, RestartAttempt, ServiceName, StopAttempt,
};

/// ANSI roles for a TTY stream. Pipes and `NO_COLOR` stay plain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Ink {
    color: bool,
}

impl Ink {
    /// Color when `stream` is a TTY and `NO_COLOR` is unset.
    #[must_use]
    pub(crate) fn detect(stream: impl IsTerminal) -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some();
        Self {
            color: stream.is_terminal() && !no_color,
        }
    }

    /// No ANSI.
    #[must_use]
    pub(crate) const fn plain() -> Self {
        Self { color: false }
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) const fn color() -> Self {
        Self { color: true }
    }

    #[must_use]
    pub(crate) fn paint(self, role: Role, text: &str) -> String {
        if !self.color {
            return text.to_owned();
        }
        match role {
            Role::Title => text.bold().to_string(),
            Role::Done => text.green().to_string(),
            Role::Run => text.cyan().to_string(),
            Role::Fail => text.red().to_string(),
            Role::Idle => text.dim().to_string(),
            Role::Neutral => text.to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Role {
    Title,
    Done,
    Run,
    Fail,
    Idle,
    Neutral,
}

#[derive(Clone, Debug)]
struct TaskView {
    subject: Subject,
    verb: Verb,
    place: Option<MachineName>,
    state: TaskState,
    service: Option<ServiceName>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Subject {
    Storage { name: String },
    Container { name: String },
    Volume { name: String },
    Dependency { name: String },
    Hook { name: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Verb {
    Prepare,
    Create,
    Replace,
    Remove,
    Stop,
    Wait,
    RunHook,
    StopHook,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TaskState {
    Pending,
    Running { pulse: Pulse },
    Done { word: DoneWord },
    Failed { cause: Cause },
    Unexecuted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pulse {
    Starting,
    WaitingHealth { elapsed_ms: u64 },
    WaitingHook { elapsed_ms: u64 },
    Removing,
    Compensating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DoneWord {
    Ready,
    Healthy,
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Cause {
    Machine {
        action: ActionWord,
        message: String,
    },
    HealthTimeout,
    HealthCancelled,
    HealthRuntime {
        summary: RuntimeSummary,
    },
    HookTimeout {
        stop_message: Option<String>,
    },
    HookCancelled {
        stop_message: Option<String>,
    },
    HookExit {
        code: i64,
        stop_message: Option<String>,
    },
    DependencyCancelled,
    DependencyEmpty {
        dependency: String,
    },
    DependencyObserve {
        dependency: String,
        message: String,
    },
    DependencyContainer {
        cause: Box<Cause>,
    },
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionWord {
    PrepareVolumes,
    Create,
    Start,
    Inspect,
    Stop,
    Remove,
    RemoveVolume,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeSummary {
    NeverStarted,
    Unhealthy,
    StillStarting,
    NoHealthcheck,
    Paused,
    Restarting,
    Exited { code: i64 },
    Removing,
    Dead,
    Unrecognized,
    ReportedHealthy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CompensationFact {
    StoppedNew,
    StopNewFailed { cause: Cause },
    RestartedOld,
    RestartOldFailed { cause: Cause },
    RestartNotAttempted,
}

/// Live progress title plus one row per operation.
#[must_use]
pub(crate) fn paint_live(
    title: &str,
    completed: u32,
    total: u32,
    rows: &[OperationRow],
    ink: &Ink,
) -> String {
    let tasks: Vec<_> = rows.iter().map(TaskView::from_row).collect();
    paint_tasks(title, completed, total, &tasks, ink)
}

/// Halt footer. Synthesizes the live list when Progress never printed.
#[must_use]
pub(crate) fn paint_closing(
    outcome: &DeployOutcome<ExecutionError>,
    rows: &[OperationRow],
    live_shown: bool,
    ink: &Ink,
) -> String {
    let DeployOutcome::Failed {
        completed,
        failed,
        unexecuted,
    } = outcome
    else {
        return String::new();
    };
    let tasks = if rows.is_empty() {
        tasks_from_failed_outcome(completed, failed, unexecuted)
    } else {
        let mut tasks: Vec<_> = rows.iter().map(TaskView::from_row).collect();
        overlay_failed(&mut tasks, rows, failed);
        tasks
    };
    let mut out = String::new();
    if !live_shown {
        let total = (completed.len() + 1 + unexecuted.len()) as u32;
        out.push_str(&paint_tasks("", completed.len() as u32, total, &tasks, ink));
    }
    let failed_row = tasks
        .iter()
        .find(|row| matches!(row.state, TaskState::Failed { .. }))
        .cloned()
        .unwrap_or_else(|| task_from_failed(failed));
    let TaskState::Failed { cause } = &failed_row.state else {
        return out;
    };
    let place = failed_row
        .place
        .as_ref()
        .map(|machine| format!(" on {machine}"))
        .unwrap_or_default();
    let prefix = ink.paint(Role::Fail, "Failed:");
    let _ = writeln!(
        out,
        "{prefix} {} {}{place}",
        failed_row.verb.word(),
        failed_row.subject.name()
    );
    let _ = writeln!(out, "  {}", ink.paint(Role::Fail, &cause.english()));
    if let FailedOperation::ReplacementHealth { compensation, .. } = failed {
        for fact in compensation_facts(compensation) {
            let _ = writeln!(out, "  {}", compensation_line(&fact));
        }
    }
    if wants_logs(cause)
        && let Some(service) = &failed_row.service
    {
        let hint = ink.paint(Role::Neutral, &format!("next: ployz logs {service}"));
        let _ = writeln!(out, "  {hint}");
    }
    out
}

fn paint_tasks(title: &str, completed: u32, total: u32, rows: &[TaskView], ink: &Ink) -> String {
    let mut out = String::new();
    if !title.is_empty() {
        let mark = ink.paint(Role::Title, "[+]");
        let _ = writeln!(out, "{mark} {title} {completed}/{total}");
    }
    for row in rows {
        out.push_str(&paint_row(row, ink));
    }
    out
}

fn tasks_from_failed_outcome(
    completed: &[DeployOperation],
    failed: &FailedOperation<ExecutionError>,
    unexecuted: &[DeployOperation],
) -> Vec<TaskView> {
    let mut rows: Vec<_> = completed
        .iter()
        .map(|operation| {
            TaskView::from_operation(
                operation,
                TaskState::Done {
                    word: done_word(operation),
                },
            )
        })
        .collect();
    rows.push(task_from_failed(failed));
    rows.extend(
        unexecuted
            .iter()
            .map(|operation| TaskView::from_operation(operation, TaskState::Unexecuted)),
    );
    rows
}

impl TaskView {
    fn from_row(row: &OperationRow) -> Self {
        let name = visible_row_name(row);
        Self {
            subject: subject_of(&row.operation, name),
            verb: verb_of(&row.operation),
            place: place_of(row),
            state: task_state(row),
            service: logs_service(row),
        }
    }

    fn from_operation(operation: &DeployOperation, state: TaskState) -> Self {
        let name = visible_name(None, operation, operation.container_id().as_ref());
        Self {
            subject: subject_of(operation, name),
            verb: verb_of(operation),
            place: None,
            state,
            service: operation.service_name().cloned(),
        }
    }
}

impl Subject {
    fn name(&self) -> &str {
        match self {
            Self::Storage { name }
            | Self::Container { name }
            | Self::Volume { name }
            | Self::Dependency { name }
            | Self::Hook { name } => name,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Storage { .. } => "Storage",
            Self::Container { .. } => "Container",
            Self::Volume { .. } => "Volume",
            Self::Dependency { .. } => "Dependency",
            Self::Hook { .. } => "Hook",
        }
    }
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Create => "create",
            Self::Replace => "replace",
            Self::Remove => "remove",
            Self::Stop => "stop",
            Self::Wait => "wait",
            Self::RunHook => "run hook",
            Self::StopHook => "stop hook",
        }
    }
}

impl ActionWord {
    fn from_machine(action: MachineAction) -> Self {
        match action {
            MachineAction::PrepareVolumes => Self::PrepareVolumes,
            MachineAction::CreateContainer => Self::Create,
            MachineAction::StartContainer => Self::Start,
            MachineAction::InspectContainer => Self::Inspect,
            MachineAction::StopContainer => Self::Stop,
            MachineAction::RemoveContainer => Self::Remove,
            MachineAction::RemoveVolume => Self::RemoveVolume,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::PrepareVolumes => "prepare storage",
            Self::Create => "create",
            Self::Start => "start",
            Self::Inspect => "inspect",
            Self::Stop => "stop",
            Self::Remove => "remove",
            Self::RemoveVolume => "remove volume",
        }
    }
}

impl Cause {
    fn english(&self) -> String {
        match self {
            Self::Machine { action, message } => format!("{} failed: {message}", action.word()),
            Self::HealthTimeout => "health check timed out".into(),
            Self::HealthCancelled => "health check cancelled".into(),
            Self::HealthRuntime { summary } => summary.english(),
            Self::HookTimeout { stop_message } => {
                hook_line("pre-deploy hook timed out", stop_message)
            }
            Self::HookCancelled { stop_message } => {
                hook_line("pre-deploy hook cancelled", stop_message)
            }
            Self::HookExit { code, stop_message } => {
                hook_line(&format!("pre-deploy hook exited {code}"), stop_message)
            }
            Self::DependencyCancelled | Self::Cancelled => "deploy cancelled".into(),
            Self::DependencyEmpty { dependency } => {
                format!("no containers were observed for {dependency}")
            }
            Self::DependencyObserve {
                dependency,
                message,
            } => format!("could not observe {dependency}: {message}"),
            Self::DependencyContainer { cause } => cause.english(),
        }
    }
}

impl RuntimeSummary {
    fn english(self) -> String {
        match self {
            Self::NeverStarted => "container never started".into(),
            Self::Unhealthy => "container reported unhealthy".into(),
            Self::StillStarting => "container never became healthy".into(),
            Self::NoHealthcheck => "container has no health check".into(),
            Self::Paused => "container paused".into(),
            Self::Restarting => "container is restarting".into(),
            Self::Exited { code } => format!("container exited {code}"),
            Self::Removing => "container is being removed".into(),
            Self::Dead => "container is dead".into(),
            Self::Unrecognized => "container in an unrecognized state".into(),
            Self::ReportedHealthy => "monitor rejected a healthy observation".into(),
        }
    }
}

pub(super) fn visible_row_name(row: &OperationRow) -> String {
    visible_name(
        row.display_name.as_deref(),
        &row.operation,
        live_container_id(row).as_ref(),
    )
}

fn task_from_failed(failed: &FailedOperation<ExecutionError>) -> TaskView {
    let (operation, error) = match failed {
        FailedOperation::Operation { operation, error } => (operation.clone(), error),
        FailedOperation::ReplacementHealth {
            operation, error, ..
        } => (DeployOperation::ReplaceContainer(operation.clone()), error),
    };
    TaskView::from_operation(
        &operation,
        TaskState::Failed {
            cause: cause_from_error(error),
        },
    )
}

fn overlay_failed(
    tasks: &mut [TaskView],
    rows: &[OperationRow],
    failed: &FailedOperation<ExecutionError>,
) {
    let overlay = task_from_failed(failed);
    let Some(row) = failed_row_index(rows, failed).and_then(|index| tasks.get_mut(index)) else {
        return;
    };
    row.state = overlay.state;
    if row.service.is_none() {
        row.service = overlay.service;
    }
}

fn failed_row_index(
    rows: &[OperationRow],
    failed: &FailedOperation<ExecutionError>,
) -> Option<usize> {
    rows.iter()
        .position(|row| row_matches_failed(row, failed))
        .or_else(|| {
            rows.iter()
                .position(|row| matches!(row.status, OperationStatus::Failed { .. }))
        })
}

fn row_matches_failed(row: &OperationRow, failed: &FailedOperation<ExecutionError>) -> bool {
    match failed {
        FailedOperation::Operation { operation, .. } => row.operation == *operation,
        FailedOperation::ReplacementHealth { operation, .. } => {
            matches!(
                &row.operation,
                DeployOperation::ReplaceContainer(existing) if existing == operation
            )
        }
    }
}

fn subject_of(operation: &DeployOperation, name: String) -> Subject {
    match operation {
        DeployOperation::PrepareVolumes { .. } => Subject::Storage { name },
        DeployOperation::WaitHealthy { .. } => Subject::Dependency { name },
        DeployOperation::RemoveVolume { .. } => Subject::Volume { name },
        DeployOperation::RunHook { .. } | DeployOperation::StopHook { .. } => {
            Subject::Hook { name }
        }
        DeployOperation::RunContainer { .. }
        | DeployOperation::ReplaceContainer(_)
        | DeployOperation::StopContainer { .. }
        | DeployOperation::RemoveContainer { .. } => Subject::Container { name },
    }
}

fn verb_of(operation: &DeployOperation) -> Verb {
    match operation {
        DeployOperation::PrepareVolumes { .. } => Verb::Prepare,
        DeployOperation::RunContainer { .. } => Verb::Create,
        DeployOperation::ReplaceContainer(_) => Verb::Replace,
        DeployOperation::RemoveContainer { .. } | DeployOperation::RemoveVolume { .. } => {
            Verb::Remove
        }
        DeployOperation::StopContainer { .. } => Verb::Stop,
        DeployOperation::WaitHealthy { .. } => Verb::Wait,
        DeployOperation::RunHook { .. } => Verb::RunHook,
        DeployOperation::StopHook { .. } => Verb::StopHook,
    }
}

fn place_of(row: &OperationRow) -> Option<MachineName> {
    if matches!(row.operation, DeployOperation::WaitHealthy { .. }) {
        return None;
    }
    row.machine_name.clone()
}

fn task_state(row: &OperationRow) -> TaskState {
    match &row.status {
        OperationStatus::Pending => TaskState::Pending,
        OperationStatus::Unexecuted => TaskState::Unexecuted,
        OperationStatus::Completed => TaskState::Done {
            word: done_word(&row.operation),
        },
        OperationStatus::Failed { error } => TaskState::Failed {
            cause: cause_from_error(error),
        },
        OperationStatus::Running { phase } => TaskState::Running {
            pulse: pulse_of(phase),
        },
    }
}

fn pulse_of(phase: &OperationPhase) -> Pulse {
    match phase {
        OperationPhase::Starting
        | OperationPhase::CreatingContainer
        | OperationPhase::StartingContainer => Pulse::Starting,
        OperationPhase::WaitingForHealth { elapsed_ms, .. } => Pulse::WaitingHealth {
            elapsed_ms: *elapsed_ms,
        },
        OperationPhase::WaitingForHook { elapsed_ms, .. } => Pulse::WaitingHook {
            elapsed_ms: *elapsed_ms,
        },
        OperationPhase::StoppingContainer
        | OperationPhase::RemovingContainer
        | OperationPhase::RemovingVolume => Pulse::Removing,
        OperationPhase::Compensating => Pulse::Compensating,
    }
}

fn done_word(operation: &DeployOperation) -> DoneWord {
    match operation {
        DeployOperation::PrepareVolumes { .. } => DoneWord::Ready,
        DeployOperation::RemoveContainer { .. }
        | DeployOperation::StopContainer { .. }
        | DeployOperation::StopHook { .. }
        | DeployOperation::RemoveVolume { .. } => DoneWord::Removed,
        DeployOperation::WaitHealthy { .. }
        | DeployOperation::RunContainer { .. }
        | DeployOperation::ReplaceContainer(_)
        | DeployOperation::RunHook { .. } => DoneWord::Healthy,
    }
}

fn visible_name(
    display: Option<&str>,
    operation: &DeployOperation,
    live_id: Option<&ContainerId>,
) -> String {
    if let Some(display) = display.filter(|name| !is_hex_len(name, 64)) {
        return display.to_owned();
    }
    match operation {
        DeployOperation::PrepareVolumes { .. } => "provisioned volumes".into(),
        DeployOperation::WaitHealthy { dependency, .. } => dependency.to_string(),
        DeployOperation::RunContainer { spec, .. } | DeployOperation::RunHook { spec, .. } => {
            spec.name.to_string()
        }
        DeployOperation::ReplaceContainer(replacement) => replacement.spec.name.to_string(),
        DeployOperation::RemoveVolume { id } => id.name.to_string(),
        DeployOperation::StopContainer { container_id, .. }
        | DeployOperation::RemoveContainer { container_id, .. }
        | DeployOperation::StopHook { container_id, .. } => {
            short_container_id(live_id.unwrap_or(container_id))
        }
    }
}

fn live_container_id(row: &OperationRow) -> Option<ContainerId> {
    match &row.status {
        OperationStatus::Running {
            phase:
                OperationPhase::WaitingForHealth { container_id, .. }
                | OperationPhase::WaitingForHook { container_id, .. },
        } => Some(*container_id),
        OperationStatus::Failed {
            error:
                ExecutionError::Health { container_id, .. } | ExecutionError::Hook { container_id, .. },
        } => Some(*container_id),
        OperationStatus::Pending
        | OperationStatus::Running { .. }
        | OperationStatus::Completed
        | OperationStatus::Failed { .. }
        | OperationStatus::Unexecuted => row.operation.container_id(),
    }
}

fn logs_service(row: &OperationRow) -> Option<ServiceName> {
    if let DeployOperation::WaitHealthy { dependency, .. } = &row.operation {
        return Some(dependency.name.clone());
    }
    row.service_name
        .clone()
        .or_else(|| row.operation.service_name().cloned())
}

fn is_hex_len(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn short_container_id(id: &ContainerId) -> String {
    id.as_str()
        .get(..12)
        .unwrap_or_else(|| id.as_str())
        .to_owned()
}

fn cause_from_error(error: &ExecutionError) -> Cause {
    match error {
        ExecutionError::Machine { action, error } => Cause::Machine {
            action: ActionWord::from_machine(*action),
            message: error.message.clone(),
        },
        ExecutionError::Health { failure, .. } => cause_from_health(failure),
        ExecutionError::Hook { failure, .. } => cause_from_hook(failure),
        ExecutionError::DependencyHealth {
            dependency,
            failure,
        } => cause_from_dependency(dependency.to_string(), failure),
        ExecutionError::Cancelled => Cause::Cancelled,
    }
}

fn cause_from_health(failure: &HealthFailure) -> Cause {
    match failure {
        HealthFailure::TimedOut => Cause::HealthTimeout,
        HealthFailure::Cancelled => Cause::HealthCancelled,
        HealthFailure::Runtime { observation } => Cause::HealthRuntime {
            summary: runtime_summary(observation),
        },
    }
}

fn cause_from_hook(failure: &HookFailure) -> Cause {
    match failure {
        HookFailure::TimedOut { stop_error } => Cause::HookTimeout {
            stop_message: stop_error.as_ref().map(|error| error.message.clone()),
        },
        HookFailure::Cancelled { stop_error } => Cause::HookCancelled {
            stop_message: stop_error.as_ref().map(|error| error.message.clone()),
        },
        HookFailure::Exit { code } => Cause::HookExit {
            code: *code,
            stop_message: None,
        },
    }
}

fn cause_from_dependency(dependency: String, failure: &DependencyHealthFailure) -> Cause {
    match failure {
        DependencyHealthFailure::Cancelled => Cause::DependencyCancelled,
        DependencyHealthFailure::NoContainers => Cause::DependencyEmpty { dependency },
        DependencyHealthFailure::Observation { error } => Cause::DependencyObserve {
            dependency,
            message: error.message.clone(),
        },
        DependencyHealthFailure::Container { failure, .. } => Cause::DependencyContainer {
            cause: Box::new(cause_from_health(failure)),
        },
    }
}

fn runtime_summary(observation: &ContainerRuntimeObservation) -> RuntimeSummary {
    match observation {
        ContainerRuntimeObservation::Created => RuntimeSummary::NeverStarted,
        ContainerRuntimeObservation::Running { health } => match health {
            HealthObservation::Unhealthy => RuntimeSummary::Unhealthy,
            HealthObservation::Starting => RuntimeSummary::StillStarting,
            HealthObservation::NotConfigured => RuntimeSummary::NoHealthcheck,
            HealthObservation::Healthy => RuntimeSummary::ReportedHealthy,
            HealthObservation::Unrecognized(_) => RuntimeSummary::Unrecognized,
        },
        ContainerRuntimeObservation::Paused => RuntimeSummary::Paused,
        ContainerRuntimeObservation::Restarting => RuntimeSummary::Restarting,
        ContainerRuntimeObservation::Exited { code } => RuntimeSummary::Exited { code: *code },
        ContainerRuntimeObservation::Removing => RuntimeSummary::Removing,
        ContainerRuntimeObservation::Dead => RuntimeSummary::Dead,
        ContainerRuntimeObservation::Unknown { .. } => RuntimeSummary::Unrecognized,
    }
}

fn hook_line(base: &str, stop_message: &Option<String>) -> String {
    match stop_message {
        Some(message) => format!("{base}: stop also failed: {message}"),
        None => base.to_owned(),
    }
}

fn wants_logs(cause: &Cause) -> bool {
    match cause {
        Cause::HealthTimeout
        | Cause::HealthCancelled
        | Cause::HealthRuntime { .. }
        | Cause::HookTimeout { .. }
        | Cause::HookCancelled { .. }
        | Cause::HookExit { .. }
        | Cause::DependencyEmpty { .. }
        | Cause::DependencyObserve { .. }
        | Cause::DependencyContainer { .. } => true,
        Cause::Machine { .. } | Cause::Cancelled | Cause::DependencyCancelled => false,
    }
}

fn compensation_facts(
    compensation: &ReplacementCompensation<ExecutionError>,
) -> Vec<CompensationFact> {
    match compensation {
        ReplacementCompensation::StartFirst { stop_new_container } => {
            vec![stop_fact(stop_new_container)]
        }
        ReplacementCompensation::StopFirst {
            stop_new_container,
            restart_old_container,
        } => vec![
            stop_fact(stop_new_container),
            restart_fact(restart_old_container),
        ],
    }
}

fn stop_fact(attempt: &StopAttempt<ExecutionError>) -> CompensationFact {
    match attempt {
        StopAttempt::Stopped => CompensationFact::StoppedNew,
        StopAttempt::Failed { error } => CompensationFact::StopNewFailed {
            cause: cause_from_error(error),
        },
    }
}

fn restart_fact(attempt: &RestartAttempt<ExecutionError>) -> CompensationFact {
    match attempt {
        RestartAttempt::Restarted => CompensationFact::RestartedOld,
        RestartAttempt::Failed { error } => CompensationFact::RestartOldFailed {
            cause: cause_from_error(error),
        },
        RestartAttempt::NotAttempted => CompensationFact::RestartNotAttempted,
    }
}

fn compensation_line(fact: &CompensationFact) -> String {
    match fact {
        CompensationFact::StoppedNew => "stopped the new container".into(),
        CompensationFact::StopNewFailed { cause } => {
            format!("could not stop the new container: {}", cause.english())
        }
        CompensationFact::RestartedOld => "restarted the old container".into(),
        CompensationFact::RestartOldFailed { cause } => {
            format!("could not restart the old container: {}", cause.english())
        }
        CompensationFact::RestartNotAttempted => "did not restart the old container".into(),
    }
}

fn paint_row(row: &TaskView, ink: &Ink) -> String {
    let (mark, status, elapsed, role) = status_paint(&row.state);
    let mark = ink.paint(role, mark);
    let status = ink.paint(role, status);
    let place = row
        .place
        .as_ref()
        .map(|machine| format!(" on {machine}"))
        .unwrap_or_default();
    let kind = ink.paint(Role::Neutral, row.subject.kind());
    let mut line = format!(
        " {mark} {kind} {}{place}  {status}{elapsed}\n",
        row.subject.name()
    );
    if let TaskState::Failed { cause } = &row.state {
        let body = ink.paint(Role::Fail, &cause.english());
        let _ = writeln!(line, "   {body}");
    }
    line
}

fn status_paint(state: &TaskState) -> (&'static str, &'static str, String, Role) {
    match state {
        TaskState::Pending => ("•", "Pending", String::new(), Role::Idle),
        TaskState::Unexecuted => ("•", "Unexecuted", String::new(), Role::Idle),
        TaskState::Done {
            word: DoneWord::Ready,
        } => ("✔", "Ready", String::new(), Role::Done),
        TaskState::Done {
            word: DoneWord::Healthy,
        } => ("✔", "Healthy", String::new(), Role::Done),
        TaskState::Done {
            word: DoneWord::Removed,
        } => ("✔", "Removed", String::new(), Role::Done),
        TaskState::Failed { .. } => ("✖", "Failed", String::new(), Role::Fail),
        TaskState::Running {
            pulse: Pulse::Starting,
        } => ("…", "Running", String::new(), Role::Run),
        TaskState::Running {
            pulse: Pulse::WaitingHealth { elapsed_ms },
        } => ("…", "waiting for health", elapsed(*elapsed_ms), Role::Run),
        TaskState::Running {
            pulse: Pulse::WaitingHook { elapsed_ms },
        } => ("…", "waiting for hook", elapsed(*elapsed_ms), Role::Run),
        TaskState::Running {
            pulse: Pulse::Removing,
        } => ("…", "Removed", String::new(), Role::Run),
        TaskState::Running {
            pulse: Pulse::Compensating,
        } => ("…", "Compensating", String::new(), Role::Run),
    }
}

fn elapsed(elapsed_ms: u64) -> String {
    format!("  {:.1}s", elapsed_ms as f64 / 1000.0)
}
