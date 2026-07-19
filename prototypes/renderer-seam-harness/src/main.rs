//! PROTOTYPE — throwaway shell for wayfinder ticket #457.
//!
//! ## The question
//!
//! The renderer seam feeds one live-or-bounded event stream to one renderer.
//! The open decision is the *presenter*: should the rich branch adopt ratatui
//! (`Viewport::Inline` + `insert_before`, never alt-screen, `TestBackend`
//! goldens), or stay hand-rolled? The map's standing preference is inline output
//! that leaves the operation result in scrollback, never a full-screen app.
//!
//! This shell drives one canned deploy replay through three presenters so a
//! human can watch each one and answer, by eye: which of these leaves committed
//! phases and the final result in the terminal's scrollback, and how much code
//! each costs.
//!
//!   1. hand-rolled inline  — print committed lines permanently, redraw only the
//!                            live tail with cursor-up + clear-to-end. This is
//!                            `follow.rs::redraw_frame`, refined to let committed
//!                            lines scroll into history. Never alt-screen.
//!   2. ratatui inline      — `insert_before` pushes committed lines into
//!                            scrollback; the live tail redraws in an inline
//!                            viewport. Never alt-screen.
//!   3. full-clear (naive)  — `\x1b[2J\x1b[H` every frame. The anti-pattern:
//!                            one stable view, but scrollback is destroyed.
//!
//! Only the `seam` module (the fold + render) is worth keeping; this shell is
//! throwaway. Run `cargo run` for the interactive harness, or `cargo run --
//! --frames` to dump the pure frames with no TTY.

mod seam;

use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::TerminalOptions;
use ratatui::Viewport;
use ratatui::backend::CrosstermBackend;
use ratatui::text::Line as TuiLine;
use ratatui::widgets::{Paragraph, Widget};

use seam::{Color, Commit, DeployEvent, DeployView, Line};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Presenter {
    HandRolledInline,
    RatatuiInline,
    FullClearNaive,
}

impl Presenter {
    fn label(self) -> &'static str {
        match self {
            Presenter::HandRolledInline => "1 · hand-rolled inline (cursor-up redraw, committed lines persist)",
            Presenter::RatatuiInline => "2 · ratatui inline (insert_before + inline viewport)",
            Presenter::FullClearNaive => "3 · full-clear naive (wipes screen every frame — loses scrollback)",
        }
    }
}

fn replay() -> Vec<DeployEvent> {
    vec![
        DeployEvent::Submitted { op: "op_317".into(), namespace: "prod".into(), services: 2 },
        DeployEvent::ImagesReady,
        DeployEvent::PhaseStarted { phase: 1, label: "data".into(), of: 1 },
        DeployEvent::ContainerHealthy { service: "web".into(), replica: "1".into(), machine: "hetzner-1".into() },
        DeployEvent::PhaseCommitted { phase: 1 },
        DeployEvent::PhaseStarted { phase: 2, label: "edge".into(), of: 2 },
        DeployEvent::ContainerHealthy { service: "web".into(), replica: "2".into(), machine: "hetzner-2".into() },
        DeployEvent::PhaseCommitted { phase: 2 },
        DeployEvent::Completed,
    ]
}

fn view_at(events: &[DeployEvent], upto: usize) -> DeployView {
    let mut view = DeployView::new();
    for event in &events[..upto] {
        view.ingest(event);
    }
    view
}

fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--frames") {
        return dump_frames();
    }
    run_interactive()
}

/// Headless: print the pure frame at each step. No TTY, no raw mode — proves the
/// fold + render logic without a terminal, and gives a pipeable view.
fn dump_frames() -> io::Result<()> {
    let events = replay();
    let mut out = io::stdout().lock();
    for step in 1..=events.len() {
        let view = view_at(&events, step);
        writeln!(out, "── after event {step}/{} ──", events.len())?;
        for line in view.render(Color::Off) {
            let mark = match line.commit {
                Commit::Committed => " ",
                Commit::Live => "~",
            };
            writeln!(out, "{mark} {}", line.text)?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn run_interactive() -> io::Result<()> {
    let events = replay();
    let mut presenter = Presenter::HandRolledInline;
    let mut color = Color::On;

    loop {
        menu(presenter, color)?;
        match read_key()? {
            KeyCode::Char('1') => presenter = Presenter::HandRolledInline,
            KeyCode::Char('2') => presenter = Presenter::RatatuiInline,
            KeyCode::Char('3') => presenter = Presenter::FullClearNaive,
            KeyCode::Char('c') => color = toggle(color),
            KeyCode::Enter | KeyCode::Char(' ') => play(&events, presenter, color)?,
            KeyCode::Char('q') => return Ok(()),
            _ => {}
        }
    }
}

fn menu(presenter: Presenter, color: Color) -> io::Result<()> {
    let mut out = io::stdout().lock();
    write!(out, "\x1b[2J\x1b[H")?;
    writeln!(out, "renderer-seam-harness — pick a presenter, then step a deploy replay\r")?;
    writeln!(out, "\r")?;
    for candidate in [Presenter::HandRolledInline, Presenter::RatatuiInline, Presenter::FullClearNaive] {
        let cursor = if candidate == presenter { "→" } else { " " };
        writeln!(out, "  {cursor} {}\r", candidate.label())?;
    }
    writeln!(out, "\r")?;
    writeln!(out, "  colour: {}\r", match color { Color::On => "on", Color::Off => "off" })?;
    writeln!(out, "\r")?;
    writeln!(out, "  [1/2/3] choose   [c] colour   [enter] play   [q] quit\r")?;
    out.flush()
}

/// Play the replay through the chosen presenter, one event per keypress, then
/// leave whatever it drew on screen so the human can scroll up and inspect what
/// survived in scrollback.
fn play(events: &[DeployEvent], presenter: Presenter, color: Color) -> io::Result<()> {
    print!("\x1b[2J\x1b[H");
    io::stdout().flush()?;
    match presenter {
        Presenter::HandRolledInline => play_hand_rolled(events, color),
        Presenter::FullClearNaive => play_full_clear(events, color),
        Presenter::RatatuiInline => play_ratatui(events, color),
    }
}

fn play_hand_rolled(events: &[DeployEvent], color: Color) -> io::Result<()> {
    let mut out = io::stdout().lock();
    let mut printed_committed = 0usize;
    let mut prev_live = 0usize;
    for step in 1..=events.len() {
        let view = view_at(events, step);
        let lines = view.render(color);
        let committed: Vec<&Line> = lines.iter().filter(|l| l.commit == Commit::Committed).collect();
        let live: Vec<&Line> = lines.iter().filter(|l| l.commit == Commit::Live).collect();

        // Erase the previous live tail only; committed lines already scrolled
        // into history and are never touched again.
        if prev_live > 0 {
            write!(out, "\x1b[{prev_live}A\x1b[0J")?;
        }
        // Newly-committed lines print permanently (into scrollback).
        for line in committed.iter().skip(printed_committed) {
            write!(out, "{}\r\n", line.text)?;
        }
        printed_committed = committed.len();
        // Redraw the live tail.
        for line in &live {
            write!(out, "{}\r\n", line.text)?;
        }
        prev_live = live.len();
        out.flush()?;
        wait_step(&mut out)?;
    }
    banner(&mut out, "hand-rolled inline: committed phases + result are above in scrollback")
}

fn play_full_clear(events: &[DeployEvent], color: Color) -> io::Result<()> {
    let mut out = io::stdout().lock();
    for step in 1..=events.len() {
        let view = view_at(events, step);
        write!(out, "\x1b[2J\x1b[H")?;
        for line in view.render(color) {
            write!(out, "{}\r\n", line.text)?;
        }
        out.flush()?;
        wait_step(&mut out)?;
    }
    banner(&mut out, "full-clear naive: scroll up — earlier frames are GONE, only this one remains")
}

fn play_ratatui(events: &[DeployEvent], color: Color) -> io::Result<()> {
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport: Viewport::Inline(6) })?;
    let mut inserted_committed = 0usize;

    for step in 1..=events.len() {
        let view = view_at(events, step);
        let lines = view.render(color);
        let committed: Vec<Line> = lines.iter().filter(|l| l.commit == Commit::Committed).cloned().collect();
        let live: Vec<Line> = lines.iter().filter(|l| l.commit == Commit::Live).cloned().collect();

        // insert_before pushes newly-committed lines into scrollback, above the
        // inline viewport — the ratatui idiom for "this is history now".
        for line in committed.iter().skip(inserted_committed) {
            let text = line.text.clone();
            terminal.insert_before(1, move |buf| {
                let area = buf.area;
                Paragraph::new(TuiLine::from(text.as_str())).render(area, buf);
            })?;
        }
        inserted_committed = committed.len();

        // The live tail redraws inside the inline viewport.
        terminal.draw(|frame| {
            let body: Vec<TuiLine> = live.iter().map(|l| TuiLine::from(l.text.as_str())).collect();
            frame.render_widget(Paragraph::new(body), frame.area());
        })?;
        wait_step_raw()?;
    }

    // Clear the (now empty) live viewport so only scrollback remains.
    terminal.clear()?;
    let mut out = io::stdout().lock();
    banner(&mut out, "ratatui inline: committed phases + result are above in scrollback (via insert_before)")
}

fn wait_step(out: &mut impl Write) -> io::Result<()> {
    write!(out, "\x1b[2m  [space] next · [q] stop\x1b[0m\r\n")?;
    out.flush()?;
    // The prompt line is part of the live tail; nothing to erase here because
    // callers redraw from their own tracked baseline.
    if matches!(read_key()?, KeyCode::Char('q')) {
        return Ok(());
    }
    // Erase the prompt line we just wrote.
    write!(out, "\x1b[1A\x1b[0J")?;
    Ok(())
}

fn wait_step_raw() -> io::Result<()> {
    let _ = read_key()?;
    Ok(())
}

fn banner(out: &mut impl Write, message: &str) -> io::Result<()> {
    write!(out, "\r\n\x1b[1m{message}\x1b[0m\r\n")?;
    write!(out, "\x1b[2mpress any key to return to the menu\x1b[0m\r\n")?;
    out.flush()?;
    let _ = read_key()?;
    Ok(())
}

fn read_key() -> io::Result<KeyCode> {
    enable_raw_mode()?;
    let code = loop {
        if let Event::Key(key) = event::read()? {
            break key.code;
        }
    };
    disable_raw_mode()?;
    Ok(code)
}

fn toggle(color: Color) -> Color {
    match color {
        Color::On => Color::Off,
        Color::Off => Color::On,
    }
}
