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

/// A Cloud URL Ployz won't store or reach.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Refused {
    /// What was given, as an error names it ([`CloudOrigin::shown`]).
    pub(crate) given: Shown,
    pub(crate) why: Refusal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Refusal {
    /// A scheme other than `http` or `https`.
    Scheme,
    /// No host a URL can name, such as a placeholder.
    NoHost,
    /// A user name or password: Ployz signs in through the approval page, and would store
    /// and print the URL whole.
    UserInfo,
    /// A query or fragment, which Ployz's own paths can't follow.
    QueryOrFragment,
}

impl Refused {
    /// The refusal of the Cloud `named` (`--cloud-url`, a variable, or the stored one) gave.
    pub(crate) fn sentence(&self, named: &str) -> String {
        let given = &self.given;
        match self.why {
            Refusal::Scheme | Refusal::NoHost => format!(
                "{named} takes a host such as ployz.dev, or an http:// or https:// URL, not {given}"
            ),
            Refusal::UserInfo => format!(
                "{named} can't hold a user name or password, which Ployz would store and print: {given}. Name the Cloud alone; you sign in on its approval page"
            ),
            Refusal::QueryOrFragment => format!(
                "{named} can't hold a query or fragment, which Ployz's own paths can't follow: {given}. Name the Cloud alone"
            ),
        }
    }
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

    /// [`Self::new`], refusing a URL Ployz would not store as given: a scheme the Cloud is
    /// not reached by, no host, user info, a query or a fragment.
    pub(crate) fn checked(text: &str) -> Result<Self, Refused> {
        let origin = Self::new(text);
        let refused = |why| {
            Err(Refused {
                given: origin.shown(),
                why,
            })
        };
        if let Some((scheme, _)) = text.trim().split_once("://")
            && !WEB.contains(&scheme.to_ascii_lowercase().as_str())
        {
            return refused(Refusal::Scheme);
        }
        let Some(url) = url::Url::parse(&origin.0)
            .ok()
            .filter(|url| url.host_str().is_some_and(|host| !host.is_empty()))
        else {
            return refused(Refusal::NoHost);
        };
        if !url.username().is_empty() || url.password().is_some() {
            return refused(Refusal::UserInfo);
        }
        if url.query().is_some() || url.fragment().is_some() {
            return refused(Refusal::QueryOrFragment);
        }
        Ok(origin)
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
    /// fragment marked in place (`https://<USER>@cloud.example/<PATH>`), and `CLOUD_URL` when it
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
                .map_err(|refused| (refused.given.to_string(), refused.why)),
            Err((
                "<SCHEME>://<USER>@ployz.dev/<PATH>".to_owned(),
                Refusal::Scheme
            ))
        );
    }

    /// A Cloud URL is stored and printed whole on success, so one that holds a credential,
    /// or anything Ployz's own paths can't follow, is refused before it is stored.
    #[test]
    fn a_cloud_with_user_info_a_query_a_fragment_or_no_host_is_refused() {
        for (typed, why, given) in [
            (
                "https://EXAMPLE:EXAMPLE@cloud.example",
                Refusal::UserInfo,
                "https://<USER>:<PASSWORD>@cloud.example",
            ),
            (
                "EXAMPLE@cloud.example",
                Refusal::UserInfo,
                "https://<USER>@cloud.example",
            ),
            (
                "https://cloud.example/?token=EXAMPLE",
                Refusal::QueryOrFragment,
                "https://cloud.example?<QUERY>",
            ),
            (
                "https://cloud.example#EXAMPLE",
                Refusal::QueryOrFragment,
                "https://cloud.example#<FRAGMENT>",
            ),
            ("<CLOUD_URL>", Refusal::NoHost, "<CLOUD_URL>"),
        ] {
            assert_eq!(
                CloudOrigin::checked(typed)
                    .map_err(|refused| (refused.given.to_string(), refused.why)),
                Err((given.to_owned(), why)),
                "{typed}"
            );
        }
        for typed in [
            "ployz.dev",
            "http://127.0.0.1:9",
            "https://example.test/ployz",
        ] {
            assert!(CloudOrigin::checked(typed).is_ok(), "{typed}");
        }
    }
}
