//! One-off operator scrub for #1433: rewrites every Deploy Preview stored in
//! `config_deployment.run` through `redacted_runtime_preview`, so none keeps
//! pre-deploy hook environment values or Config content recorded before #1418.
//!
//! ```text
//! PLOYZ_STORE_URL='postgres://…' cargo run -p ployz-store --example scrub_deploy_previews -- --dry-run
//! PLOYZ_STORE_URL='postgres://…' cargo run -p ployz-store --example scrub_deploy_previews
//! ```
//!
//! It prints counts only, never a value. A preview already in its redacted form
//! is left byte for byte, so a rerun changes nothing. Each write applies only if
//! the row still holds what was read, so a Deployment recording evidence at the
//! same moment keeps its write and the next run picks the row up.

use std::process::ExitCode;
use std::sync::Arc;

use ployz_core::DeployPreview;
use ployz_core::config::redacted_runtime_preview;
use postgres::Client;
use serde_json::Value;
use tokio_postgres_rustls::MakeRustlsConnect;

enum Stored {
    Clean,
    Scrubbed(String),
    Unreadable,
}

fn scrub(run: &str) -> Stored {
    let Ok(mut run) = serde_json::from_str::<Value>(run) else {
        return Stored::Unreadable;
    };
    let Some(preview) = run.get_mut("preview").filter(|preview| !preview.is_null()) else {
        return Stored::Clean;
    };
    let Ok(decoded) = serde_json::from_value::<DeployPreview>(preview.clone()) else {
        return Stored::Unreadable;
    };
    let Ok(redacted) = serde_json::to_value(redacted_runtime_preview(decoded)) else {
        return Stored::Unreadable;
    };
    if *preview == redacted {
        return Stored::Clean;
    }
    *preview = redacted;
    Stored::Scrubbed(run.to_string())
}

fn connect(url: &str) -> Result<Client, String> {
    let config: postgres::Config = url
        .parse()
        .map_err(|_| "PLOYZ_STORE_URL is not a postgres:// URL".to_owned())?;
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| format!("TLS setup failed: {error}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    config
        .connect(MakeRustlsConnect::new(tls))
        .map_err(|error| format!("could not connect: {error}"))
}

fn run(dry_run: bool) -> Result<bool, String> {
    let url = std::env::var("PLOYZ_STORE_URL").map_err(|_| "set PLOYZ_STORE_URL".to_owned())?;
    let mut db = connect(&url)?;
    let rows = db
        .query("SELECT id, run FROM config_deployment ORDER BY id", &[])
        .map_err(|error| format!("could not read Deployments: {error}"))?;
    let (mut changed, mut unreadable, mut moved_on) = (0usize, 0usize, 0usize);
    for row in &rows {
        let id: String = row.get(0);
        let stored: String = row.get(1);
        match scrub(&stored) {
            Stored::Clean => {}
            Stored::Unreadable => unreadable += 1,
            Stored::Scrubbed(_) if dry_run => changed += 1,
            Stored::Scrubbed(scrubbed) => {
                let written = db
                    .execute(
                        "UPDATE config_deployment SET run = $1 WHERE id = $2 AND run = $3",
                        &[&scrubbed, &id, &stored],
                    )
                    .map_err(|error| format!("could not write Deployment {id}: {error}"))?;
                match written {
                    0 => moved_on += 1,
                    _ => changed += 1,
                }
            }
        }
    }
    match dry_run {
        true => println!("would scrub {changed} Deployment previews"),
        false => println!("scrubbed {changed} Deployment previews"),
    }
    if moved_on > 0 {
        println!("{moved_on} changed while this ran; run it again");
    }
    if unreadable > 0 {
        println!("{unreadable} runs could not be read and were left as stored");
    }
    Ok(moved_on == 0 && unreadable == 0)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let dry_run = match (args.next().as_deref(), args.next()) {
        (None, None) => false,
        (Some("--dry-run"), None) => true,
        _ => {
            eprintln!("usage: scrub_deploy_previews [--dry-run]");
            return ExitCode::from(2);
        }
    };
    match run(dry_run) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
