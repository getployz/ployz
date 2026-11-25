//! The Cloud a command talks to, written once: a bare host is https, a URL is kept as given,
//! and nothing else about it is spelled anywhere else.

use crate::cli::Shown;
use crate::env;

/// The schemes a Cloud is reached by.
const WEB: &[&str] = &["http", "https"];

/// The Cloud a sign-in goes to when none is named or stored.
pub(crate) const DEFAULT_CLOUD: &str = "ployz.dev";

/// `https://` and a bare host, or the URL as given, with no trailing slash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudOrigin(String);

/// A Cloud written with a scheme that is neither `http` nor `https`.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct NotWebUrl {
    /// What was given, as an error names it: its scheme as `SCHEME`.
    pub(crate) given: Shown,
}

impl CloudOrigin {
    pub(crate) fn new(text: &str) -> Self {
        let host = text.trim().trim_end_matches('/');
        Self(if host.contains("://") {
            host.to_owned()
        } else {
            format!("https://{host}")
        })
    }

    /// [`Self::new`], refusing a scheme the Cloud is not reached by.
    pub(crate) fn checked(text: &str) -> Result<Self, NotWebUrl> {
        if let Some((scheme, _)) = text.split_once("://")
            && !WEB.contains(&scheme.to_ascii_lowercase().as_str())
        {
            return Err(NotWebUrl {
                given: Self(text.to_owned()).shown(),
            });
        }
        Ok(Self::new(text))
    }

    pub(crate) fn default_cloud() -> Self {
        Self::new(DEFAULT_CLOUD)
    }

    /// Whether `cloud`, written exactly as stored or typed, is the default Cloud's origin.
    pub(crate) fn is_default(cloud: &str) -> bool {
        cloud == Self::default_cloud().as_str()
    }

    /// What `PLOYZ_CLOUD_URL` says, as written: the only place the variable is read.
    pub(crate) fn named_by_env() -> Option<String> {
        std::env::var(env::CLOUD_URL).ok()
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// As an error names it: its scheme, host and port, with any user info, path, query or
    /// fragment marked in place (`https://USER@cloud.example/PATH`), and `CLOUD_URL` when it
    /// isn't a URL at all.
    pub(crate) fn shown(&self) -> Shown {
        url::Url::parse(&self.0).map_or_else(
            |_| Shown::placeholder("CLOUD_URL"),
            |url| Shown::url_redacted(&url, WEB),
        )
    }

    /// As a command shows it again: whole when it is a scheme, a host and a port alone,
    /// else none, so the command names `CLOUD_URL` rather than reaching another Cloud.
    pub(crate) fn whole(&self) -> Option<Shown> {
        url::Url::parse(&self.0)
            .ok()
            .and_then(|url| Shown::url(&url, WEB))
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }

    /// The token travels only as a bearer header, so it never reaches a URL that proxies and
    /// access logs record.
    pub(crate) fn enroll_url(&self) -> String {
        format!("{}/api/enroll", self.0)
    }

    /// Final enrollment completion: `POST /api/enroll/callback`.
    pub(crate) fn callback_url(&self) -> String {
        format!("{}/callback", self.enroll_url())
    }

    /// Best-effort setup report: `POST /api/enroll/report`.
    pub(crate) fn report_url(&self) -> String {
        format!("{}/report", self.enroll_url())
    }

    pub(crate) fn checkin_url(&self) -> String {
        format!("{}/api/checkin", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_without_scheme_is_https_under_ployz_dev() {
        let url = |text: &str| CloudOrigin::new(text);
        assert_eq!(
            url("ployz.dev").enroll_url(),
            "https://ployz.dev/api/enroll"
        );
        assert_eq!(
            url("http://127.0.0.1:9/").enroll_url(),
            "http://127.0.0.1:9/api/enroll"
        );
        assert_eq!(
            url("ployz.dev").callback_url(),
            "https://ployz.dev/api/enroll/callback"
        );
        assert_eq!(
            url("ployz.dev").report_url(),
            "https://ployz.dev/api/enroll/report"
        );
        assert_eq!(
            url(" ployz.dev ").checkin_url(),
            "https://ployz.dev/api/checkin"
        );
        assert_eq!(CloudOrigin::default_cloud(), url("https://ployz.dev"));
    }

    #[test]
    fn only_http_and_https_name_a_cloud() {
        assert_eq!(
            CloudOrigin::checked("HTTP://localhost:3000")
                .unwrap()
                .as_str(),
            "HTTP://localhost:3000"
        );
        assert_eq!(
            CloudOrigin::checked("ployz.dev").unwrap().as_str(),
            "https://ployz.dev"
        );
        assert_eq!(
            CloudOrigin::checked("ftp://EXAMPLE@ployz.dev/EXAMPLE")
                .map_err(|refused| refused.given),
            Err(crate::cli::Shown::placeholder(
                "SCHEME://USER@ployz.dev/PATH"
            ))
        );
    }
}
