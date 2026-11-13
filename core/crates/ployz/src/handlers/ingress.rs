//! `ployz ingress`: which Doors each Server with the ingress role holds open for web
//! traffic, and `ployz ingress use` to open the Public Door or the Tunnel Door instead.

use std::io::Read;

use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{ArgMatches, Command};
use ployz_core::{Hint, IngressDoors, RpcErrorCode};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{Error, leaf_matches, with_client};
use crate::cli::{base, positional, switch};
use crate::ingress::{DoorMachine, Doors, MachineDoors, checked_doors, use_doors};
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
            let gaps = gaps(&observed);
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
    let config = super::config_path(root)?;
    with_client(root, |client| {
        Box::pin(async move {
            let observed = crate::ingress::observe_doors(client).await?;
            let doors = checked_doors(
                client,
                &observed,
                doors,
                token,
                async |client| super::device::seal_to(&config, client).await,
                ask_token,
            )
            .await?;
            let addressed = observed.machines.clone();
            let changed = use_doors(client, observed, doors, &CancellationToken::new()).await?;
            let gaps = gaps_of(&addressed, changed.failures);
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
/// containers.
fn gaps(observed: &Doors) -> Gaps {
    let ingress = |machine_id: &ployz_core::MachineId| {
        observed
            .machines
            .iter()
            .any(|machine| machine.id == *machine_id)
    };
    Gaps::from_parts(
        observed
            .listed
            .failures
            .iter()
            .filter(|failure| ingress(&failure.machine_id))
            .cloned()
            .collect(),
        observed
            .listed
            .omissions
            .iter()
            .filter(|machine_id| ingress(machine_id))
            .cloned()
            .collect(),
    )
    .named(
        observed
            .machines
            .iter()
            .map(|machine| (machine.id, &machine.name)),
    )
}

/// The gaps in a change, naming every ingress Server the command addressed: one whose
/// answer was a failure has no row, and its warning still carries its name.
fn gaps_of(
    addressed: &[DoorMachine],
    failures: Vec<ployz_core::MachineFailure<ployz_core::RpcError>>,
) -> Gaps {
    Gaps::from_parts(failures, Vec::new())
        .named(addressed.iter().map(|machine| (machine.id, &machine.name)))
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

    fn machine_id(seed: char) -> ployz_core::MachineId {
        ployz_core::MachineId::parse(seed.to_string().repeat(32)).unwrap()
    }

    fn door_machine(seed: char) -> crate::ingress::DoorMachine {
        crate::ingress::DoorMachine {
            id: machine_id(seed),
            name: ployz_core::MachineName::parse(format!("machine-{seed}")).unwrap(),
            supported: true,
            tunnels: true,
        }
    }

    fn silent(seed: char) -> ployz_core::MachineFailure<ployz_core::RpcError> {
        ployz_core::MachineFailure {
            machine_id: machine_id(seed),
            error: ployz_core::RpcError::unavailable("no answer".to_owned()),
        }
    }

    fn gap_ids(gaps: &Gaps) -> (Vec<ployz_core::MachineId>, Vec<ployz_core::MachineId>) {
        (
            gaps.failures
                .iter()
                .map(|failure| failure.machine_id)
                .collect(),
            gaps.omitted.clone(),
        )
    }

    /// `ployz ingress` warns about the Servers with the ingress role that didn't list their
    /// containers, never about one without the role, and names each by its Server name.
    #[test]
    fn the_listing_gaps_are_those_of_the_ingress_servers() {
        let observed = Doors {
            machines: vec![door_machine('a'), door_machine('b')],
            proxy: ployz_core::ServiceId::random(),
            cluster: ployz_core::ClusterDoors {
                doors: IngressDoors::public(),
                sealed_token: None,
            },
            declared: None,
            declaration: crate::ingress::Declaration::Entry,
            open: std::collections::BTreeMap::new(),
            listed: ployz_core::PartialResult {
                successes: Vec::new(),
                failures: vec![silent('a'), silent('c')],
                omissions: vec![machine_id('b'), machine_id('c')],
            },
            sealing: None,
            recipient: None,
        };
        let gaps = gaps(&observed);
        assert_eq!(
            gap_ids(&gaps),
            (vec![machine_id('a')], vec![machine_id('b')])
        );
        assert!(
            gaps.stopped_answering(&machine_id('a'))
                .starts_with("machine-a ")
        );
        assert!(
            gaps.stopped_answering(&machine_id('b'))
                .starts_with("machine-b ")
        );
    }

    /// A change names every ingress Server it addressed by its Server name in a failure's
    /// warning, one that answered with no row included; a Machine it never addressed is
    /// named by its id.
    #[test]
    fn a_door_change_names_the_servers_it_addressed() {
        let addressed = [door_machine('a'), door_machine('b')];
        let gaps = gaps_of(&addressed, vec![silent('a'), silent('b'), silent('d')]);
        assert_eq!(
            gap_ids(&gaps),
            (
                vec![machine_id('a'), machine_id('b'), machine_id('d')],
                Vec::new()
            )
        );
        assert!(
            gaps.stopped_answering(&machine_id('a'))
                .starts_with("machine-a ")
        );
        assert!(
            gaps.stopped_answering(&machine_id('b'))
                .starts_with("machine-b ")
        );
        assert!(
            gaps.stopped_answering(&machine_id('d'))
                .starts_with(&"d".repeat(32))
        );
    }
}
