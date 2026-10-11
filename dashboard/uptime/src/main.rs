use std::sync::Arc;

use ployz_uptime::config::{ConfigError, Mode};
use ployz_uptime::run::{self, Shutdown};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "Usage: ployz-uptime <control|agent|all>";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args: Vec<String> = std::env::args().collect();
    let mode = match Mode::from(&args, &|key| std::env::var(key).ok()) {
        Ok(mode) => mode,
        Err(ConfigError::Usage) => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("The runtime could not start: {e}");
            std::process::exit(1);
        }
    };
    let outcome = runtime.block_on(async {
        let (trigger, shutdown) = Shutdown::new();
        tokio::spawn(async move {
            let mut term =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(term) => term,
                    Err(e) => {
                        tracing::warn!(error = %e, "SIGTERM not listened for");
                        drop(tokio::signal::ctrl_c().await);
                        trigger.fire();
                        return;
                    }
                };
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            tracing::info!("stopping");
            trigger.fire();
        });
        run::main(mode, crypto, shutdown).await
    });
    if let Err(e) = outcome {
        tracing::error!("{}", e.0);
        std::process::exit(1);
    }
}
