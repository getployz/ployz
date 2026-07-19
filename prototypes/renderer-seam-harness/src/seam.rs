//! The renderer-seam logic under test — the bit worth keeping.
//!
//! The fold (`DeployView::ingest`) turns a stream of typed operation events into
//! a drawable snapshot; the render (`DeployView::render`) turns that snapshot
//! into frame lines. Both are pure: no I/O, no terminal codes, no time. The TUI
//! shell in `main.rs` drives this module and nothing flows back — so this fold +
//! render pair could lift into the real `ployz` renderer unchanged, while the
//! shell around it stays throwaway.
//!
//! The seam has three responsibilities in the real design; this prototype
//! exercises the first two and lets the shell exercise the third:
//!   1. fold   — events -> view          (here: `ingest`)
//!   2. render — view  -> frame lines    (here: `render`, pure, style-parametric)
//!   3. present — frame lines -> terminal (the shell: hand-rolled vs ratatui)
//! The question #457 asks lives entirely in (3); (1) and (2) are here only so the
//! frames the presenter draws are the real, settled deploy vocabulary.

/// A committed line has reached its terminal position — "position is time", so
/// it will never move again and can scroll into history. A live line is the
/// in-flight tail the presenter keeps redrawing until it, too, commits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    Committed,
    Live,
}

/// One rendered frame line plus whether it has committed. The presenter uses the
/// flag to decide what may scroll into scrollback (committed) versus what stays
/// in the live redraw region (live).
#[derive(Clone)]
pub struct Line {
    pub text: String,
    pub commit: Commit,
}

impl Line {
    fn committed(text: impl Into<String>) -> Self {
        Self { text: text.into(), commit: Commit::Committed }
    }

    fn live(text: impl Into<String>) -> Self {
        Self { text: text.into(), commit: Commit::Live }
    }
}

/// Colour axis. The invariant under test elsewhere: colour is never the sole
/// carrier of a state — every line already reads correctly with `Off`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Color {
    On,
    Off,
}

/// A minimal slice of the real deploy event stream — enough phases, containers,
/// and a commit barrier to make the "does the committed line stay in scrollback"
/// question concrete. Mirrors `ployz_core::operation::OperationEvent` in shape,
/// not in full fidelity (this is a prototype).
#[derive(Clone)]
pub enum DeployEvent {
    Submitted { op: String, namespace: String, services: usize },
    ImagesReady,
    PhaseStarted { phase: usize, label: String, of: usize },
    ContainerHealthy { service: String, replica: String, machine: String },
    PhaseCommitted { phase: usize },
    Completed,
}

/// The folded snapshot. Committed groups have scrolled to their final position;
/// `live` is whatever is still in flight.
pub struct DeployView {
    op: Option<String>,
    namespace: String,
    services: usize,
    images_ready: bool,
    committed_lines: Vec<Line>,
    current_phase: Option<(usize, String, usize)>,
    healthy: Vec<(String, String, String)>,
    terminal: bool,
}

impl DeployView {
    pub fn new() -> Self {
        Self {
            op: None,
            namespace: String::new(),
            services: 0,
            images_ready: false,
            committed_lines: Vec::new(),
            current_phase: None,
            healthy: Vec::new(),
            terminal: false,
        }
    }

    /// The fold. The SAME call for a live feed and a bounded replay feed —
    /// boundedness is invisible here; the shell just stops sending events.
    pub fn ingest(&mut self, event: &DeployEvent) {
        match event {
            DeployEvent::Submitted { op, namespace, services } => {
                self.op = Some(op.clone());
                self.namespace = namespace.clone();
                self.services = *services;
            }
            DeployEvent::ImagesReady => self.images_ready = true,
            DeployEvent::PhaseStarted { phase, label, of } => {
                self.current_phase = Some((*phase, label.clone(), *of));
                self.healthy.clear();
            }
            DeployEvent::ContainerHealthy { service, replica, machine } => {
                self.healthy.push((service.clone(), replica.clone(), machine.clone()));
            }
            DeployEvent::PhaseCommitted { phase } => {
                // A commit barrier: the phase group reaches its final position
                // and moves into the committed region, never to be redrawn.
                if let Some((_, label, _)) = self.current_phase.take() {
                    self.committed_lines.push(Line::committed(format!("[+] Phase {phase} · {label}")));
                    for (service, replica, machine) in self.healthy.drain(..) {
                        self.committed_lines
                            .push(Line::committed(format!("    ✔ {service}.{replica} on {machine} — healthy")));
                    }
                    self.committed_lines.push(Line::committed(format!("✔ Phase {phase} committed")));
                }
            }
            DeployEvent::Completed => self.terminal = true,
        }
    }

    /// The render. Pure function of the snapshot: committed lines first (they
    /// have scrolled into place), then the live tail. `_color` would drive the
    /// style layer; the text carries every state on its own, so the frame reads
    /// identically with colour off — which is the whole point of the invariant.
    pub fn render(&self, _color: Color) -> Vec<Line> {
        let mut lines = Vec::new();
        let Some(op) = &self.op else {
            return lines;
        };

        lines.push(Line::committed(format!(
            "Deploy {op} started — namespace {}, {} services",
            self.namespace, self.services
        )));
        lines.extend(self.committed_lines.iter().cloned());

        if self.terminal {
            lines.push(Line::committed(String::new()));
            lines.push(Line::committed("Deploy succeeded.".to_string()));
            return lines;
        }

        // The live tail: whatever is in flight right now.
        if !self.images_ready {
            lines.push(Line::live("  images — resolving …".to_string()));
        } else if let Some((phase, label, of)) = &self.current_phase {
            lines.push(Line::live(format!("[+] Phase {phase} · {label}    0/{of}")));
            for (service, replica, machine) in &self.healthy {
                lines.push(Line::live(format!("    ● {service}.{replica} on {machine} — healthy")));
            }
            lines.push(Line::live("    ◐ starting …".to_string()));
        } else if self.images_ready {
            lines.push(Line::committed("  ✔ images ready".to_string()));
            lines.push(Line::live("  waiting for first phase …".to_string()));
        }
        lines
    }
}
