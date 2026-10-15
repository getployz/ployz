//! One terminal-size observation for indicatif's width and per-row height checks.

use std::io;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

use console::Term;
use indicatif::{ProgressDrawTarget, TermLike};

/// Columns kept free right of the last live row's text, so the terminal's `^C` echo
/// can't wrap onto a row indicatif doesn't count, where its next clear would leave the
/// caption behind.
pub(super) const ECHO_ROOM: u16 = 2;

pub(super) fn draw_target() -> ProgressDrawTarget {
    let term = Term::buffered_stderr();
    if !term.is_term() || console::is_dumb() {
        return ProgressDrawTarget::hidden();
    }
    let height = AtomicU16::new(term.size().0);
    ProgressDrawTarget::term_like_with_hz(Box::new(EchoRoom::new(DrawTerm { term, height })), 20)
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

/// Ends the live block with the cursor right after the last row's text.
///
/// indicatif writes no newline between rows. It pads each row with spaces to the width
/// it is given and relies on the terminal wrapping the cursor onto the next line. Held
/// padding goes out before the next write, so every row still wraps, and the padding
/// still held at a flush is cleared to the end of the line instead.
#[derive(Debug)]
pub(super) struct EchoRoom<T> {
    term: T,
    padding: AtomicUsize,
}

impl<T: TermLike> EchoRoom<T> {
    pub(super) fn new(term: T) -> Self {
        Self {
            term,
            padding: AtomicUsize::new(0),
        }
    }

    fn pad(&self) -> io::Result<()> {
        match self.padding.swap(0, Ordering::Relaxed) {
            0 => Ok(()),
            n => self.term.write_str(&" ".repeat(n)),
        }
    }
}

impl<T: TermLike> TermLike for EchoRoom<T> {
    fn width(&self) -> u16 {
        self.term.width()
    }

    fn height(&self) -> u16 {
        self.term.height()
    }

    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        self.pad()?;
        self.term.move_cursor_up(n)
    }

    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        self.pad()?;
        self.term.move_cursor_down(n)
    }

    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        self.pad()?;
        self.term.move_cursor_right(n)
    }

    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        self.pad()?;
        self.term.move_cursor_left(n)
    }

    fn write_line(&self, value: &str) -> io::Result<()> {
        self.pad()?;
        self.term.write_line(value)
    }

    fn write_str(&self, value: &str) -> io::Result<()> {
        if value.bytes().all(|byte| byte == b' ') {
            self.padding.fetch_add(value.len(), Ordering::Relaxed);
            return Ok(());
        }
        self.pad()?;
        self.term.write_str(value)
    }

    fn clear_line(&self) -> io::Result<()> {
        self.pad()?;
        self.term.clear_line()
    }

    fn flush(&self) -> io::Result<()> {
        if self.padding.swap(0, Ordering::Relaxed) > 0 {
            self.term.write_str("\x1b[K")?;
        }
        self.term.flush()
    }
}
