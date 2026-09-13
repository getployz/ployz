//! Tracing subscriber for `journalctl -u ployz`.

use std::{
    env::{self, VarError},
    io,
};

use thiserror::Error;
use tracing_subscriber::{EnvFilter, fmt::MakeWriter};

/// Environment variable that raises verbosity without editing the unit file.
pub const ENV: &str = "PLOYZ_LOG";

const DEFAULT: &str = "info";

/// Invalid `PLOYZ_LOG` / `--log-level` filter.
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Parse(#[from] tracing_subscriber::filter::ParseError),
    #[error("{ENV} is not valid Unicode")]
    NotUnicode,
}

/// Install the journal subscriber. `--log-level` overrides [`ENV`]; default is `info`.
///
/// # Errors
///
/// If the CLI or `PLOYZ_LOG` filter cannot be parsed.
pub fn init(cli: Option<&str>) -> Result<(), Error> {
    let filter = match env::var(ENV) {
        Ok(value) => directive(cli, Some(&value))?,
        Err(VarError::NotPresent) => directive(cli, None)?,
        Err(VarError::NotUnicode(_)) => return Err(Error::NotUnicode),
    };
    tracing::subscriber::set_global_default(subscriber(filter, io::stderr))
        .expect("tracing subscriber already installed");
    Ok(())
}

fn directive(cli: Option<&str>, env: Option<&str>) -> Result<EnvFilter, Error> {
    Ok(EnvFilter::try_new(cli.or(env).unwrap_or(DEFAULT))?)
}

fn subscriber<W>(filter: EnvFilter, writer: W) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .compact()
        .finish()
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    use tracing_subscriber::fmt::MakeWriter;

    use super::{directive, subscriber};

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl Capture {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("capture")).into_owned()
        }
    }

    fn emit(cli: Option<&str>, env: Option<&str>, body: impl FnOnce()) -> String {
        let capture = Capture::default();
        let subscriber = subscriber(directive(cli, env).unwrap(), capture.clone());
        tracing::subscriber::with_default(subscriber, body);
        capture.text()
    }

    #[test]
    fn default_info_emits_info_and_hides_debug() {
        let text = emit(None, None, || {
            tracing::info!("visible");
            tracing::debug!("hidden");
        });
        assert!(text.contains("visible"));
        assert!(!text.contains("hidden"));
    }

    #[test]
    fn cli_overrides_env_and_invalid_filter_is_rejected() {
        let text = emit(Some("debug"), Some("error"), || {
            tracing::debug!(socket = "/run/ployz/ployz.sock", "listening");
        });
        assert!(text.contains("listening"));
        assert!(directive(Some("foo=notalevel"), None).is_err());
    }
}
