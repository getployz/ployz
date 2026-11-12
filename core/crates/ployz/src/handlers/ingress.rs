//! `ployz ingress`: which Doors each Server with the ingress role holds open for web
//! traffic, and `ployz ingress use` to open the Public Door or the Tunnel Door instead.

use std::io::Read;

use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{ArgMatches, Command};
use ployz_core::{Hint, IngressDoors, PartialResult, RpcErrorCode};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{Error, leaf_matches, with_client};
use crate::cli::{base, positional, switch};
use crate::ingress::{Doors, MachineDoors, cluster_doors_for, use_doors};
use crate::ui::{self, Cell, Gaps, Table};

/// The most a Cloudflare tunnel token holds on stdin. Cloudflare's run a few hundred bytes.
const TOKEN_LIMIT: usize = 8192;

pub(crate) fn show_command(command: Command) -> Command {
    base(
        command,
        "Show which Doors each Server opens for web traffic",
    )
    .long_about(
        "Show which Doors each Server with the ingress role opens: public (ports 80 and 443), \
             tunnel (a Cloudflare Tunnel), both during a switch, or closed.",
    )
    .after_help(concat!(
        "Examples:\n",
        "  ployz ingress\n",
        "  ployz ingress --json"
    ))
}

pub(crate) fn use_command(command: Command) -> Command {
    base(
        command,
        "Open the Public Door or the Tunnel Door on every Server taking web traffic",
    )
    .long_about(
        "Open the Public Door (ports 80 and 443) or the Tunnel Door (a Cloudflare Tunnel, no \
             open port) on every Server with the ingress role. Visitors keep getting answers \
             while the switch runs. The tunnel token leaves this device sealed to the Cluster.",
    )
    .after_help(concat!(
        "Examples:\n",
        "  ployz ingress use public\n",
        "  ployz ingress use tunnel\n",
        "  ployz ingress use tunnel --token-stdin < token.txt"
    ))
    .arg(
        positional("door", true)
            .value_parser(["public", "tunnel"])
            .help("public or tunnel"),
    )
    .arg(
        switch("token-stdin", None)
            .help("Read the Cloudflare tunnel token from stdin instead of asking for it"),
    )
}

pub(crate) fn show(root: &ArgMatches) -> Result<(), Error> {
    with_client(root, |client| {
        Box::pin(async move {
            let observed = crate::ingress::observe_doors(client).await?;
            let rows = observed
                .machines
                .iter()
                .map(|machine| MachineDoors {
                    machine: machine.id,
                    server: machine.name.to_string(),
                    doors: observed
                        .open
                        .get(&machine.id)
                        .map(names)
                        .unwrap_or_default(),
                })
                .collect::<Vec<_>>();
            let gaps = gaps(&observed, Vec::new());
            let table = table(&rows);
            ui::finish_fanout("machines", &rows, &gaps, || {
                if rows.is_empty() && gaps.is_complete() {
                    ui::note("No Server takes web traffic.");
                    ui::hint(&Hint::Next(
                        "ployz server set <server> --accepts-ingress=true".into(),
                    ));
                } else {
                    ui::rows(&table);
                }
            })
        })
    })
}

pub(crate) fn use_doors_on(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let door = leaf
        .get_one::<String>("door")
        .map(String::as_str)
        .unwrap_or_default();
    let doors = if door == "tunnel" {
        IngressDoors::tunnel()
    } else {
        IngressDoors::public()
    };
    let token = if leaf.get_flag("token-stdin") {
        Some(read_token(std::io::stdin())?)
    } else {
        None
    };
    with_client(root, |client| {
        Box::pin(async move {
            let observed = crate::ingress::observe_doors(client).await?;
            let token = match token {
                Some(token) => Some(token),
                None if doors.has(ployz_core::IngressDoor::Tunnel)
                    && observed.cluster.sealed_token.is_none() =>
                {
                    Some(ask_token()?)
                }
                None => None,
            };
            let doors = cluster_doors_for(&observed, doors, token.as_deref())?;
            let changed = use_doors(client, observed, doors, &CancellationToken::new()).await?;
            let gaps = gaps_of(&changed.machines, changed.failures);
            let table = table(&changed.machines);
            ui::finish_fanout("machines", &changed.machines, &gaps, || {
                if changed.machines.is_empty() && gaps.is_complete() {
                    ui::note("No Server takes web traffic.");
                    ui::hint(&Hint::Next(
                        "ployz server set <server> --accepts-ingress=true".into(),
                    ));
                } else {
                    ui::rows(&table);
                }
            })
        })
    })
}

fn names(doors: &IngressDoors) -> Vec<String> {
    doors.names().into_iter().map(str::to_owned).collect()
}

fn table(rows: &[MachineDoors]) -> Table {
    let mut table = Table::new(["SERVER", "DOORS"], "No servers answered.");
    for row in rows {
        let doors = if row.doors.is_empty() {
            Cell::status("closed", ui::Tone::Bad)
        } else {
            Cell::from(row.doors.join(", "))
        };
        table.row([Cell::from(&row.server), doors]);
    }
    table
}

/// The fan-out gaps of a listing: the ingress Servers that didn't list their
/// containers, and `failures`.
fn gaps(observed: &Doors, failures: Vec<ployz_core::MachineFailure<ployz_core::RpcError>>) -> Gaps {
    let mut listing: PartialResult<(), ployz_core::RpcError> = PartialResult {
        successes: Vec::new(),
        failures: observed.listed.failures.clone(),
        omissions: observed.listed.omissions.clone(),
    };
    let ingress = |machine_id: &ployz_core::MachineId| {
        observed
            .machines
            .iter()
            .any(|machine| machine.id == *machine_id)
    };
    listing
        .failures
        .retain(|failure| ingress(&failure.machine_id));
    listing.omissions.retain(ingress);
    listing.failures.extend(failures);
    Gaps::of(&listing).named(
        observed
            .machines
            .iter()
            .map(|machine| (machine.id, &machine.name)),
    )
}

fn gaps_of(
    rows: &[MachineDoors],
    failures: Vec<ployz_core::MachineFailure<ployz_core::RpcError>>,
) -> Gaps {
    let listing: PartialResult<(), ployz_core::RpcError> = PartialResult {
        successes: Vec::new(),
        failures,
        omissions: Vec::new(),
    };
    let names = rows
        .iter()
        .filter_map(|row| {
            ployz_core::MachineName::parse(&row.server)
                .ok()
                .map(|name| (row.machine, name))
        })
        .collect::<Vec<_>>();
    Gaps::of(&listing).named(names.iter().map(|(id, name)| (*id, name)))
}

fn read_token(stdin: impl Read) -> Result<String, Error> {
    let bytes = super::read_bounded(stdin, TOKEN_LIMIT)
        .map_err(|error| {
            Error::caused(
                RpcErrorCode::InvalidArgument,
                "Could not read the tunnel token from stdin",
                error,
            )
        })?
        .ok_or_else(|| {
            Error::usage(format!(
                "The token on stdin is over {TOKEN_LIMIT} bytes. A Cloudflare tunnel token is a \
                 few hundred characters."
            ))
        })?;
    let token = String::from_utf8(bytes)
        .map_err(|_| Error::usage("The tunnel token on stdin isn't UTF-8"))?;
    let token = token.trim();
    if token.is_empty() {
        return Err(Error::usage(
            "stdin holds no tunnel token. Copy it from the tunnel's Cloudflare page.",
        ));
    }
    check_shape(token)?;
    Ok(token.to_owned())
}

/// What a Cloudflare tunnel token holds once decoded. `cloudflared` refuses one that
/// doesn't, and a connector that can't start leaves every Server dark.
#[derive(Deserialize)]
struct CloudflareToken {
    /// The account tag.
    a: String,
    /// The tunnel id.
    t: String,
    /// The tunnel secret, base64.
    s: String,
}

/// Refuse a token `cloudflared` can't use, before any Server is asked to switch.
fn check_shape(token: &str) -> Result<(), Error> {
    let usable = STANDARD
        .decode(token)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CloudflareToken>(&bytes).ok())
        .is_some_and(|parsed| {
            !parsed.a.is_empty()
                && uuid::Uuid::parse_str(&parsed.t).is_ok()
                && STANDARD
                    .decode(&parsed.s)
                    .is_ok_and(|secret| !secret.is_empty())
        });
    if usable {
        Ok(())
    } else {
        Err(Error::usage(
            "Paste the whole token from the tunnel's install command. Tokens start with eyJ.",
        ))
    }
}

fn ask_token() -> Result<String, Error> {
    let token = ui::secret(
        "Cloudflare tunnel token",
        || {
            Error::coded(
                RpcErrorCode::InvalidArgument,
                "No terminal to ask for the tunnel token. Pipe it with --token-stdin.",
            )
        },
        "Cancelled. The Doors stay as they are.",
    )?;
    let token = token.trim();
    if token.is_empty() {
        return Err(Error::usage(
            "The tunnel token is empty. Copy it from the tunnel's Cloudflare page.",
        ));
    }
    check_shape(token)?;
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TUNNEL_ID: &str = "11111111-2222-4333-8444-555555555555";

    fn token(fields: serde_json::Value) -> String {
        STANDARD.encode(serde_json::to_vec(&fields).unwrap())
    }

    fn real_token() -> String {
        token(serde_json::json!({
            "a": "0123456789abcdef0123456789abcdef",
            "t": TUNNEL_ID,
            "s": STANDARD.encode([7u8; 32]),
        }))
    }

    #[test]
    fn a_token_on_stdin_is_trimmed_and_an_empty_or_oversized_one_refused() {
        let real = real_token();
        assert_eq!(read_token(format!("  {real}\n").as_bytes()).unwrap(), real);
        let empty = read_token("\n".as_bytes()).unwrap_err().report();
        assert_eq!(empty.code, RpcErrorCode::InvalidArgument);
        assert!(
            empty.message.contains("no tunnel token"),
            "{}",
            empty.message
        );
        let long = vec![b'a'; TOKEN_LIMIT + 1];
        let oversized = read_token(long.as_slice()).unwrap_err().report();
        assert!(
            oversized.message.contains("over 8192 bytes"),
            "{}",
            oversized.message
        );
    }

    #[test]
    fn a_token_cloudflared_cannot_use_is_refused_before_any_server_is_asked() {
        let secret = STANDARD.encode([7u8; 32]);
        let unusable = [
            ("not-a-real-token".to_owned(), "not base64"),
            (STANDARD.encode("not json"), "not JSON"),
            (token(serde_json::json!({ "a": "b" })), "fields missing"),
            (
                token(serde_json::json!({ "a": "", "t": TUNNEL_ID, "s": secret })),
                "empty account tag",
            ),
            (
                token(serde_json::json!({ "a": "b", "t": "tunnel", "s": secret })),
                "tunnel id not a UUID",
            ),
            (
                token(serde_json::json!({ "a": "b", "t": TUNNEL_ID, "s": "***" })),
                "secret not base64",
            ),
            (
                token(serde_json::json!({ "a": "b", "t": TUNNEL_ID, "s": "" })),
                "empty secret",
            ),
            (
                token(serde_json::json!({ "a": 1, "t": TUNNEL_ID, "s": secret })),
                "account tag not a string",
            ),
        ];
        for (candidate, why) in unusable {
            let error = read_token(candidate.as_bytes()).unwrap_err().report();
            assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{why}");
            assert!(
                error.message.contains("Tokens start with eyJ"),
                "{why}: {}",
                error.message
            );
        }
    }
}
