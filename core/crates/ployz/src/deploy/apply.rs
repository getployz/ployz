use ployz_core::{DeployEvent, DeployIntent, Namespace, OperationRow, RequestedServiceSpec};
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use crate::{connect::Client, failure::Failure, output::say_inline};

use super::{
    DeployError, DeployOutcome, DeployPlan, DeployPreview, ExecutionError,
    pipeline::plan_options,
    render,
    report::{self, Ink},
};

/// Apply one Ployz infrastructure Service in the reserved system Namespace.
pub(crate) async fn apply_requested(
    client: &mut Client,
    requested: &RequestedServiceSpec,
    force_recreate: bool,
    skip_health_monitor: bool,
    context: &str,
) -> Result<Outcome, ApplyError> {
    let preview = crate::setup_retry::run(
        client,
        "Preparing service deployment",
        crate::setup_retry::WAIT,
        |error| matches!(error, DeployError::Connect(error) if error.is_setup_retryable()),
        async |client| {
            client
                .preview(DeployIntent::apply_one(
                    Namespace::system(),
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
        say_inline!("{}", render::plan_text(&preview, context));
        return Ok(nothing_done());
    }
    let cancellation = crate::cancellation::on_ctrl_c();
    let _stop_listener = cancellation.clone().drop_guard();
    finish(
        stream_confirm(
            client,
            &preview,
            format!("Running service {}", requested.name),
            Ink::human(),
            &cancellation,
        )
        .await,
        &format!("Deployed to {context}"),
    )
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
            } => closing_failure(&outcome, &rows, live_shown),
        }
    }
}

/// The closing report of a failed execution, with its outcome as evidence.
pub(super) fn closing_failure(
    outcome: &DeployOutcome<ExecutionError>,
    rows: &[OperationRow],
    live_shown: bool,
) -> Failure {
    let text = report::paint_closing(outcome, rows, live_shown, &Ink::plain());
    Failure::detailed(
        ployz_core::RpcErrorCode::Internal,
        text.trim().to_owned(),
        serde_json::json!({ "outcome": outcome }),
    )
}

async fn stream_confirm(
    client: &Client,
    preview: &DeployPlan,
    title: String,
    ink: Ink,
    cancel: &CancellationToken,
) -> (DeployOutcome<ExecutionError>, ProgressPrinter) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let execute = client.confirm(preview, cancel, Some(tx));
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
        let tty = crate::output::human_is_terminal();
        if !tty && self.last_signature.as_ref() == Some(&signature) {
            return;
        }
        self.last_rows = rows.clone();
        let text = report::paint_live(&self.title, *completed, *total, rows, &self.ink);
        if tty && self.last_terminal_rows > 0 {
            say_inline!("\x1b[{}F\x1b[J", self.last_terminal_rows);
        }
        say_inline!("{text}");
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
        DeployEvent::Outcome { .. } | DeployEvent::ImagesPruned { .. } => String::new(),
    }
}

fn print_warnings(preview: &DeployPreview) {
    for warning in &preview.warnings {
        eprintln!("WARNING: {warning}");
    }
}

/// Terminal evidence of a Deploy that ran to completion.
pub(crate) type Outcome = DeployOutcome<ExecutionError>;

/// A no-op, declined, or already-converged Deploy: nothing to do, nothing done.
fn nothing_done() -> Outcome {
    DeployOutcome::Success {
        completed: Vec::new(),
    }
}

fn finish(
    (outcome, printer): (DeployOutcome<ExecutionError>, ProgressPrinter),
    success_title: &str,
) -> Result<Outcome, ApplyError> {
    match outcome {
        DeployOutcome::Success { completed } => {
            let text = render::success_text(&completed, success_title);
            if crate::output::human_is_terminal() && printer.last_terminal_rows > 0 {
                say_inline!("\x1b[{}F\x1b[J", printer.last_terminal_rows);
            }
            say_inline!("{text}");
            Ok(DeployOutcome::Success { completed })
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
    use super::super::pipeline::namespace_not_found;
    use super::*;
    use crate::deploy::DeployWarning;
    use crate::dns::ingress_dns_warnings;
    use ployz_core::{
        DeployOperation, FailedOperation, MachineAction, MachineId, PruneRefusal,
        RequestedServiceSpec, RpcError, RpcErrorCode,
    };

    #[test]
    fn the_resolved_color_choice_decides_the_deploy_ink() {
        assert_eq!(Ink::of(anstream::ColorChoice::Never), Ink::plain());
        assert_eq!(Ink::of(anstream::ColorChoice::Always), Ink::color());
        assert_eq!(Ink::of(anstream::ColorChoice::AlwaysAnsi), Ink::color());
    }

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
                    "hostname": "app.example.com",
                    "load_balancer_port": 443,
                    "container_port": 8080,
                    "http_protocol": "https"
                },
                {
                    "mode": "ingress",
                    "hostname": "plain.example.com",
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
                "app.example.com" => {
                    ployz_core::HostnameVerdict::Refused(ployz_core::Refusal::ReachesElsewhere)
                }
                "plain.example.com" => {
                    ployz_core::HostnameVerdict::Refused(ployz_core::Refusal::DoesNotResolve)
                }
                other => panic!("unexpected {other}"),
            })
            .into_iter()
            .map(DeployWarning::from)
            .collect(),
            Namespace::parse("app").unwrap(),
        );
        assert_eq!(
            preview
                .warnings
                .iter()
                .map(|warning| format!("WARNING: {warning}"))
                .collect::<Vec<_>>(),
            [
                "WARNING: app.example.com answers from another server. Point it at 192.0.2.1. A certificate cannot be issued until then.",
                "WARNING: plain.example.com does not resolve. Add a DNS record pointing at 192.0.2.1.",
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
            DeployPreview::new(Vec::new(), Vec::new(), Namespace::parse("shop").unwrap());
        assert!(namespace_not_found(&preview));
        preview.prune_refusal = Some(PruneRefusal::IncompleteSnapshot);
        assert!(!namespace_not_found(&preview));
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
