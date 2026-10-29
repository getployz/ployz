//! Questions put to a person. Only Interactive mode with a terminal on stdin
//! asks; anywhere else a prompt returns its refusal, which names the flag
//! that answers it. A declined prompt says what did not happen and exits 130.

use std::io::{self, IsTerminal as _};
use std::os::fd::{AsFd as _, BorrowedFd};

use dialoguer::{Confirm, Input, Select, console::Term};
use rustix::termios::{self, LocalModes, OptionalActions, SpecialCodeIndex, Termios};

use super::{Mode, mode};
use crate::failure::Failure;

/// Whether this run may ask: Interactive mode and a terminal to read from.
#[must_use]
pub(crate) fn can_prompt() -> bool {
    mode() == Mode::Interactive && io::stdin().is_terminal()
}

/// Ask for `name` to be typed. Anything else, an empty answer or Ctrl-C
/// declines, printing `declined`.
pub(crate) fn confirm_name(
    name: &str,
    refusal: impl FnOnce() -> Failure,
    declined: &str,
) -> Result<(), Failure> {
    if !can_prompt() {
        return Err(refusal());
    }
    let answer = ask(|| {
        Input::<String>::new()
            .with_prompt(format!("Type {name} to continue"))
            .allow_empty(true)
            .interact_text_on(&Term::stderr())
            .map(Some)
    })?;
    if answer.as_deref().map(str::trim) == Some(name) {
        Ok(())
    } else {
        Err(decline(declined))
    }
}

/// Ask a yes/no question that defaults to no.
pub(crate) fn confirm(
    question: &str,
    refusal: impl FnOnce() -> Failure,
    declined: &str,
) -> Result<(), Failure> {
    if !can_prompt() {
        return Err(refusal());
    }
    let answer = ask(|| {
        Confirm::new()
            .with_prompt(question)
            .default(false)
            .interact_on_opt(&Term::stderr())
    })?;
    if answer == Some(true) {
        Ok(())
    } else {
        Err(decline(declined))
    }
}

/// Pick one of `items`, starting on `default`. Esc or Ctrl-C declines.
pub(crate) fn select<T: ToString>(
    title: &str,
    items: &[T],
    default: usize,
    refusal: impl FnOnce() -> Failure,
    declined: &str,
) -> Result<usize, Failure> {
    if !can_prompt() {
        return Err(refusal());
    }
    ask(|| {
        Select::new()
            .with_prompt(title)
            .items(items.iter().map(ToString::to_string))
            .default(default)
            .interact_on_opt(&Term::stderr())
    })?
    .ok_or_else(|| decline(declined))
}

/// Ask for a secret without echoing it. Ctrl-C, or Ctrl-D on an empty line,
/// declines; an empty line asks again, and a paste of more than one line is refused, never cut short.
pub(crate) fn secret(
    question: &str,
    refusal: impl FnOnce() -> Failure,
    declined: &str,
) -> Result<String, Failure> {
    if !can_prompt() {
        return Err(refusal());
    }
    loop {
        match crate::cancellation::while_prompting(|| hidden_line(question))?? {
            Typed::Line(line) if line.is_empty() => {}
            Typed::Line(line) => {
                return String::from_utf8(line)
                    .map_err(|_| Failure::usage("What you typed isn't UTF-8 text."));
            }
            Typed::Lines => {
                return Err(Failure::usage(
                    "That paste held more than one line, and only one is read here; nothing was \
                     kept. Pipe a value of several lines in instead.",
                ));
            }
            Typed::Cancelled => return Err(decline(declined)),
        }
    }
}

const CTRL_C: u8 = 0x03;
const CTRL_D: u8 = 0x04;
const BACKSPACE: u8 = 0x08;
const CTRL_U: u8 = 0x15;
const CTRL_W: u8 = 0x17;
const ESC: u8 = 0x1b;
const DELETE: u8 = 0x7f;

/// How a line typed with echo off ended.
#[derive(Debug, PartialEq, Eq)]
enum Typed {
    Line(Vec<u8>),
    /// Enter came with more of a paste behind it.
    Lines,
    Cancelled,
}

/// Prompt on stderr and read one line from the terminal on stdin with echo off.
/// Signals are off too, so Ctrl-C arrives as a key rather than a SIGINT that
/// the read would restart through. Bracketed paste is on meanwhile, so a
/// paste's end is marked rather than guessed from a pause.
fn hidden_line(question: &str) -> io::Result<Typed> {
    let term = Term::stderr();
    term.write_str(&format!("{question}: "))?;
    let stdin = io::stdin();
    let typed = {
        let quiet = Quiet::on(stdin.as_fd())?;
        term.write_str("\x1b[?2004h")?;
        let typed = quiet.read_line();
        term.write_str("\x1b[?2004l")?;
        typed?
    };
    term.write_line("")?;
    Ok(typed)
}

/// The terminal with echo, line editing and signals off, restored on drop.
struct Quiet<'fd> {
    tty: BorrowedFd<'fd>,
    quiet: Termios,
    original: Termios,
}

impl<'fd> Quiet<'fd> {
    fn on(tty: BorrowedFd<'fd>) -> io::Result<Self> {
        let original = termios::tcgetattr(tty)?;
        let mut quiet = original.clone();
        quiet.local_modes -= LocalModes::ECHO | LocalModes::ICANON | LocalModes::ISIG;
        quiet.local_modes -= LocalModes::IEXTEN;
        quiet.special_codes[SpecialCodeIndex::VMIN] = 1;
        quiet.special_codes[SpecialCodeIndex::VTIME] = 0;
        termios::tcsetattr(tty, OptionalActions::Now, &quiet)?;
        Ok(Self {
            tty,
            quiet,
            original,
        })
    }

    fn read_line(&self) -> io::Result<Typed> {
        let mut typed = Keys::default();
        let mut keys = [0; 4096];
        loop {
            let read = match rustix::io::read(self.tty, &mut keys) {
                Ok(0) => return Ok(Typed::Cancelled),
                Ok(read) => read,
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            };
            match typed.feed(keys.get(..read).unwrap_or_default()) {
                None => {}
                Some(Typed::Line(line)) => {
                    return Ok(if self.settle()? {
                        Typed::Lines
                    } else {
                        Typed::Line(line)
                    });
                }
                Some(typed) => {
                    self.settle()?;
                    return Ok(typed);
                }
            }
        }
    }

    /// Read on, echo still off, until nothing arrives for 200ms, for a terminal
    /// that doesn't bracket a paste: a paste can come in pieces, and none of it
    /// may be taken as the whole value or reach the shell. Whether anything but
    /// line endings came.
    fn settle(&self) -> io::Result<bool> {
        let mut waiting = self.quiet.clone();
        waiting.special_codes[SpecialCodeIndex::VMIN] = 0;
        waiting.special_codes[SpecialCodeIndex::VTIME] = 2;
        termios::tcsetattr(self.tty, OptionalActions::Now, &waiting)?;
        let mut more = false;
        let mut keys = [0; 4096];
        loop {
            match rustix::io::read(self.tty, &mut keys) {
                Ok(0) => return Ok(more),
                Ok(read) => {
                    more |= keys
                        .get(..read)
                        .unwrap_or_default()
                        .iter()
                        .any(|key| !matches!(key, b'\r' | b'\n'));
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for Quiet<'_> {
    fn drop(&mut self) {
        // Flush, so nothing typed with echo off reaches the shell.
        let _ = termios::tcsetattr(self.tty, OptionalActions::Flush, &self.original);
    }
}

const PASTE_START: &[u8] = b"[200~";
const PASTE_END: &[u8] = b"[201~";

/// The line typed so far, and where it stands in an escape sequence or a
/// bracketed paste.
#[derive(Default)]
struct Keys {
    line: Vec<u8>,
    /// Bytes after an ESC, while they may still be a paste marker.
    escape: Option<Vec<u8>>,
    pasting: bool,
    /// A newline came inside the paste.
    pasted_end: bool,
    /// Something other than a newline came after one inside the paste.
    pasted_lines: bool,
}

impl Keys {
    /// Apply `keys`, or `None` while the line goes on. Enter ends it; newlines
    /// right after it are one pasted line's ending, anything else is a further
    /// line. A bracketed paste is read to its end marker however slowly it
    /// arrives, and ends the line if it held a newline.
    fn feed(&mut self, keys: &[u8]) -> Option<Typed> {
        let mut keys = keys.iter();
        while let Some(&key) = keys.next() {
            if let Some(escape) = &mut self.escape {
                escape.push(key);
                if escape == PASTE_START {
                    self.escape = None;
                    self.pasting = true;
                } else if escape == PASTE_END {
                    self.escape = None;
                    self.pasting = false;
                    if self.pasted_lines {
                        return Some(Typed::Lines);
                    }
                    if self.pasted_end {
                        return Some(self.end(&mut keys));
                    }
                } else if !PASTE_START.starts_with(escape) && !PASTE_END.starts_with(escape) {
                    // Another sequence, such as an arrow key: no part of the value.
                    self.escape = None;
                }
                continue;
            }
            if key == ESC {
                self.escape = Some(Vec::new());
            } else if self.pasting {
                match key {
                    b'\r' | b'\n' => self.pasted_end = true,
                    _ if self.pasted_end => self.pasted_lines = true,
                    b'\t' => self.line.push(key),
                    key if key < 0x20 => {}
                    key => self.line.push(key),
                }
            } else {
                match key {
                    CTRL_C => return Some(Typed::Cancelled),
                    CTRL_D if self.line.is_empty() => return Some(Typed::Cancelled),
                    // Ctrl-D after some text ends the line, as a terminal's own reading does.
                    b'\r' | b'\n' | CTRL_D => return Some(self.end(&mut keys)),
                    BACKSPACE | DELETE => {
                        // Drop one character: its UTF-8 continuation bytes, then its lead.
                        while self.line.pop().is_some_and(|byte| byte & 0xc0 == 0x80) {}
                    }
                    CTRL_U => self.line.clear(),
                    CTRL_W => {
                        // Drop the word before the cursor and the blanks after it.
                        while self.line.last().is_some_and(u8::is_ascii_whitespace) {
                            self.line.pop();
                        }
                        while self
                            .line
                            .last()
                            .is_some_and(|byte| !byte.is_ascii_whitespace())
                        {
                            self.line.pop();
                        }
                    }
                    b'\t' => self.line.push(key),
                    key if key < 0x20 => {}
                    key => self.line.push(key),
                }
            }
        }
        None
    }

    fn end<'k>(&mut self, rest: &mut impl Iterator<Item = &'k u8>) -> Typed {
        if rest.all(|key| matches!(key, b'\r' | b'\n')) {
            Typed::Line(std::mem::take(&mut self.line))
        } else {
            Typed::Lines
        }
    }
}

/// Ctrl-C reaches the prompt as `Interrupted`: that is a no, and the cursor a
/// select hid comes back.
fn ask<T>(prompt: impl FnOnce() -> dialoguer::Result<Option<T>>) -> Result<Option<T>, Failure> {
    match crate::cancellation::while_prompting(prompt)? {
        Ok(answer) => Ok(answer),
        Err(dialoguer::Error::IO(error)) => {
            let _ = Term::stderr().show_cursor();
            if error.kind() == io::ErrorKind::Interrupted {
                let _ = Term::stderr().write_line("");
                Ok(None)
            } else {
                Err(Failure::from(error))
            }
        }
    }
}

fn decline(declined: &str) -> Failure {
    super::note(declined);
    Failure::exit(super::exit::CANCELLED_EXIT).interrupted()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::RpcErrorCode;

    fn refusal() -> Failure {
        Failure::coded(RpcErrorCode::ConfirmationRequired, "needs a terminal")
    }

    #[test]
    fn a_hidden_line_ends_at_enter_and_never_keeps_part_of_a_paste() {
        let typed = |keys: &[&[u8]]| {
            let mut typed = Keys::default();
            keys.iter().find_map(|keys| typed.feed(keys))
        };
        let line = |text: &str| Some(Typed::Line(text.as_bytes().to_vec()));
        assert_eq!(typed(&[b"dummy-", b"value\r"]), line("dummy-value"));
        assert_eq!(typed(&[b"dummy\r\n"]), line("dummy"));
        assert_eq!(typed(&[b"dummx\x7fy\n"]), line("dummy"));
        assert_eq!(typed(&["dummy\u{e9}\x7f\n".as_bytes()]), line("dummy"));
        assert_eq!(typed(&[b"wrong\x15dummy\n"]), line("dummy"));
        assert_eq!(typed(&[b"dummy wrong \x17\n"]), line("dummy "));
        assert_eq!(typed(&[b"dum\x1b[Dmy\n"]), line("dummy"));
        assert_eq!(typed(&[b"\n"]), line(""));
        assert_eq!(typed(&[b"dummy"]), None);
        assert_eq!(typed(&[b"-----BEGIN-----\nabc\n"]), Some(Typed::Lines));
        assert_eq!(typed(&[b"dum\x03"]), Some(Typed::Cancelled));
        assert_eq!(typed(&[b"\x04"]), Some(Typed::Cancelled));
        assert_eq!(typed(&[b"dummy\x04"]), line("dummy"));

        // A bracketed paste is read to its end marker, however it is split.
        assert_eq!(typed(&[b"\x1b[200~dummy\x1b[201~", b"\r"]), line("dummy"));
        assert_eq!(typed(&[b"\x1b[200~dummy\n\x1b[201~"]), line("dummy"));
        assert_eq!(
            typed(&[b"\x1b[2", b"00~line1\n", b"line2", b"\n\x1b[20", b"1~"]),
            Some(Typed::Lines)
        );
        assert_eq!(typed(&[b"\x1b[200~line1\n", b"line2"]), None);
    }

    #[test]
    fn every_prompt_refuses_without_an_interactive_terminal() {
        // Tests run in Plain mode, the default before dispatch.
        let refused = |result: Result<(), Failure>| {
            let error = result.unwrap_err();
            assert_eq!(error.report().code, RpcErrorCode::ConfirmationRequired);
            assert_eq!(crate::ui::exit_code(&error.report().code), 2);
        };
        refused(confirm_name("blog", refusal, "Cancelled."));
        refused(confirm("Reset?", refusal, "Cancelled."));
        refused(select("Pick", &["a", "b"], 0, refusal, "Cancelled.").map(drop));
        refused(secret("Token", refusal, "Cancelled.").map(drop));
    }
}
