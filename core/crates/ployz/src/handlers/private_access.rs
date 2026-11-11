//! `ployz private-access`: which Servers run the Tailscale router that gives a tailnet
//! the Cluster's private network, and `on` and `off` to run it everywhere or nowhere.

use std::io::Read;

use clap::{ArgMatches, Command};
use ployz_core::{MachineFailure, PartialResult, RpcError, RpcErrorCode, authority::Signer};
use tokio_util::sync::CancellationToken;

use super::{Error, leaf_matches, with_client};
use crate::cli::{base, switch};
use crate::private_access::{
    AccessChanged, AccessMachine, MachineAccess, device_approval, key_refused, observe_access,
    seal_key, shown, turn_off, turn_on,
};
use crate::ui::{self, Cell, Gaps, Hint, Table};

/// The most a Tailscale auth key holds on stdin. Theirs run about a hundred bytes.
const KEY_LIMIT: usize = 4096;

pub(crate) fn show_command(command: Command) -> Command {
    base(command, "Show which Servers run the Private Access router")
        .long_about("Show which Servers run the Private Access router.")
        .after_help(concat!(
            "Examples:\n",
            "  ployz private-access\n",
            "  ployz private-access --json"
        ))
}

pub(crate) fn on_command(command: Command) -> Command {
    base(
        command,
        "Reach the Cluster's private network from your tailnet",
    )
    .long_about(
        "Run a Tailscale router on every Server so devices on your tailnet reach Services \
         at their private addresses. The auth key is sealed to the Cluster. Approve the \
         route in the Tailscale admin console once. On Linux, run `tailscale up \
         --accept-routes` on each device.",
    )
    .after_help(concat!(
        "Examples:\n",
        "  ployz private-access on\n",
        "  ployz private-access on --auth-key-stdin < auth-key.txt"
    ))
    .arg(
        switch("auth-key-stdin", None)
            .help("Read the Tailscale auth key from stdin instead of asking for it"),
    )
}

pub(crate) fn off_command(command: Command) -> Command {
    base(
        command,
        "Remove the Private Access router and its auth key from every Server",
    )
    .long_about("Remove the Tailscale router and its auth key from every Server.")
    .after_help(concat!(
        "Examples:\n",
        "  ployz private-access off\n",
        "  ployz private-access off --json"
    ))
}

pub(crate) fn show(root: &ArgMatches) -> Result<(), Error> {
    with_client(root, |client| {
        Box::pin(async move {
            let observed = observe_access(client).await?;
            let rows = shown(&observed);
            let listing: PartialResult<(), RpcError> = PartialResult {
                successes: Vec::new(),
                failures: observed.listed.failures.clone(),
                omissions: observed.listed.omissions.clone(),
            };
            let gaps = Gaps::of(&listing).named(
                observed
                    .machines
                    .iter()
                    .map(|machine| (machine.id, &machine.name)),
            );
            let table = table(&rows);
            ui::finish_fanout("machines", &rows, &gaps, || {
                if rows.iter().any(|row| row.router) {
                    ui::rows(&table);
                } else if gaps.is_complete() {
                    ui::note("No Server runs the Private Access router.");
                    ui::hint(&Hint::Next("ployz private-access on".into()));
                }
            })
        })
    })
}

pub(crate) fn on(root: &ArgMatches) -> Result<(), Error> {
    let key = if leaf_matches(root).get_flag("auth-key-stdin") {
        read_key(std::io::stdin())?
    } else {
        ask_key()?
    };
    let config = super::config_path(root)?;
    let device_key = super::device::key::path(&config);
    with_client(root, |client| {
        Box::pin(async move {
            let seal_to = super::device::seal_to(&config, client).await?;
            let observed = observe_access(client).await?;
            let cluster = seal_to.key(client, observed.sealing).await?;
            let sealed = seal_key(&cluster, &key)?;
            let approval = match super::device::key::load(&device_key) {
                Ok(Some(secret)) => Some(device_approval(
                    &Signer::from_seed(&secret),
                    cluster,
                    &sealed,
                )?),
                _ => None,
            };
            let addressed = observed.machines.clone();
            let changed = turn_on(
                &*client,
                observed.machines,
                sealed,
                approval,
                &CancellationToken::new(),
            )
            .await;
            let AccessChanged { machines, failures } = changed;
            let refused = failures.iter().any(|failure| key_refused(&failure.error));
            let any_ran = machines.iter().any(|machine| machine.router);
            let gaps = gaps_of(&addressed, failures);
            let table = table(&machines);
            let outcome = ui::finish_fanout("machines", &machines, &gaps, || {
                if machines.is_empty() && gaps.is_complete() {
                    ui::note("No Server answered.");
                    return;
                }
                ui::rows(&table);
                if any_ran {
                    ui::note(
                        "Approve the route under ployz-router in the Tailscale admin console. \
                         On Linux, run `tailscale up --accept-routes` on each device.",
                    );
                }
            });
            if refused {
                ui::hint(&Hint::Retry(
                    "ployz private-access on --auth-key-stdin".to_owned(),
                ));
            }
            outcome
        })
    })
}

pub(crate) fn off(root: &ArgMatches) -> Result<(), Error> {
    with_client(root, |client| {
        Box::pin(async move {
            let observed = observe_access(client).await?;
            let addressed = observed.machines.clone();
            let changed = turn_off(&*client, observed.machines, &CancellationToken::new()).await;
            let gaps = gaps_of(&addressed, changed.failures);
            let table = table(&changed.machines);
            ui::finish_fanout("machines", &changed.machines, &gaps, || {
                if changed.machines.is_empty() && gaps.is_complete() {
                    ui::note("No Server answered.");
                } else {
                    ui::rows(&table);
                }
            })
        })
    })
}

fn table(rows: &[MachineAccess]) -> Table {
    let mut table = Table::new(["SERVER", "ROUTER"], "No servers answered.");
    for row in rows {
        let router = if row.router {
            Cell::status("running", ui::Tone::Good)
        } else {
            Cell::status("off", ui::Tone::Muted)
        };
        table.row([Cell::from(&row.server), router]);
    }
    table
}

/// The gaps in a change, naming every Machine the command addressed: one whose answer
/// was a failure has no row, and its warning still carries its name.
fn gaps_of(addressed: &[AccessMachine], failures: Vec<MachineFailure<RpcError>>) -> Gaps {
    let listing: PartialResult<(), RpcError> = PartialResult {
        successes: Vec::new(),
        failures,
        omissions: Vec::new(),
    };
    Gaps::of(&listing).named(addressed.iter().map(|machine| (machine.id, &machine.name)))
}

fn read_key(stdin: impl Read) -> Result<String, Error> {
    let bytes = super::read_bounded(stdin, KEY_LIMIT)
        .map_err(|error| {
            Error::caused(
                RpcErrorCode::InvalidArgument,
                "Could not read the Tailscale auth key from stdin",
                error,
            )
        })?
        .ok_or_else(|| {
            Error::usage(format!(
                "The key on stdin is over {KEY_LIMIT} bytes. Auth keys run about 100 characters."
            ))
        })?;
    let key =
        String::from_utf8(bytes).map_err(|_| Error::usage("The auth key on stdin isn't UTF-8."))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(Error::usage(
            "stdin holds no auth key. Create one in the Tailscale admin console.",
        ));
    }
    Ok(key.to_owned())
}

fn ask_key() -> Result<String, Error> {
    let key = ui::secret(
        "Tailscale auth key",
        || {
            Error::coded(
                RpcErrorCode::InvalidArgument,
                "No terminal to ask for the auth key. Pipe it with --auth-key-stdin.",
            )
        },
        "Cancelled. Private Access stays as it is.",
    )?;
    let key = key.trim();
    if key.is_empty() {
        return Err(Error::usage(
            "The auth key is empty. Create one in the Tailscale admin console.",
        ));
    }
    Ok(key.to_owned())
}

#[cfg(test)]
mod tests {
    use ployz_core::{MachineId, MachineName};

    use super::*;

    #[test]
    fn a_machine_whose_off_failed_is_named_in_its_warning() {
        let id = MachineId::parse("a".repeat(32)).unwrap();
        let machines = vec![AccessMachine {
            id,
            name: MachineName::parse("machine-1").unwrap(),
            supported: true,
        }];
        let failures = vec![MachineFailure {
            machine_id: id,
            error: RpcError {
                code: RpcErrorCode::Internal,
                message:
                    "Could not remove Private Access router abc, so this Server is not off yet."
                        .to_owned(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            },
        }];
        let gaps = gaps_of(&machines, failures);
        assert!(!gaps.is_complete());
        assert!(
            gaps.stopped_answering(&id).starts_with("machine-1 "),
            "{}",
            gaps.stopped_answering(&id)
        );
    }

    #[test]
    fn a_key_on_stdin_is_trimmed_and_an_empty_or_oversized_one_refused() {
        assert_eq!(
            read_key("  tskey-auth-abc\n".as_bytes()).unwrap(),
            "tskey-auth-abc"
        );
        let empty = read_key("\n".as_bytes()).unwrap_err().report();
        assert_eq!(empty.code, RpcErrorCode::InvalidArgument);
        assert!(empty.message.contains("no auth key"), "{}", empty.message);
        let long = vec![b'a'; KEY_LIMIT + 1];
        let oversized = read_key(long.as_slice()).unwrap_err().report();
        assert!(
            oversized.message.contains("over 4096 bytes"),
            "{}",
            oversized.message
        );
    }
}
