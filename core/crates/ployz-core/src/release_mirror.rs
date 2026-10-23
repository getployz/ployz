//! Where release assets download from: GitHub, else ployz.sh when GitHub can't be reached.

use std::{error::Error, future::Future, io};

/// Release download bases in the order a download tries them. ployz.sh passes GitHub's
/// release assets through, for a Server with no route to GitHub such as an IPv6-only one.
pub const RELEASE_MIRRORS: [&str; 2] = [
    "https://github.com/getployz/ployz/releases/download",
    "https://ployz.sh/releases/download",
];

/// Fetch `asset`, a path under a release download base such as `v0.3.4/checksums.txt`, from
/// the primary mirror, or from the fallback when [`release_mirror_unreachable`] says the
/// request never reached the primary. Any other failure is returned as is.
///
/// # Errors
///
/// Returns the failure of the last mirror tried. Its URL names that mirror.
pub async fn from_release_mirrors<T, F, Fut>(
    [primary, fallback]: [&str; 2],
    asset: &str,
    mut fetch: F,
) -> Result<T, reqwest::Error>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<T, reqwest::Error>>,
{
    match fetch(format!("{primary}/{asset}")).await {
        Err(error) if release_mirror_unreachable(&error) => {
            fetch(format!("{fallback}/{asset}")).await
        }
        result => result,
    }
}

/// Whether a request failed while connecting, without a timeout, and [`mirror_unreachable`]
/// says nothing answered. GitHub serves each asset through a redirect to its asset host, and
/// reqwest follows it, so that host counts as the mirror: it is the one that must answer.
///
/// hyper-util reports only the first error when a name has several addresses of one family,
/// so a refusal from one can stand for another's timeout. GitHub's asset hosts have several,
/// so that mix can fall back; whatever ployz.sh serves is still checked against the signed
/// checksums.
#[must_use]
pub fn release_mirror_unreachable(error: &reqwest::Error) -> bool {
    error.is_connect() && !error.is_timeout() && mirror_unreachable(error)
}

/// Whether a failed request never reached the mirror: its name didn't resolve to an address
/// this host can use, or no route or listener answered. A timeout, a TLS failure or an HTTP
/// status did reach something, so it never counts; trying elsewhere would hide it.
#[must_use]
pub fn mirror_unreachable(error: &(dyn Error + 'static)) -> bool {
    let mut link = Some(error);
    while let Some(error) = link {
        // hyper-util's connector names a resolver failure so; its type is private.
        if error.to_string() == "dns error" {
            return true;
        }
        if let Some(io) = error.downcast_ref::<io::Error>() {
            // hyper-util's connector found no address to try, not a socket's ENOTCONN.
            let no_address =
                io.kind() == io::ErrorKind::NotConnected && io.raw_os_error().is_none();
            return no_address
                || matches!(
                    io.kind(),
                    io::ErrorKind::NetworkUnreachable
                        | io::ErrorKind::HostUnreachable
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::AddrNotAvailable
                );
        }
        link = error.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::*;

    #[derive(Debug)]
    struct Wrapped(&'static str, Option<io::Error>);

    impl fmt::Display for Wrapped {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.0)
        }
    }

    impl Error for Wrapped {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.1.as_ref().map(|error| error as _)
        }
    }

    fn errno(code: i32) -> Wrapped {
        Wrapped(
            "tcp connect error",
            Some(io::Error::from_raw_os_error(code)),
        )
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_a_mirror_that_was_never_reached_reads_unreachable() {
        // Linux errnos: ENETUNREACH, EHOSTUNREACH, ECONNREFUSED, EADDRNOTAVAIL.
        for code in [101, 113, 111, 99] {
            assert!(mirror_unreachable(&errno(code)), "errno {code}");
        }
        assert!(mirror_unreachable(&Wrapped("dns error", None)));
        let no_address = io::Error::new(io::ErrorKind::NotConnected, "Network unreachable");
        assert!(mirror_unreachable(&Wrapped(
            "tcp connect error",
            Some(no_address)
        )));

        // ETIMEDOUT, ECONNRESET and a socket's own ENOTCONN reached something.
        for code in [110, 104, 107] {
            assert!(!mirror_unreachable(&errno(code)), "errno {code}");
        }
        let tls = io::Error::new(io::ErrorKind::InvalidData, "invalid peer certificate");
        assert!(!mirror_unreachable(&Wrapped("client error", Some(tls))));
        assert!(!mirror_unreachable(&Wrapped("operation timed out", None)));
    }
}
