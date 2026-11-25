//! The one way to stdout and stderr. Everything Ployz prints for a person, text or a
//! `--json` document, passes [`userinfo::mask`] here, so no URL's user info reaches
//! either. Only the relays pass bytes on unmasked: what another program or a file wrote,
//! byte for byte. Each relay is a `disallowed-methods` entry that its caller names a
//! reason for.
//!
//! SIGPIPE stays ignored, Rust's default, because hyper `writev`s to the daemon socket
//! and a hang-up there must be an error, not a silent death. Only a reader gone from
//! stdout ends the process by SIGPIPE. Any other write error is kept, and the command
//! fails with it at exit.

use std::{
    cell::RefCell,
    io::{self, IsTerminal, Write},
    pin::Pin,
    task::{Context, Poll},
};

use serde::Serialize;
use tokio::io::AsyncWrite;
use tokio_util::sync::CancellationToken;

use super::userinfo;

thread_local! {
    /// The first stdout write error other than a gone reader. Results are
    /// printed on the handler's thread, as `exit` runs.
    static BROKEN: RefCell<Option<io::Error>> = const { RefCell::new(None) };
}

/// A stdout writer that dies of SIGPIPE when the reader is gone.
pub(crate) struct Stdout<W>(pub(super) W);

/// Text for a person, each line masked before it is passed on. A line held at a flush
/// goes out masked as it stands.
pub(crate) struct Masked<W: Write> {
    inner: W,
    line: Vec<u8>,
}

impl<W: Write> Masked<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            line: Vec::new(),
        }
    }

    fn pass(&mut self, through: usize) -> io::Result<()> {
        let rest = self.line.split_off(through);
        let lines = std::mem::replace(&mut self.line, rest);
        self.inner.write_all(&userinfo::mask_bytes(&lines))
    }
}

impl<W: Write> Write for Masked<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.line.extend_from_slice(buf);
        if let Some(end) = self.line.iter().rposition(|&byte| byte == b'\n') {
            self.pass(end + 1)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.pass(self.line.len())?;
        self.inner.flush()
    }
}

impl<W: Write> Drop for Masked<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// stdout through `anstream`, which strips color `--color` turns off.
#[expect(clippy::disallowed_methods, reason = "this is the stdout accessor")]
pub(crate) fn stdout() -> Masked<Stdout<anstream::Stdout>> {
    Masked::new(Stdout(anstream::stdout()))
}

/// stdout for lines that keep their own color codes, such as a container's log.
#[expect(clippy::disallowed_methods, reason = "this is the stdout accessor")]
pub(crate) fn stdout_bytes() -> Masked<Stdout<io::StdoutLock<'static>>> {
    Masked::new(Stdout(io::stdout().lock()))
}

/// stderr through `anstream`.
#[expect(clippy::disallowed_methods, reason = "this is the stderr accessor")]
pub(crate) fn stderr() -> Masked<anstream::Stderr> {
    Masked::new(anstream::stderr())
}

/// stderr for lines that keep their own color codes, such as a container's log.
#[expect(clippy::disallowed_methods, reason = "this is the stderr accessor")]
pub(crate) fn stderr_bytes() -> Masked<io::Stderr> {
    Masked::new(io::stderr())
}

/// stderr colored when `color`, whatever `--color` chose, for a thread that decided
/// before it started.
#[expect(clippy::disallowed_methods, reason = "this is the stderr accessor")]
pub(super) fn stderr_painted(color: bool) -> Masked<anstream::AutoStream<io::Stderr>> {
    let choice = match color {
        true => anstream::ColorChoice::Always,
        false => anstream::ColorChoice::Never,
    };
    Masked::new(anstream::AutoStream::new(io::stderr(), choice))
}

/// Whether stderr takes color as `--color` and the terminal decide.
#[expect(clippy::disallowed_methods, reason = "this is the stderr accessor")]
pub(super) fn stderr_colors() -> bool {
    anstream::AutoStream::choice(&io::stderr()) != anstream::ColorChoice::Never
}

/// The terminal on stderr, for the prompt and live-progress libraries that draw on it.
/// What they draw is masked before it is handed over: prompts mask their text, and the
/// live rows mask each line.
#[expect(clippy::disallowed_methods, reason = "this is the stderr accessor")]
pub(super) fn terminal() -> console::Term {
    console::Term::stderr()
}

/// [`terminal`], buffered until a flush, for drawing a frame at once.
#[expect(clippy::disallowed_methods, reason = "this is the stderr accessor")]
pub(super) fn terminal_buffered() -> console::Term {
    console::Term::buffered_stderr()
}

#[expect(clippy::disallowed_methods, reason = "this is the stdout accessor")]
pub(crate) fn stdout_is_terminal() -> bool {
    io::stdout().is_terminal()
}

#[expect(clippy::disallowed_methods, reason = "this is the stderr accessor")]
pub(crate) fn stderr_is_terminal() -> bool {
    io::stderr().is_terminal()
}

/// Print a panic through [`stderr`], so its message, which may repeat a value typed, is
/// masked too. `RUST_BACKTRACE` still adds the backtrace.
pub fn mask_panics() {
    std::panic::set_hook(Box::new(|panic| {
        let backtrace = std::backtrace::Backtrace::capture();
        let thread = std::thread::current();
        let mut stderr = stderr();
        let _ = writeln!(
            stderr,
            "thread '{}' {panic}",
            thread.name().unwrap_or("<unnamed>")
        );
        let _ = if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            writeln!(stderr, "stack backtrace:\n{backtrace}")
        } else {
            writeln!(
                stderr,
                "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace"
            )
        };
    }));
}

/// Send what stdout holds: a result's last line may still sit in std's buffer, and std
/// drops a flush error at exit.
#[expect(clippy::disallowed_methods, reason = "this is the stdout accessor")]
pub(super) fn flush_stdout() {
    let _ = Stdout(io::stdout().lock()).flush();
}

/// Where a JSON document goes: each string in it, keys too, is masked before it is
/// written, never the text it serializes to.
pub(crate) struct Json<W>(W);

impl Json<Stdout<io::StdoutLock<'static>>> {
    #[expect(clippy::disallowed_methods, reason = "this is the stdout accessor")]
    pub(crate) fn stdout() -> Self {
        Self(Stdout(io::stdout().lock()))
    }
}

impl<W: Write> Json<W> {
    pub(super) fn new(out: W) -> Self {
        Self(out)
    }

    #[cfg(test)]
    pub(super) fn written(&self) -> &W {
        &self.0
    }

    /// `value` on one line, or `pretty` over several, then a newline, flushed.
    ///
    /// # Errors
    ///
    /// Returns a serialization or write error.
    pub(crate) fn write<T: Serialize + ?Sized>(
        &mut self,
        value: &T,
        pretty: bool,
    ) -> serde_json::Result<()> {
        let mut value = serde_json::to_value(value)?;
        userinfo::mask_json(&mut value);
        match pretty {
            true => serde_json::to_writer_pretty(&mut self.0, &value)?,
            false => serde_json::to_writer(&mut self.0, &value)?,
        }
        self.0.write_all(b"\n").map_err(serde_json::Error::io)?;
        self.0.flush().map_err(serde_json::Error::io)
    }
}

/// stdout for bytes another program or a file wrote, passed on unmasked: what `exec`
/// relays from its container, or a file `github cat` prints.
#[expect(clippy::disallowed_methods, reason = "this is the stdout accessor")]
pub(crate) fn relayed_stdout() -> Stdout<io::StdoutLock<'static>> {
    Stdout(io::stdout().lock())
}

/// stderr for bytes another program wrote, passed on unmasked: what `exec` relays from
/// its container, or the `::add-mask::` lines a GitHub runner reads, which must name a
/// secret exactly for the runner to hide it.
#[expect(clippy::disallowed_methods, reason = "this is the stderr accessor")]
pub(crate) fn relayed_stderr() -> io::Stderr {
    io::stderr()
}

/// stderr handed to a child program, which writes on it itself, unmasked.
#[expect(clippy::disallowed_methods, reason = "this is the stderr accessor")]
pub(crate) fn child_stderr() -> std::process::Stdio {
    io::stderr().into()
}

impl<W: Write> Write for Stdout<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        unless_reader_gone(self.0.write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        unless_reader_gone(self.0.flush())
    }
}

fn unless_reader_gone<T>(result: io::Result<T>) -> io::Result<T> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => die_of_sigpipe(),
        Err(error) if error.kind() != io::ErrorKind::Interrupted => {
            BROKEN.with_borrow_mut(|broken| {
                broken.get_or_insert_with(|| copy(&error));
            });
            Err(error)
        }
        result => result,
    }
}

fn copy(error: &io::Error) -> io::Error {
    error.raw_os_error().map_or_else(
        || io::Error::new(error.kind(), error.to_string()),
        io::Error::from_raw_os_error,
    )
}

/// The first stdout write error, once; a result that never reached its reader.
pub(super) fn take_broken() -> Option<io::Error> {
    BROKEN.take()
}

fn die_of_sigpipe() -> ! {
    sigpipe::reset();
    let _ = rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::PIPE);
    // Unreachable once the signal lands; the shell's 128 + SIGPIPE otherwise.
    std::process::exit(128 + 13)
}

/// Async stdout for the MCP server's JSON-RPC stream to its client, unmasked: it must
/// stop its work before it exits, so `closed` is cancelled when the reader is gone, and
/// the process lives on.
pub(crate) struct AsyncStdout {
    stdout: tokio::io::Stdout,
    closed: CancellationToken,
}

#[expect(clippy::disallowed_methods, reason = "this is the stdout accessor")]
pub(crate) fn async_stdout(closed: CancellationToken) -> AsyncStdout {
    AsyncStdout {
        stdout: tokio::io::stdout(),
        closed,
    }
}

impl AsyncStdout {
    fn noticing<T>(&self, poll: Poll<io::Result<T>>) -> Poll<io::Result<T>> {
        if let Poll::Ready(Err(error)) = &poll
            && error.kind() == io::ErrorKind::BrokenPipe
        {
            self.closed.cancel();
        }
        poll
    }
}

impl AsyncWrite for AsyncStdout {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let poll = Pin::new(&mut self.stdout).poll_write(cx, buf);
        self.noticing(poll)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let poll = Pin::new(&mut self.stdout).poll_flush(cx);
        self.noticing(poll)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let poll = Pin::new(&mut self.stdout).poll_shutdown(cx);
        self.noticing(poll)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::{Json, Masked};

    /// A URL split over several writes is masked once its line is whole.
    #[test]
    fn a_line_written_in_pieces_is_masked_whole() {
        let mut out = Vec::new();
        {
            let mut masked = Masked::new(&mut out);
            write!(masked, "a https://user:").unwrap();
            write!(masked, "SENTINEL@host b\nnext ssh://SENTINEL").unwrap();
            write!(masked, "@host").unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "a https://<USER>:<PASSWORD>@host b\nnext ssh://<USER>@host"
        );
    }

    #[test]
    fn a_json_document_is_masked_value_by_value() {
        let mut json = Json::new(Vec::new());
        json.write(
            &serde_json::json!({"a": "https://host:8443", "b": "x@y", "c": "ssh://SENTINEL@h"}),
            false,
        )
        .unwrap();
        let written: serde_json::Value = serde_json::from_slice(json.written()).unwrap();
        assert_eq!(
            written,
            serde_json::json!({"a": "https://host:8443", "b": "x@y", "c": "ssh://<USER>@h"})
        );
    }
}
