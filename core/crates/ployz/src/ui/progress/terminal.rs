//! One terminal-size observation for indicatif's width and per-row height checks.

use std::io;
use std::sync::atomic::{AtomicU16, Ordering};

use console::Term;
use indicatif::{ProgressDrawTarget, TermLike};

pub(super) fn draw_target() -> ProgressDrawTarget {
    let term = Term::buffered_stderr();
    if !term.is_term() || console::is_dumb() {
        return ProgressDrawTarget::hidden();
    }
    let height = AtomicU16::new(term.size().0);
    ProgressDrawTarget::term_like_with_hz(Box::new(DrawTerm { term, height }), 20)
}

#[derive(Debug)]
struct DrawTerm {
    term: Term,
    height: AtomicU16,
}

impl TermLike for DrawTerm {
    fn width(&self) -> u16 {
        let (height, width) = self.term.size();
        self.height.store(height, Ordering::Relaxed);
        width
    }

    fn height(&self) -> u16 {
        // indicatif samples width before checking each row against the height.
        self.height.load(Ordering::Relaxed)
    }

    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        self.term.move_cursor_up(n)
    }

    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        self.term.move_cursor_down(n)
    }

    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        self.term.move_cursor_right(n)
    }

    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        self.term.move_cursor_left(n)
    }

    fn write_line(&self, value: &str) -> io::Result<()> {
        self.term.write_line(value)
    }

    fn write_str(&self, value: &str) -> io::Result<()> {
        self.term.write_str(value)
    }

    fn clear_line(&self) -> io::Result<()> {
        self.term.clear_line()
    }

    fn flush(&self) -> io::Result<()> {
        self.term.flush()
    }
}
