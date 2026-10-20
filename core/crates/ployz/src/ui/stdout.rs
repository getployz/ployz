//! The one way to stdout. SIGPIPE stays ignored, Rust's default, because hyper
//! `writev`s to the daemon socket and a hang-up there must be an error, not a
//! silent death. Only a reader gone from stdout ends the process by SIGPIPE.
//! Any other write error is kept, and the command fails with it at exit.

use std::{
    cell::RefCell,
    io::{self, IsTerminal, Write},
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::AsyncWrite;
use tokio_util::sync::CancellationToken;

thread_local! {
    /// The first stdout write error other than a gone reader. Results are
    /// printed on the handler's thread, as `exit` runs.
    static BROKEN: RefCell<Option<io::Error>> = const { RefCell::new(None) };
}

/// A stdout writer that dies of SIGPIPE when the reader is gone.
pub(crate) struct Stdout<W>(pub(super) W);

/// stdout through `anstream`, which strips color `--color` turns off.
#[expect(clippy::disallowed_methods, reason = "this is the stdout accessor")]
pub(crate) fn stdout() -> Stdout<anstream::Stdout> {
    Stdout(anstream::stdout())
}

/// stdout locked, for bytes written as they are.
#[expect(clippy::disallowed_methods, reason = "this is the stdout accessor")]
pub(crate) fn stdout_bytes() -> Stdout<io::StdoutLock<'static>> {
    Stdout(io::stdout().lock())
}

#[expect(clippy::disallowed_methods, reason = "this is the stdout accessor")]
pub(crate) fn stdout_is_terminal() -> bool {
    io::stdout().is_terminal()
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

/// Async stdout for a server that must stop its work before it exits:
/// `closed` is cancelled when the reader is gone, and the process lives on.
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
