# renderer-seam-harness (PROTOTYPE — throwaway)

Primary source for the harness decision in wayfinder ticket **#457 — "Design the
renderer ownership seam, visual vocabulary, and preview harness."** Throwaway:
nothing here merges to `main`; only the validated decision lands, in the ticket.

## The question

The renderer seam feeds one live-or-bounded event stream to one renderer. The
open decision is the **presenter** — the part that puts frame lines on the
terminal. Should the rich branch **adopt ratatui** (`Viewport::Inline` +
`insert_before`, never alt-screen, `TestBackend` goldens) or stay **hand-rolled**
(cursor-up + clear-to-end, as `follow.rs::redraw_frame` already does)?

The map's standing preference: inline output that leaves the operation result in
scrollback, never a full-screen app. So the concrete thing to watch by eye is:
**which presenter leaves committed phases and the final result in scrollback, and
how much code does each cost to do it?**

## What it does

Drives one canned deploy replay (submit → phase 1 commit → phase 2 commit →
succeeded) through three presenters, one event per keypress:

1. **hand-rolled inline** — committed lines print permanently; only the live tail
   redraws (cursor-up + clear-to-end). Never alt-screen.
2. **ratatui inline** — `insert_before` pushes committed lines into scrollback; a
   6-line inline viewport holds the live tail. Never alt-screen.
3. **full-clear naive** — `\x1b[2J\x1b[H` every frame. The anti-pattern: stable
   view, scrollback destroyed.

Play each, then quit and scroll up: (1) and (2) leave every committed phase and
`Deploy succeeded.` in history; (3) leaves only the last frame.

The `seam` module (the event→view fold and view→lines render) is the only part
worth keeping — it mirrors the real `DeployTree` shape and could lift into the
`ployz` renderer. The shell around it is disposable.

## Run

```sh
cd prototypes/renderer-seam-harness
cargo run              # interactive harness (needs a real terminal)
cargo run -- --frames  # headless: dump the pure frames, no TTY
```
