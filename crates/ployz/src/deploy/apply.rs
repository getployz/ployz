use std::{
    io::{self, IsTerminal, Write},
    num::NonZeroU32,
};

use ployz_core::{
    DataLossConfirmation, DeployEvent, DeployIntent, OperationRow, ProjectName,
    RequestedServiceSpec, ServiceSelector,
};
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use crate::{
    compose::{BuildService, CapturedCompose},
    connect::Client,
    failure::Failure,
    project::ResolvedProject,
};

use super::{
    DeployError, DeployOutcome, DeployPlan, DeployPreview, ExecutionError, VolumeFate,
    pipeline::{PushOutcome, plan_options, plan_project, plan_scale, push_project_images},
    render,
    report::{self, Ink},
};

pub(crate) async fn deploy_spec(
    client: &mut Client,
    requested: &RequestedServiceSpec,
    force_recreate: bool,
    skip_health_monitor: bool,
    project_name: &ProjectName,
    context: &str,
    project: Option<&ResolvedProject>,
) -> Result<(), Failure> {
    apply_spec(
        client,
        requested,
        force_recreate,
        skip_health_monitor,
        project_name,
        context,
        project,
    )
    .await
    .map_err(Into::into)
}

async fn apply_spec(
    client: &mut Client,
    requested: &RequestedServiceSpec,
    force_recreate: bool,
    skip_health_monitor: bool,
    project_name: &ProjectName,
    context: &str,
    project: Option<&ResolvedProject>,
) -> Result<(), ApplyError> {
    let preview = crate::setup_retry::run(
        client,
        "Preparing service deployment",
        crate::setup_retry::WAIT,
        |error| matches!(error, DeployError::Connect(error) if error.is_setup_retryable()),
        async |client| {
            client
                .preview(DeployIntent::apply_one(
                    project_name.clone(),
                    requested.clone(),
                    plan_options(force_recreate, skip_health_monitor),
                ))
                .await
        },
    )
    .await
    .map_err(|error| ApplyError::Prepare(error.into()))?;
    print_warnings(&preview);
    if preview.noop() {
        let source = project.map(|project| project.source.to_string());
        print!(
            "{}",
            render::plan_text(&preview, context, source.as_deref())
        );
        return Ok(());
    }
    finish(
        stream_confirm(
            client,
            &preview,
            format!("Running service {}", requested.name),
            Ink::detect(io::stdout()),
        )
        .await,
        &format!("Deployed to {context}"),
        preview.cluster_domain.as_deref(),
    )
}

pub(crate) async fn apply_requested(
    client: &mut Client,
    requested: &RequestedServiceSpec,
) -> Result<(), ApplyError> {
    apply_spec(
        client,
        requested,
        false,
        false,
        &ProjectName::system(),
        "default",
        None,
    )
    .await
}

/// Keep execution evidence available for the closing deployment report.
#[derive(Debug)]
pub(crate) enum ApplyError {
    Prepare(Failure),
    Execute {
        outcome: Box<DeployOutcome<ExecutionError>>,
        rows: Vec<OperationRow>,
        live_shown: bool,
    },
}

impl From<DeployError> for ApplyError {
    fn from(error: DeployError) -> Self {
        Self::Prepare(error.into())
    }
}

impl From<ApplyError> for Failure {
    fn from(error: ApplyError) -> Self {
        match error {
            ApplyError::Prepare(error) => error,
            ApplyError::Execute {
                outcome,
                rows,
                live_shown,
            } => {
                let text =
                    report::paint_closing(&outcome, &rows, live_shown, &Ink::detect(io::stderr()));
                Failure::usage(text.trim().to_owned())
            }
        }
    }
}

pub(crate) struct ConfirmGate<'a> {
    pub auto_confirm: bool,
    pub context: &'a str,
    pub project: &'a ResolvedProject,
}

pub(crate) async fn deploy_project(
    client: &mut Client,
    candidate: &CapturedCompose,
    builds: &[BuildService],
    gate: ConfirmGate<'_>,
) -> Result<(), Failure> {
    let machines = client.machines().await?;
    let outcome = push_project_images(client, builds, &machines).await?;
    print_pushed_images(&outcome);
    if !outcome.failures.is_empty() {
        return Err(Failure::usage(format!(
            "image push failed: {}",
            outcome.failures.join("; ")
        )));
    }
    let preview = plan_project(client, candidate, machines).await?;
    println!("Captured candidate {}", candidate.id());
    print_warnings(&preview);
    confirm_and_execute(client, &preview, gate).await
}

pub(crate) async fn deploy_scale(
    client: &mut Client,
    selector: &ServiceSelector,
    replicas: NonZeroU32,
    skip_health_monitor: bool,
    gate: ConfirmGate<'_>,
) -> Result<(), Failure> {
    let (preview, project_name) = plan_scale(
        client,
        selector,
        replicas,
        plan_options(false, skip_health_monitor),
    )
    .await?;
    print_warnings(&preview);
    let project = ResolvedProject {
        name: project_name,
        source: gate.project.source,
    };
    confirm_and_execute(
        client,
        &preview,
        ConfirmGate {
            auto_confirm: gate.auto_confirm,
            context: gate.context,
            project: &project,
        },
    )
    .await
}

async fn confirm_and_execute(
    client: &Client,
    preview: &DeployPlan,
    gate: ConfirmGate<'_>,
) -> Result<(), Failure> {
    let source = Some(gate.project.source.to_string());
    print!(
        "{}",
        render::plan_text(preview, gate.context, source.as_deref())
    );
    if preview.noop() {
        return Ok(());
    }
    if !gate.auto_confirm && !confirm(&render::confirm_prompt(gate.context))? {
        println!("No changes were made.");
        return Ok(());
    }
    finish(
        stream_confirm(
            client,
            preview,
            format!("Deploying to {}", gate.context),
            Ink::detect(io::stdout()),
        )
        .await,
        &format!("Deployed to {}", gate.context),
        preview.cluster_domain.as_deref(),
    )
    .map_err(Into::into)
}

pub(crate) async fn remove_project(
    client: &mut Client,
    name: &ProjectName,
    volumes: VolumeFate,
    context: &str,
    confirm_data_loss: &DataLossConfirmation,
) -> Result<(), Failure> {
    let preview = client
        .prepare_project_destroy(name, confirm_data_loss, volumes)
        .await
        .map_err(crate::failure::refusal_from_rpc)?;
    print_warnings(&preview);
    print!("{}", render::removal_plan_text(&preview, context));
    if preview.noop() {
        return Ok(());
    }
    finish(
        stream_confirm(
            client,
            &preview,
            format!("Removing Project {name} from {context}"),
            Ink::detect(io::stdout()),
        )
        .await,
        &format!("Removed Project {name} from {context}"),
        preview.cluster_domain.as_deref(),
    )
    .map_err(Into::into)
}

async fn stream_confirm(
    client: &Client,
    preview: &DeployPlan,
    title: String,
    ink: Ink,
) -> (DeployOutcome<ExecutionError>, ProgressPrinter) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let abort = cancel.clone();
    let ctrl_c = tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        abort.cancel();
    });
    let execute = client.confirm(preview, &cancel, Some(tx));
    tokio::pin!(execute);
    let mut printer = ProgressPrinter::new(title, ink);
    let outcome = loop {
        tokio::select! {
            event = rx.recv() => {
                if let Some(event) = event {
                    printer.print(&event);
                }
            }
            outcome = &mut execute => {
                while let Ok(event) = rx.try_recv() {
                    printer.print(&event);
                }
                break outcome;
            }
        }
    };
    ctrl_c.abort();
    (outcome, printer)
}

struct ProgressPrinter {
    title: String,
    last_rows: Vec<OperationRow>,
    live_shown: bool,
    ink: Ink,
    last_terminal_rows: usize,
    last_signature: Option<String>,
}

impl ProgressPrinter {
    fn new(title: String, ink: Ink) -> Self {
        Self {
            title,
            last_rows: Vec::new(),
            live_shown: false,
            ink,
            last_terminal_rows: 0,
            last_signature: None,
        }
    }

    fn print(&mut self, event: &DeployEvent) {
        let DeployEvent::Progress {
            rows,
            completed,
            total,
        } = event
        else {
            return;
        };
        let signature = progress_signature(event);
        let tty = io::stdout().is_terminal();
        if !tty && self.last_signature.as_ref() == Some(&signature) {
            return;
        }
        self.last_rows = rows.clone();
        let text = report::paint_live(&self.title, *completed, *total, rows, &self.ink);
        if tty && self.last_terminal_rows > 0 {
            print!("\x1b[{}F\x1b[J", self.last_terminal_rows);
        }
        print!("{text}");
        let _ = io::stdout().flush();
        if tty {
            let columns = crossterm::terminal::size().map_or(80, |(columns, _)| columns);
            let plain = report::paint_live(&self.title, *completed, *total, rows, &Ink::plain());
            self.last_terminal_rows = terminal_rows(&plain, usize::from(columns));
        }
        self.last_signature = Some(signature);
        self.live_shown = true;
    }
}

fn terminal_rows(plain_text: &str, columns: usize) -> usize {
    let columns = columns.max(1);
    plain_text
        .lines()
        .map(|line| {
            let mut rows = 1;
            let mut column = 0;
            for grapheme in line.graphemes(true) {
                let width = grapheme.width();
                if width > 0 && column > 0 && column + width > columns {
                    rows += 1;
                    column = 0;
                }
                column += width;
            }
            rows
        })
        .sum()
}

fn progress_signature(event: &DeployEvent) -> String {
    match event {
        DeployEvent::Progress {
            rows, completed, ..
        } => {
            let kinds: Vec<_> = rows
                .iter()
                .map(|row| render::status_kind(&row.status))
                .collect();
            format!("{completed}:{}", kinds.join(","))
        }
        DeployEvent::Outcome { .. } => String::new(),
    }
}

fn print_pushed_images(outcome: &PushOutcome) {
    for pushed in &outcome.pushed {
        println!("Pushed {} to {}", pushed.image, pushed.machine_id);
    }
}

fn print_warnings(preview: &DeployPreview) {
    for warning in &preview.warnings {
        eprintln!("WARNING: {warning}");
    }
}

fn confirm(prompt: &str) -> Result<bool, Failure> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Failure::usage(
            "confirmation requires a terminal; pass --yes to continue",
        ));
    }
    print!("{prompt}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn finish(
    (outcome, printer): (DeployOutcome<ExecutionError>, ProgressPrinter),
    success_title: &str,
    cluster_domain: Option<&str>,
) -> Result<(), ApplyError> {
    match outcome {
        DeployOutcome::Success { completed } => {
            let text = render::success_text(&completed, success_title, cluster_domain);
            if io::stdout().is_terminal() && printer.last_terminal_rows > 0 {
                print!("\x1b[{}F\x1b[J", printer.last_terminal_rows);
            }
            print!("{text}");
            Ok(())
        }
        failed @ DeployOutcome::Failed { .. } => Err(ApplyError::Execute {
            outcome: Box::new(failed),
            rows: printer.last_rows,
            live_shown: printer.live_shown,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::super::pipeline::project_not_found;
    use super::*;
    use crate::deploy::DeployWarning;
    use crate::dns::ingress_dns_warnings;
    use ployz_core::{
        DeployOperation, FailedOperation, MachineAction, MachineId, PruneRefusal,
        RequestedServiceSpec, RpcError, RpcErrorCode,
    };

    #[test]
    fn progress_frame_counts_soft_wrapped_terminal_rows() {
        for (text, columns, expected) in [
            (
                "[+] Deploying to default 1/1\n✔ Container cashdash-frontend on machine1 Healthy\n",
                20,
                5,
            ),
            ("abcd\n", 4, 1),
            ("abcde\n\n", 4, 3),
            ("界界界\n", 3, 3),
            ("e\u{301}e\u{301}\n", 2, 1),
            ("👩‍💻👩‍💻\n", 2, 2),
            ("ab\n", 0, 2),
            ("", 20, 0),
        ] {
            assert_eq!(
                terminal_rows(text, columns),
                expected,
                "{columns}: {text:?}"
            );
        }
    }

    #[test]
    fn deploy_prints_ingress_misses_as_warning_lines_without_failing() {
        let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
            "name": "web",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "nginx", "pull_policy": "missing" },
            "ports": [
                {
                    "mode": "ingress",
                    "hostname": { "kind": "explicit", "hostname": "app.example.com" },
                    "load_balancer_port": 443,
                    "container_port": 8080,
                    "http_protocol": "https"
                },
                {
                    "mode": "ingress",
                    "hostname": { "kind": "explicit", "hostname": "plain.example.com" },
                    "load_balancer_port": 80,
                    "container_port": 8080,
                    "http_protocol": "http"
                }
            ]
        }))
        .unwrap();
        let cluster = ["192.0.2.1".parse().unwrap()];
        let preview = DeployPreview::new(
            Vec::new(),
            ingress_dns_warnings([&spec], &cluster, |hostname| match hostname.as_str() {
                "app.example.com" => vec!["198.51.100.10".parse().unwrap()],
                "plain.example.com" => Vec::new(),
                other => panic!("unexpected {other}"),
            })
            .into_iter()
            .map(DeployWarning::from)
            .collect(),
            ProjectName::parse("app").unwrap(),
        );
        assert_eq!(
            preview
                .warnings
                .iter()
                .map(|warning| format!("WARNING: {warning}"))
                .collect::<Vec<_>>(),
            [
                "WARNING: Ingress Hostname app.example.com resolves to 198.51.100.10; it should resolve to 192.0.2.1. A certificate cannot be issued until it points at this Cluster.",
                "WARNING: Ingress Hostname plain.example.com does not resolve; it should resolve to 192.0.2.1.",
            ]
        );
        assert!(
            !preview
                .warnings
                .iter()
                .map(|warning| format!("WARNING: {warning}"))
                .any(|line| line.contains("plain.example.com")
                    && line.to_ascii_lowercase().contains("certificate"))
        );
    }

    #[test]
    fn incomplete_empty_view_is_not_reported_as_missing() {
        let mut preview =
            DeployPreview::new(Vec::new(), Vec::new(), ProjectName::parse("shop").unwrap());
        assert!(project_not_found(&preview));
        preview.prune_refusal = Some(PruneRefusal::IncompleteSnapshot);
        assert!(!project_not_found(&preview));
    }

    #[test]
    fn interrupted_execution_keeps_outcome_after_stderr() {
        let machine_id = MachineId::parse("d".repeat(32)).unwrap();
        let outcome = DeployOutcome::Failed {
            completed: Vec::new(),
            failed: FailedOperation::Operation {
                operation: DeployOperation::RunContainer {
                    machine_id,
                    spec: serde_json::from_value(serde_json::json!({
                        "service_id": "a".repeat(32),
                        "name": "web",
                        "mode": { "mode": "replicated", "replicas": 1 },
                        "container": { "image": "nginx", "pull_policy": "missing" }
                    }))
                    .unwrap(),
                    skip_health_monitor: true,
                },
                error: ExecutionError::Machine {
                    action: MachineAction::CreateContainer,
                    error: RpcError {
                        code: RpcErrorCode::Unavailable,
                        message: "target Machine RPC timed out".into(),
                        details: serde_json::Value::Null,
                    },
                },
            },
            unexecuted: Vec::new(),
        };
        let error = ApplyError::Execute {
            outcome: Box::new(outcome),
            rows: Vec::new(),
            live_shown: false,
        };
        let failure = Failure::from(error);
        assert!(format!("{failure}").contains("create failed"));
    }
}
