//! Serve a fake fleet for the agent eval.
//!
//! `ployz-fake-fleet --machines <fleet.json> --sockets <dir> --ledger <ledger.jsonl>
//! [--scenario <json>]` prints `{"ready":true}` once every online Machine's socket is
//! bound, then reads one control line per request on stdin: `{"online":"<name>"}` or
//! `{"offline":"<name>"}`, each answered `{"ok":true}` or `{"ok":false,"error":"..."}`.
//! End of stdin stops the fleet.

use std::{fs::File, io, path::PathBuf};

use ployz_core::MachineName;
use ployz_testkit::fake_fleet::{FakeFleet, FakeFleetError, FleetFile, Scenario};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const USAGE: &str = "usage: ployz-fake-fleet --machines <fleet.json> --sockets <dir> --ledger <ledger.jsonl> [--scenario <json>]";

#[derive(Debug, Error)]
enum Error {
    #[error("{USAGE}")]
    Usage,
    #[error("{path} could not be opened")]
    File {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{what} is not valid JSON for it")]
    Parse {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Fleet(#[from] FakeFleetError),
    #[error("stdio failed")]
    Stdio(#[source] io::Error),
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
enum Control {
    Online(MachineName),
    Offline(MachineName),
}

struct Args {
    machines: PathBuf,
    sockets: PathBuf,
    ledger: PathBuf,
    scenario: Scenario,
}

fn parse_args() -> Result<Args, Error> {
    let mut machines = None;
    let mut sockets = None;
    let mut ledger = None;
    let mut scenario = Scenario::default();
    let mut args = std::env::args_os().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(Error::Usage)?;
        match flag.to_str() {
            Some("--machines") => machines = Some(PathBuf::from(value)),
            Some("--sockets") => sockets = Some(PathBuf::from(value)),
            Some("--ledger") => ledger = Some(PathBuf::from(value)),
            Some("--scenario") => {
                let text = value.into_string().map_err(|_| Error::Usage)?;
                scenario = serde_json::from_str(&text).map_err(|source| Error::Parse {
                    what: "--scenario",
                    source,
                })?;
            }
            _ => return Err(Error::Usage),
        }
    }
    Ok(Args {
        machines: machines.ok_or(Error::Usage)?,
        sockets: sockets.ok_or(Error::Usage)?,
        ledger: ledger.ok_or(Error::Usage)?,
        scenario,
    })
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = parse_args()?;
    let text = std::fs::read_to_string(&args.machines).map_err(|source| Error::File {
        path: args.machines.clone(),
        source,
    })?;
    let fleet: FleetFile = serde_json::from_str(&text).map_err(|source| Error::Parse {
        what: "--machines",
        source,
    })?;
    let ledger = File::create(&args.ledger).map_err(|source| Error::File {
        path: args.ledger.clone(),
        source,
    })?;
    let fleet = FakeFleet::start(fleet, args.scenario, args.sockets, Some(ledger))?;

    let mut stdout = tokio::io::stdout();
    reply(&mut stdout, &json!({"ready": true})).await?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await.map_err(Error::Stdio)? {
        let outcome = serde_json::from_str::<Control>(&line)
            .map_err(|error| error.to_string())
            .and_then(|control| {
                match control {
                    Control::Online(name) => fleet.online(&name),
                    Control::Offline(name) => fleet.offline(&name),
                }
                .map_err(|error| error.to_string())
            });
        let answer = match outcome {
            Ok(()) => json!({"ok": true}),
            Err(error) => json!({"ok": false, "error": error}),
        };
        reply(&mut stdout, &answer).await?;
    }
    Ok(())
}

async fn reply(stdout: &mut tokio::io::Stdout, value: &serde_json::Value) -> Result<(), Error> {
    let mut line = value.to_string();
    line.push('\n');
    stdout
        .write_all(line.as_bytes())
        .await
        .map_err(Error::Stdio)?;
    stdout.flush().await.map_err(Error::Stdio)
}
