//! The Cloud a command talks to, written once: a bare host is https, a URL is kept as given,
//! and nothing else about it is spelled anywhere else.

use crate::env;

/// The Cloud a sign-in goes to when none is named or stored.
pub(crate) const DEFAULT_CLOUD: &str = "ployz.dev";

/// `https://` and a bare host, or the URL as given, with no trailing slash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudOrigin(String);

/// A Cloud written with a scheme that is neither `http` nor `https`.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct NotWebUrl {
    pub(crate) given: String,
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
            && !["http", "https"].contains(&scheme.to_ascii_lowercase().as_str())
        {
            return Err(NotWebUrl {
                given: text.to_owned(),
            });
        }
        Ok(Self::new(text))
    }

    pub(crate) fn default_cloud() -> Self {
        Self::new(DEFAULT_CLOUD)
    }

    /// What `PLOYZ_CLOUD_URL` says, as written: the only place the variable is read.
    pub(crate) fn named_by_env() -> Option<String> {
        std::env::var(env::CLOUD_URL).ok()
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
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
            CloudOrigin::checked("ftp://ployz.dev"),
            Err(NotWebUrl {
                given: "ftp://ployz.dev".into()
            })
        );
    }
}
