//! What the Server commands print about a Server: its JSON, the command that takes out one
//! that never joined, and a committed step whose Ingress Proxy did not follow.

use ployz_core::{Hint, Machine, MachineId};
use serde_json::{Value, json};

use crate::failure::Failure as Error;

/// The command that takes out a Server that never joined. By ID, since a rerun of its add
/// can list a second Server under the same name.
pub(crate) fn forget_unarrived(id: MachineId) -> String {
    shell_words::join([
        "ployz",
        "server",
        "forget",
        id.as_str(),
        "--confirm",
        "dead",
    ])
}

/// A Machine as every `server` command prints it: its WireGuard public key in base64,
/// as `wg` prints it, and without its Sealing state.
pub(crate) fn machine_json(machine: &Machine) -> Value {
    let mut value = serde_json::to_value(machine).expect("a Machine serializes");
    if let Value::Object(fields) = &mut value {
        fields.insert("public_key".into(), json!(machine.public_key.to_string()));
        if let Some(Value::Object(runtime)) = fields.get_mut("runtime") {
            runtime.remove("sealing");
        }
    }
    value
}

/// A Server as `server add`, `init`, `set` and `rm` print it: the `server ls` and
/// `server show` shape, its Machine under `machine`.
pub(crate) fn server_json(machine: &Machine) -> Value {
    json!({ "machine": machine_json(machine) })
}

/// The headline for a committed step whose Ingress Proxy deploy failed. A failed deploy's
/// own sentence names what its Container did, so it stays on the headline; the one
/// `cause:` line is then the Container's last line.
pub(crate) fn ingress_headline(stem: &str, error: &Error) -> String {
    match error.diagnostic() {
        Some(_) => format!("{stem}: {error}"),
        None => format!("{stem}."),
    }
}

/// Direct ingress diagnostics acquire their command scope before committed output is serialized.
/// Each Server is selected by Machine ID, since a Machine Name may be ambiguous.
pub(crate) fn ingress_hints(error: Error, command: impl Fn(&[&str]) -> String) -> Error {
    let retry = error
        .diagnostic()
        .and_then(|diagnostic| diagnostic.failed_machine)
        .map(|machine| {
            Hint::Retry(command(&[
                "server",
                "set",
                &machine.to_string(),
                "--accepts-ingress=true",
            ]))
        });
    ingress_log_hints(error, command, ToString::to_string).hint(retry)
}

/// [`ingress_hints`] without the retry, for a failure that has the reader do something else
/// first. `server` selects each Server in the hints.
pub(crate) fn ingress_log_hints(
    mut error: Error,
    command: impl Fn(&[&str]) -> String,
    server: impl Fn(&MachineId) -> String,
) -> Error {
    let Some(diagnostic) = error.diagnostic() else {
        return error;
    };
    let hints = diagnostic
        .logs
        .iter()
        .map(|tail| {
            Hint::Inspect(command(&[
                "logs",
                &tail.service.to_string(),
                "--machine",
                &server(&tail.machine),
            ]))
        })
        .collect::<Vec<_>>();
    for hint in hints {
        error = error.hint(hint);
    }
    error
}

/// A committed Server change whose Ingress Proxy follow-up failed: name it, and `then`, the step
/// that finishes it.
pub(crate) fn ingress_incomplete(committed: &str, error: Error, then: Hint) -> Error {
    let headline = ingress_headline(
        &format!("{committed}; the Ingress Proxy did not follow"),
        &error,
    );
    error.context(headline).hint(then)
}
