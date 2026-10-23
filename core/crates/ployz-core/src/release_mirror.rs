//! Where release assets download from: GitHub, else ployz.sh when GitHub can't be reached,
//! or the bases `PLOYZ_GITHUB_URL` and `PLOYZ_FALLBACK_URL` name; and where a Release
//! Channel resolves: ployz.sh, or the base `PLOYZ_CHANNEL_URL` names.

use std::{error::Error, future::Future, io};

/// The repository releases come from, unless `PLOYZ_GITHUB_URL` names another, as in
/// install.sh.
pub const RELEASE_REPOSITORY_URL: &str = "https://github.com/getployz/ployz";
/// Passes the repository's release assets through, for a Server with no route to GitHub
/// such as an IPv6-only one, unless `PLOYZ_FALLBACK_URL` names another, as in install.sh.
pub const RELEASE_FALLBACK_URL: &str = "https://ployz.sh";

/// The variables that name the release download bases, primary first. A process that
/// downloads on another's behalf, such as the upgrade worker, takes them from it.
pub const RELEASE_MIRROR_VARIABLES: [&str; 2] = ["PLOYZ_GITHUB_URL", "PLOYZ_FALLBACK_URL"];

/// Serves each Release Channel's pointer, unless `PLOYZ_CHANNEL_URL` names another, as in
/// install.sh.
pub const RELEASE_CHANNEL_URL: &str = "https://ployz.sh";
/// The variable that names the base Release Channel pointers resolve against.
pub const RELEASE_CHANNEL_VARIABLE: &str = "PLOYZ_CHANNEL_URL";

/// The base a Release Channel's pointer resolves against: `PLOYZ_CHANNEL_URL`, or ployz.sh
/// when it is unset or empty, as in install.sh, so a soak can upgrade through a test channel.
#[must_use]
pub fn release_channel_url() -> String {
    release_channel_url_from(|name| std::env::var(name).ok())
}

fn release_channel_url_from(var: impl Fn(&str) -> Option<String>) -> String {
    let url = var(RELEASE_CHANNEL_VARIABLE).filter(|url| !url.is_empty());
    url.as_deref()
        .unwrap_or(RELEASE_CHANNEL_URL)
        .trim_end_matches('/')
        .to_owned()
}

/// Release download bases in the order a download tries them: `PLOYZ_GITHUB_URL`'s, then
/// `PLOYZ_FALLBACK_URL`'s. An unset or empty variable is its default, as in install.sh, so
/// a soak can point upgrades and bootstraps at a staged release.
#[must_use]
pub fn release_mirrors() -> [String; 2] {
    release_mirrors_from(|name| std::env::var(name).ok())
}

/// The [`RELEASE_MIRROR_VARIABLES`] and [`RELEASE_CHANNEL_VARIABLE`] this process has set,
/// for a process that resolves or downloads a release on its behalf: the upgrade worker, or
/// the installer a remote `server add` or a check-in runs.
#[must_use]
pub fn release_mirror_overrides() -> Vec<(&'static str, String)> {
    RELEASE_MIRROR_VARIABLES
        .into_iter()
        .chain([RELEASE_CHANNEL_VARIABLE])
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|url| !url.is_empty())
                .map(|url| (name, url))
        })
        .collect()
}

fn release_mirrors_from(var: impl Fn(&str) -> Option<String>) -> [String; 2] {
    let base = |name, default| {
        let url = var(name).filter(|url| !url.is_empty());
        let url = url.as_deref().unwrap_or(default).trim_end_matches('/');
        format!("{url}/releases/download")
    };
    let [primary, fallback] = RELEASE_MIRROR_VARIABLES;
    [
        base(primary, RELEASE_REPOSITORY_URL),
        base(fallback, RELEASE_FALLBACK_URL),
    ]
}

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

    #[test]
    fn the_release_channel_follows_the_install_sh_override() {
        assert_eq!(release_channel_url_from(|_| None), "https://ployz.sh");
        assert_eq!(
            release_channel_url_from(|name| {
                (name == "PLOYZ_CHANNEL_URL").then(|| "https://channels.example.test/".to_owned())
            }),
            "https://channels.example.test"
        );
        assert_eq!(
            release_channel_url_from(|_| Some(String::new())),
            "https://ployz.sh"
        );
    }

    #[test]
    fn release_mirrors_follow_install_sh_overrides() {
        let mirrors = |vars: &[(&str, &str)]| {
            release_mirrors_from(|name| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            })
        };
        assert_eq!(
            mirrors(&[]),
            [
                "https://github.com/getployz/ployz/releases/download",
                "https://ployz.sh/releases/download",
            ]
        );
        assert_eq!(
            mirrors(&[
                ("PLOYZ_GITHUB_URL", "https://example.test/staging/ployz/"),
                ("PLOYZ_FALLBACK_URL", "https://fallback.example.test"),
            ]),
            [
                "https://example.test/staging/ployz/releases/download",
                "https://fallback.example.test/releases/download",
            ]
        );
        // As in install.sh's `${PLOYZ_GITHUB_URL:-…}`, an empty value is the default.
        assert_eq!(
            mirrors(&[("PLOYZ_GITHUB_URL", ""), ("PLOYZ_FALLBACK_URL", "")]),
            mirrors(&[])
        );
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
