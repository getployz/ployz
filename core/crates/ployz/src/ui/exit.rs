//! How a command ends: the error it prints and the exit code, decided here alone.

use std::{io, process::ExitCode};

use ployz_core::RpcErrorCode;
use serde_json::json;

use super::{Mode, error};
use crate::failure::Failure;

/// A result printed, but some Servers failed or did not answer.
pub const PARTIAL_EXIT: u8 = 3;

/// It ran and failed.
const FAILED_EXIT: u8 = 1;

/// The command line is wrong.
const USAGE_EXIT: u8 = 2;

/// The exit code for an error code: 2 when the command line is wrong, else 1.
#[must_use]
pub fn exit_code(code: &RpcErrorCode) -> u8 {
    match code {
        RpcErrorCode::InvalidArgument
        | RpcErrorCode::ConfirmationRequired
        | RpcErrorCode::Ambiguous => USAGE_EXIT,
        RpcErrorCode::Unsupported
        | RpcErrorCode::NotFound
        | RpcErrorCode::Conflict
        | RpcErrorCode::Unauthenticated
        | RpcErrorCode::Unavailable
        | RpcErrorCode::Internal
        | RpcErrorCode::Unknown(_) => FAILED_EXIT,
    }
}

/// End the process: print the failure, if any, and return its exit code.
#[must_use]
pub fn exit(result: Result<(), Failure>) -> ExitCode {
    let Err(failure) = result else {
        return ExitCode::SUCCESS;
    };
    let code = render(
        &failure,
        super::mode(),
        crate::output::emitted(),
        &mut io::stdout().lock(),
        &mut anstream::stderr().lock(),
    );
    ExitCode::from(code)
}

/// Print `failure` for `mode` and return its exit code. Once a result is
/// printed, stdout keeps that one object and the failure turns it partial.
fn render(
    failure: &Failure,
    mode: Mode,
    emitted: bool,
    stdout: &mut dyn io::Write,
    stderr: &mut dyn io::Write,
) -> u8 {
    if let Some(code) = failure.printed_exit() {
        return code;
    }
    let report = failure.report();
    // Nothing is left to report a failed write to.
    if mode == Mode::Json && !emitted {
        let _ = writeln!(stdout, "{}", json!({ "error": failure.json() }));
    } else {
        let _ = error::write(stderr, &report.message, &failure.causes(), &failure.hints());
    }
    if emitted {
        PARTIAL_EXIT
    } else {
        exit_code(&report.code)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use ployz_core::RpcError;
    use serde_json::Value;

    use super::*;
    use crate::ui::Hint;

    struct Ended {
        code: u8,
        stdout: String,
        stderr: String,
    }

    fn end(failure: &Failure, mode: Mode, emitted: bool) -> Ended {
        let mut stdout = Vec::new();
        let mut stderr = anstream::StripStream::new(Vec::new());
        let code = render(failure, mode, emitted, &mut stdout, &mut stderr);
        Ended {
            code,
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr.into_inner()).unwrap(),
        }
    }

    #[derive(Debug)]
    struct Os;

    impl fmt::Display for Os {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("connection refused (os error 111)")
        }
    }

    impl std::error::Error for Os {}

    #[derive(Debug)]
    struct Tcp(Os);

    impl fmt::Display for Tcp {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("tcp connect error")
        }
    }

    impl std::error::Error for Tcp {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn every_error_code_has_its_exit_code() {
        for (code, exit) in [
            (RpcErrorCode::InvalidArgument, 2),
            (RpcErrorCode::ConfirmationRequired, 2),
            (RpcErrorCode::Ambiguous, 2),
            (RpcErrorCode::Unsupported, 1),
            (RpcErrorCode::NotFound, 1),
            (RpcErrorCode::Conflict, 1),
            (RpcErrorCode::Unauthenticated, 1),
            (RpcErrorCode::Unavailable, 1),
            (RpcErrorCode::Internal, 1),
            (RpcErrorCode::Unknown("teapot".into()), 1),
        ] {
            let ended = end(&Failure::coded(code.clone(), "Nope."), Mode::Plain, false);
            assert_eq!(ended.code, exit, "{code}");
            assert_eq!(ended.stderr, "error: Nope.\n", "{code}");
            assert_eq!(ended.stdout, "", "{code}");
        }
    }

    #[test]
    fn a_two_level_chain_prints_two_cause_lines() {
        let failure = Failure::caused(
            RpcErrorCode::Unavailable,
            "Could not reach Cloud at http://127.0.0.1:9.",
            Tcp(Os),
        )
        .hint(Hint::Next("ployz cloud status".into()));
        let ended = end(&failure, Mode::Interactive, false);
        assert_eq!(ended.code, 1);
        assert_eq!(
            ended.stderr,
            "error: Could not reach Cloud at http://127.0.0.1:9.\n  cause: tcp connect error\n  cause: connection refused (os error 111)\nnext: ployz cloud status\n"
        );
    }

    #[test]
    fn json_prints_one_error_object_on_stdout_and_nothing_on_stderr() {
        let failure = Failure::caused(RpcErrorCode::Unavailable, "Could not reach Cloud.", Os)
            .hint(Hint::Inspect("ployz cloud status".into()));
        let ended = end(&failure, Mode::Json, false);
        assert_eq!(ended.code, 1);
        assert_eq!(ended.stderr, "");
        let object: Value = serde_json::from_str(&ended.stdout).unwrap();
        assert_eq!(
            object,
            json!({ "error": {
                "code": "unavailable",
                "message": "Could not reach Cloud.",
                "cause": ["connection refused (os error 111)"],
                "details": { "inspect": ["ployz cloud status"] },
            }})
        );
    }

    #[test]
    fn hints_from_the_wire_print_after_our_own() {
        let failure = Failure::from(RpcError {
            code: RpcErrorCode::NotFound,
            message: "No Service nope in production.".into(),
            details: json!({ "valid_children": ["web", "db"] }),
        });
        let ended = end(&failure, Mode::Plain, false);
        assert_eq!(ended.code, 1);
        assert_eq!(
            ended.stderr,
            "error: No Service nope in production.\nvalid: web, db\n"
        );
    }

    #[test]
    fn a_wrapper_that_repeats_its_source_prints_once() {
        let inner = Failure::caused(RpcErrorCode::Internal, "Could not save the context.", Os);
        let failure = inner.context("Server removed; local context cleanup failed.");
        let ended = end(&failure, Mode::Plain, false);
        assert_eq!(
            ended.stderr,
            "error: Server removed; local context cleanup failed.\n  cause: Could not save the context.\n  cause: connection refused (os error 111)\n"
        );
    }

    #[test]
    fn a_failure_after_a_printed_result_is_partial_and_keeps_stdout() {
        let ended = end(&Failure::usage("Nope."), Mode::Json, true);
        assert_eq!(ended.code, PARTIAL_EXIT);
        assert_eq!(ended.stdout, "");
        assert_eq!(ended.stderr, "error: Nope.\n");
    }

    #[test]
    fn printed_exits_print_nothing() {
        for (failure, code) in [(Failure::partial(), 3), (Failure::exit(7), 7)] {
            let ended = end(&failure, Mode::Plain, false);
            assert_eq!(ended.code, code);
            assert_eq!(ended.stdout, "");
            assert_eq!(ended.stderr, "");
        }
        assert_eq!(exit(Ok(())), ExitCode::SUCCESS);
    }

    #[test]
    fn color_is_painted_and_stripped_by_the_stream() {
        let mut colored = Vec::new();
        error::write(&mut colored, "Nope.", &["boom".into()], &[]).unwrap();
        let colored = String::from_utf8(colored).unwrap();
        assert!(colored.contains("\u{1b}["), "{colored:?}");
        let mut plain = anstream::StripStream::new(Vec::new());
        error::write(&mut plain, "Nope.", &["boom".into()], &[]).unwrap();
        assert_eq!(
            String::from_utf8(plain.into_inner()).unwrap(),
            "error: Nope.\n  cause: boom\n"
        );
    }
}
